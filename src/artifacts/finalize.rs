//! Atomic finalization of artifacts (C-02 "Output paths", C-04 "Downloads" step 3–4).
//!
//! Content is first written to a temp file in the target directory named
//! `.<name>.iris-part-<random>` (created with `O_EXCL`, so an existing file or
//! symlink at that name is never followed), then moved into place:
//!
//! * [`FinalizeMode::NoClobber`] — no-clobber rename; if the target exists with
//!   identical SHA-256 the save is a successful no-op (`already_downloaded`),
//!   otherwise `output_exists`. Used for downloads, which can simply be re-run.
//! * [`FinalizeMode::Overwrite`] — atomic rename over the target (`--overwrite`).
//! * [`FinalizeMode::RenameOnConflict`] — like `NoClobber`, but a different file
//!   that appeared at the target since preflight makes the output go to
//!   `<stem>.<n>.<ext>` with warning `output_renamed`. Used for paid synchronous
//!   outputs, which must never be discarded.
//!
//! When the content needs another extension than the requested path
//! (`output_extension_adjusted`, e.g. a JPEG for `photo.png`), the output goes to a
//! path the user never named and preflight never checked. `--overwrite` does not
//! extend to it: `Overwrite` becomes `RenameOnConflict` for generated outputs and
//! `NoClobber` for downloads there, so an unrelated `photo.jpg` is never replaced.
//!
//! The temp file is removed on every failure path. Content is validated (decode /
//! ISO-BMFF walk) before it is given its final name.
//!
//! All I/O on a temp file goes through the handle opened when it was created:
//! writing ([`PartFile::file_mut`], [`PartFile::reset`]), validating, hashing, and
//! syncing. The file is never reopened by name, because in a shared, writable
//! directory another user could swap that name for a symlink, and a reopen (with
//! `O_TRUNC`, say) would follow it; that would undo the `O_EXCL` creation C-04
//! requires. Only the final rename uses the name.

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

use crate::domain::{Artifact, Warning};
use crate::error::{ErrorCode, IrisError};

use super::media::{self, MediaInfo};
use super::paths;

/// Maximum `<stem>.<n>.<ext>` suffix tried by [`FinalizeMode::RenameOnConflict`].
const MAX_RENAME_ATTEMPTS: u32 = 9999;
/// Longest part of the target name reused in a temp file name.
const MAX_NAME_IN_PART: usize = 120;

/// What to do when the target path already exists at finalize time. The mode
/// applies to the requested path; see the module docs for extension-adjusted paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinalizeMode {
    /// Refuse (`output_exists`) unless the existing file has identical content.
    NoClobber,
    /// Atomically replace the existing file.
    Overwrite,
    /// Keep the existing file and save to `<stem>.<n>.<ext>` (`output_renamed`),
    /// unless the existing file has identical content.
    RenameOnConflict,
}

impl FinalizeMode {
    /// Mode for paid synchronous outputs (image generate/edit): never discard.
    pub fn for_generated(overwrite: bool) -> FinalizeMode {
        if overwrite { FinalizeMode::Overwrite } else { FinalizeMode::RenameOnConflict }
    }

    /// Mode for downloads and local copies of job outputs (safe to re-run).
    pub fn for_download(overwrite: bool) -> FinalizeMode {
        if overwrite { FinalizeMode::Overwrite } else { FinalizeMode::NoClobber }
    }
}

/// How an artifact ended up at its final path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveOutcome {
    /// New file written (or an existing one replaced with `Overwrite`).
    Written,
    /// An identical file was already at the target; nothing was written.
    AlreadyPresent,
    /// A different file appeared at `requested`; saved next to it instead.
    Renamed { requested: PathBuf },
}

/// A saved (or already present) artifact.
#[derive(Debug, Clone)]
pub struct SavedArtifact {
    /// Absolute path, media type (sniffed), size, SHA-256, dimensions/duration.
    pub artifact: Artifact,
    pub outcome: SaveOutcome,
    /// `output_extension_adjusted`, `output_renamed`, `already_downloaded`.
    pub warnings: Vec<Warning>,
}

/// A temp file in the target's directory, named `.<name>.iris-part-<random>`,
/// removed when dropped unless finalized.
///
/// Write ONLY through [`PartFile::file_mut`] (an async writer can share the same
/// open file via `tokio::fs::File::from_std(part.file_mut().try_clone()?)`), and
/// start a retried download over with [`PartFile::reset`] or a new `PartFile`.
/// Never reopen [`PartFile::path`]; see the module docs.
#[derive(Debug)]
pub struct PartFile {
    tmp: NamedTempFile,
    target: PathBuf,
}

impl PartFile {
    /// Create the temp file for `target` (creating the target directory if needed).
    /// The file is created exclusively (`O_EXCL`) with mode `0666 & !umask`.
    pub fn create_for(target: &Path) -> Result<PartFile, IrisError> {
        let dir = target.parent().filter(|p| !p.as_os_str().is_empty()).ok_or_else(|| {
            IrisError::internal(format!("output path {} has no directory", target.display()))
        })?;
        fs::create_dir_all(dir).map_err(|e| {
            IrisError::io(format_args!("cannot create output directory {}", dir.display()), &e)
        })?;
        let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let name: String = name.chars().take(MAX_NAME_IN_PART).collect();
        let prefix = format!(".{name}.iris-part-");
        let mut builder = tempfile::Builder::new();
        builder.prefix(&prefix).rand_bytes(8);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            builder.permissions(fs::Permissions::from_mode(0o666));
        }
        let tmp = builder.tempfile_in(dir).map_err(|e| {
            IrisError::io(format_args!("cannot create a temporary file in {}", dir.display()), &e)
        })?;
        Ok(PartFile { tmp, target: target.to_path_buf() })
    }

    /// Path of the temp file, for messages. Do not open it: write through
    /// [`PartFile::file_mut`].
    pub fn path(&self) -> &Path {
        self.tmp.path()
    }

    /// The intended final path.
    pub fn target(&self) -> &Path {
        &self.target
    }

    /// The open temp file: the only way to write its content.
    pub fn file_mut(&mut self) -> &mut File {
        self.tmp.as_file_mut()
    }

    /// Empty the temp file and rewind it, through the open handle, so a retried
    /// download starts over without reopening the file by name.
    pub fn reset(&mut self) -> Result<(), IrisError> {
        let shown = self.tmp.path().to_path_buf();
        let file = self.tmp.as_file_mut();
        file.set_len(0)
            .and_then(|()| file.seek(SeekFrom::Start(0)))
            .map(|_| ())
            .map_err(|e| IrisError::io(format_args!("cannot reset {}", shown.display()), &e))
    }

    /// Size and SHA-256 of the content, read through the open handle.
    fn hash(&mut self) -> Result<(u64, String), IrisError> {
        let shown = self.tmp.path().to_path_buf();
        let file = self.tmp.as_file_mut();
        file.seek(SeekFrom::Start(0))
            .and_then(|_| sha256_reader(file))
            .map_err(|e| IrisError::io(format_args!("cannot read {}", shown.display()), &e))
    }
}

/// Validate and save a paid synchronous image (bytes held in memory) to `target`.
///
/// Steps: validate (sniff + full decode of PNG/JPEG/WebP) → temp file → fix the
/// extension if the content type differs from `target`'s
/// (`output_extension_adjusted`) → finalize with `mode` (normally
/// [`FinalizeMode::for_generated`]; at an adjusted path `Overwrite` acts as
/// `RenameOnConflict`). Any valid image type is kept, even one the model does not
/// declare: paid output is never discarded for its format. Content that is not a
/// valid image is `invalid_media` and nothing is written.
pub fn save_image(
    bytes: &[u8],
    target: &Path,
    index: u32,
    mode: FinalizeMode,
) -> Result<SavedArtifact, IrisError> {
    let info = media::validate_bytes(bytes, &[])?;
    if !media::is_image(info.media_type) {
        return Err(IrisError::new(
            ErrorCode::InvalidMedia,
            format!("content is {}, expected an image", info.media_type),
        ));
    }
    let mut part = PartFile::create_for(target)?;
    part.file_mut()
        .write_all(bytes)
        .map_err(|e| IrisError::io(format_args!("cannot write {}", part.path().display()), &e))?;
    let sha256 = sha256_bytes(bytes);
    finish(part, index, info, bytes.len() as u64, sha256, mode, FinalizeMode::RenameOnConflict)
}

/// Validate a downloaded temp file and move it to its target (normally with
/// [`FinalizeMode::for_download`]).
///
/// `expected` is the declared output media types (e.g. `["video/mp4"]`; empty =
/// any). Content of another type of the same kind (video/image) is still valid
/// and is saved under its own extension (`output_extension_adjusted`); content of
/// another kind, unrecognized content (e.g. an error body), or a broken file is
/// `invalid_media`, and the temp file is removed. The final name never holds
/// unvalidated content. At an adjusted path `Overwrite` acts as `NoClobber`: an
/// existing different file there is `output_exists` (the download can be repeated
/// to another path; nothing unrelated is replaced).
pub fn finalize_download(
    mut part: PartFile,
    index: u32,
    expected: &[&str],
    mode: FinalizeMode,
) -> Result<SavedArtifact, IrisError> {
    let info = validate_part(&mut part, expected)
        .map_err(|e| e.with_detail("path", part.target().to_string_lossy().into_owned()))?;
    let (bytes, sha256) = part.hash()?;
    finish(part, index, info, bytes, sha256, mode, FinalizeMode::NoClobber)
}

/// Validate a finished temp file through its open handle; see
/// [`finalize_download`] for how `expected` is applied.
pub(super) fn validate_part(part: &mut PartFile, expected: &[&str]) -> Result<MediaInfo, IrisError> {
    let shown = part.path().to_path_buf();
    let info = media::validate_reader(part.file_mut(), &shown, &[])?;
    if !expected.is_empty() {
        require_same_kind(expected, info.media_type)?;
    }
    Ok(info)
}

/// `media_type` must be of the same kind (image or video) as one of `expected`.
fn require_same_kind(expected: &[&str], media_type: &str) -> Result<(), IrisError> {
    let same_kind = |e: &&str| {
        (media::is_image(e) && media::is_image(media_type))
            || (media::is_video(e) && media::is_video(media_type))
    };
    if expected.iter().any(same_kind) {
        Ok(())
    } else {
        Err(IrisError::new(
            ErrorCode::InvalidMedia,
            format!("content is {media_type}, expected {}", expected.join(" or ")),
        ))
    }
}

/// Place a validated temp file at its requested path, or at the path with the
/// extension of its content type (`output_extension_adjusted`). `Overwrite` only
/// applies to the requested path; at an adjusted path it becomes `at_adjusted`.
pub(super) fn finish(
    part: PartFile,
    index: u32,
    info: MediaInfo,
    bytes: u64,
    sha256: String,
    mode: FinalizeMode,
    at_adjusted: FinalizeMode,
) -> Result<SavedArtifact, IrisError> {
    let requested = part.target().to_path_buf();
    let (target, adjusted) = paths::adjust_extension(&requested, info.media_type);
    let mode = if adjusted.is_some() && mode == FinalizeMode::Overwrite { at_adjusted } else { mode };
    let (path, outcome) = place(part, &target, &sha256, mode).map_err(|e| {
        if adjusted.is_some() && e.code == ErrorCode::OutputExists {
            adjusted_target_exists(&requested, &target, info.media_type)
        } else {
            e
        }
    })?;
    let warnings = adjusted.into_iter().chain(outcome_warning(&path, &outcome)).collect();
    Ok(SavedArtifact { artifact: build_artifact(index, &path, &info, bytes, sha256), outcome, warnings })
}

/// `output_exists` for an extension-adjusted path, explaining why `--overwrite`
/// did not apply to it.
fn adjusted_target_exists(requested: &Path, adjusted: &Path, media_type: &str) -> IrisError {
    IrisError::new(
        ErrorCode::OutputExists,
        format!(
            "the content is {media_type}, so it is saved as {} instead of {}, and a different file already \
             exists there",
            adjusted.display(),
            requested.display()
        ),
    )
    .with_detail("path", adjusted.to_string_lossy().into_owned())
    .with_hint(
        "--overwrite only replaces the path you named; move the existing file away, or choose another \
         -o/--output or -d/--out-dir",
    )
}

/// The warning that goes with a non-trivial [`SaveOutcome`] (`already_downloaded`
/// or `output_renamed`).
pub(super) fn outcome_warning(path: &Path, outcome: &SaveOutcome) -> Option<Warning> {
    match outcome {
        SaveOutcome::Written => None,
        SaveOutcome::AlreadyPresent => Some(already_present_warning(path)),
        SaveOutcome::Renamed { requested } => Some(Warning::new(
            "output_renamed",
            format!(
                "a different file already exists at {}; saved to {} instead so nothing is overwritten",
                requested.display(),
                path.display()
            ),
        )),
    }
}

/// Move a finished temp file to `target` according to `mode`. `sha256` is the
/// temp file's content hash (used to detect an identical existing target).
/// Returns the final path and outcome. The temp file is removed on failure.
pub fn place(
    part: PartFile,
    target: &Path,
    sha256: &str,
    mode: FinalizeMode,
) -> Result<(PathBuf, SaveOutcome), IrisError> {
    // Make the content durable before it gets its final name (through the held
    // handle, never by reopening the name).
    part.tmp
        .as_file()
        .sync_all()
        .map_err(|e| IrisError::io(format_args!("cannot sync {}", part.path().display()), &e))?;
    let dir = target.parent().map(Path::to_path_buf);
    let io_err = |e: &io::Error| IrisError::io(format_args!("cannot save {}", target.display()), e);

    let result = match mode {
        FinalizeMode::Overwrite => {
            if target.is_dir() {
                return Err(IrisError::invalid(format!(
                    "output path {} is an existing directory",
                    target.display()
                )));
            }
            part.tmp.persist(target).map_err(|e| io_err(&e.error))?;
            (target.to_path_buf(), SaveOutcome::Written)
        }
        FinalizeMode::NoClobber | FinalizeMode::RenameOnConflict => {
            match part.tmp.persist_noclobber(target) {
                Ok(_) => (target.to_path_buf(), SaveOutcome::Written),
                Err(e) if e.error.kind() == io::ErrorKind::AlreadyExists => {
                    let tmp = e.file;
                    if identical(target, sha256) {
                        drop(tmp);
                        (target.to_path_buf(), SaveOutcome::AlreadyPresent)
                    } else if mode == FinalizeMode::NoClobber {
                        drop(tmp);
                        return Err(paths::output_exists(target));
                    } else {
                        let mut tmp = tmp;
                        let mut placed = None;
                        for n in 1..=MAX_RENAME_ATTEMPTS {
                            let candidate = paths::numbered(target, n);
                            match tmp.persist_noclobber(&candidate) {
                                Ok(_) => {
                                    placed = Some(candidate);
                                    break;
                                }
                                Err(e) if e.error.kind() == io::ErrorKind::AlreadyExists => tmp = e.file,
                                Err(e) => return Err(io_err(&e.error)),
                            }
                        }
                        let path = placed.ok_or_else(|| {
                            IrisError::new(
                                ErrorCode::IoError,
                                format!("could not find a free file name next to {}", target.display()),
                            )
                        })?;
                        (path, SaveOutcome::Renamed { requested: target.to_path_buf() })
                    }
                }
                Err(e) => return Err(io_err(&e.error)),
            }
        }
    };
    if let Some(dir) = dir
        && let Ok(d) = File::open(&dir)
    {
        let _ = d.sync_all();
    }
    Ok(result)
}

/// `true` if `path` is a regular file whose SHA-256 is `sha256`.
fn identical(path: &Path, sha256: &str) -> bool {
    fs::metadata(path).is_ok_and(|m| m.is_file())
        && sha256_file(path).is_ok_and(|(_, existing)| existing.eq_ignore_ascii_case(sha256))
}

/// The `already_downloaded` warning for an identical file at `path`.
pub fn already_present_warning(path: &Path) -> Warning {
    Warning::new(
        "already_downloaded",
        format!("an identical file is already at {}; nothing was written", path.display()),
    )
}

/// Build the public [`Artifact`] for a saved file.
pub fn build_artifact(index: u32, path: &Path, info: &MediaInfo, bytes: u64, sha256: String) -> Artifact {
    Artifact {
        index,
        path: path.to_string_lossy().into_owned(),
        media_type: info.media_type.to_string(),
        bytes,
        sha256,
        width: info.width,
        height: info.height,
        duration_seconds: info.duration_seconds,
    }
}

/// Size and lowercase hex SHA-256 of a file, streamed.
pub fn sha256_file(path: &Path) -> io::Result<(u64, String)> {
    sha256_reader(&mut File::open(path)?)
}

/// Size and lowercase hex SHA-256 of everything `reader` yields, streamed.
fn sha256_reader(reader: &mut impl Read) -> io::Result<(u64, String)> {
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total += n as u64;
    }
    Ok((total, hex::encode(hasher.finalize())))
}

/// Lowercase hex SHA-256 of bytes.
pub fn sha256_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
