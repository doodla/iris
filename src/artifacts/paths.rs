//! Output path planning (see `iris --help` and docs/jobs.md).
//!
//! * The output directory precedence (`-d` > `IRIS_OUTPUT_DIR` > config > cwd) is
//!   resolved by the caller and passed in; planned paths are absolute.
//! * Default names: images `iris-<ulid>.<ext>` (`iris-<ulid>-<i>.<ext>` when several),
//!   videos `<job_id>.<ext>` (`<job_id>-<i>.<ext>`), `i` starting at 1. Generated
//!   names use only `[a-z0-9_.-]`; remote data never contributes to local names.
//! * `-o PATH` is used literally; with several artifacts it becomes
//!   `<stem>-<i>.<ext>`. Its extension must agree with `--format` and with what
//!   the model can produce; without `--format` it selects the format.
//! * [`preflight`] refuses existing files before any paid request unless
//!   `--overwrite`; [`preflight_dirs`] makes sure the output directories exist (or
//!   can be created) and are writable, so a paid result is never lost because it
//!   cannot be saved. An output location that cannot be used as given (a file in
//!   the way, no permission, a read-only file system, a parent that cannot be
//!   created) is `invalid_argument` with `details.path`: nothing was sent, and only
//!   choosing another location fixes it.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::domain::Warning;
use crate::error::{ErrorCode, IrisError};

use super::media;

/// How default file names are formed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Naming<'a> {
    /// `iris-<ulid>[-i].<ext>` with a fresh lowercase ULID per plan.
    Image,
    /// `<job_id>[-i].<ext>`; `job_id` must be a validated job id.
    Video { job_id: &'a str },
}

/// Input to [`plan_outputs`].
#[derive(Debug, Clone, Copy)]
pub struct PathRequest<'a> {
    pub naming: Naming<'a>,
    /// Number of artifacts to plan (at least 1).
    pub count: u32,
    /// `-o/--output`, used literally (relative paths resolve against the cwd).
    pub output: Option<&'a Path>,
    /// Output directory already resolved by the caller (used when `output` is `None`).
    pub dir: &'a Path,
    /// Explicit `--format` value (`png`, `jpeg`, `webp`), if any.
    pub format: Option<&'a str>,
    /// Media types the model can produce (catalog `outputs.media_types`); the first
    /// is the default when neither `--format` nor an `-o` extension decides.
    pub media_types: &'a [&'a str],
}

/// Result of [`plan_outputs`].
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedOutputs {
    /// One absolute path per artifact, in index order.
    pub paths: Vec<PathBuf>,
    /// Media type the plan expects the provider to return.
    pub media_type: &'static str,
    /// Image format selected by the `-o` extension when `--format` was not given
    /// (`png`, `jpeg`, `webp`); the caller applies it if the model declares `format`.
    pub implied_format: Option<&'static str>,
    /// E.g. `output_extension_adjusted` when `-o` had no extension.
    pub warnings: Vec<Warning>,
}

/// `--format` value for a media type (`image/jpeg` → `jpeg`).
pub fn format_for_media_type(media_type: &str) -> Option<&'static str> {
    match media::media_type_for_extension(media::extension_for(media_type)?)? {
        media::PNG => Some("png"),
        media::JPEG => Some("jpeg"),
        media::WEBP => Some("webp"),
        _ => None,
    }
}

/// Media type for a `--format` value.
pub fn media_type_for_format(format: &str) -> Option<&'static str> {
    match format.to_ascii_lowercase().as_str() {
        "png" => Some(media::PNG),
        "jpeg" | "jpg" => Some(media::JPEG),
        "webp" => Some(media::WEBP),
        _ => None,
    }
}

/// Plan the absolute output paths of one request/job. Performs no I/O besides
/// checking whether `-o` names an existing directory; call [`preflight`] before
/// paid requests.
pub fn plan_outputs(req: &PathRequest<'_>) -> Result<PlannedOutputs, IrisError> {
    if req.count == 0 {
        return Err(IrisError::internal("output planning needs at least one artifact"));
    }
    let default_type = req
        .media_types
        .first()
        .and_then(|t| canonical(t))
        .ok_or_else(|| IrisError::internal("the model declares no output media types"))?;
    let format_type = match req.format {
        Some(f) => {
            let t = media_type_for_format(f).ok_or_else(|| {
                IrisError::invalid(format!("unknown output format '{f}' (expected png, jpeg, or webp)"))
            })?;
            if !media::accepts(req.media_types, t) {
                return Err(IrisError::invalid(format!(
                    "this model cannot produce {f} output (it produces {})",
                    req.media_types.join(", ")
                )));
            }
            Some(t)
        }
        None => None,
    };

    let mut warnings = Vec::new();
    let mut implied_format = None;
    let (media_type, paths) = match req.output {
        Some(output) => {
            let output = absolute(output)?;
            if output.as_os_str().to_string_lossy().ends_with(std::path::MAIN_SEPARATOR) || output.is_dir() {
                return Err(IrisError::invalid(format!(
                    "-o/--output expects a file path, but {} is a directory; use -d/--out-dir for directories",
                    output.display()
                )));
            }
            let ext = output.extension().map(|e| e.to_string_lossy().into_owned());
            let (media_type, base) = match ext {
                None => {
                    let t = format_type.unwrap_or(default_type);
                    let ext = media::extension_for(t).unwrap_or("bin");
                    let mut name = output.file_name().unwrap_or_default().to_os_string();
                    name.push(format!(".{ext}"));
                    let adjusted = output.with_file_name(name);
                    warnings.push(Warning::new(
                        "output_extension_adjusted",
                        format!("{} has no extension; saving as {}", output.display(), adjusted.display()),
                    ));
                    (t, adjusted)
                }
                Some(ext) => {
                    let ext_type = media::media_type_for_extension(&ext).ok_or_else(|| {
                        IrisError::invalid(format!(
                            "unrecognized output extension '.{ext}' in {}; use one of {}",
                            output.display(),
                            extension_list(req.media_types)
                        ))
                    })?;
                    if let Some(t) = format_type {
                        if !same_type(t, ext_type) {
                            return Err(IrisError::invalid(format!(
                                "-o/--output extension '.{ext}' contradicts --format {}",
                                req.format.unwrap_or_default()
                            )));
                        }
                    } else if !media::accepts(req.media_types, ext_type) {
                        return Err(IrisError::invalid(format!(
                            "this model cannot produce {ext_type} ('.{ext}'); use one of {}",
                            extension_list(req.media_types)
                        )));
                    } else {
                        implied_format = format_for_media_type(ext_type);
                    }
                    (ext_type, output)
                }
            };
            let paths = if req.count == 1 {
                vec![base]
            } else {
                (1..=req.count).map(|i| indexed(&base, i)).collect()
            };
            (media_type, paths)
        }
        None => {
            let media_type = format_type.unwrap_or(default_type);
            let ext = media::extension_for(media_type).unwrap_or("bin");
            let dir = absolute(req.dir)?;
            let stem = match req.naming {
                Naming::Image => format!("iris-{}", ulid::Ulid::generate().to_string().to_ascii_lowercase()),
                Naming::Video { job_id } => {
                    if job_id.is_empty()
                        || !job_id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
                    {
                        return Err(IrisError::internal(format!(
                            "invalid job id for a file name: {job_id:?}"
                        )));
                    }
                    job_id.to_string()
                }
            };
            let paths = if req.count == 1 {
                vec![dir.join(format!("{stem}.{ext}"))]
            } else {
                (1..=req.count).map(|i| dir.join(format!("{stem}-{i}.{ext}"))).collect()
            };
            (media_type, paths)
        }
    };
    Ok(PlannedOutputs { paths, media_type, implied_format, warnings })
}

/// Check planned paths before any paid request: an existing file without
/// `overwrite` is `output_exists` (exit 2, nothing sent); an existing directory is
/// always `invalid_argument`, and so is a path that cannot be used as given (a file
/// where a directory should be, no permission to look).
pub fn preflight(paths: &[PathBuf], overwrite: bool) -> Result<(), IrisError> {
    for path in paths {
        match std::fs::symlink_metadata(path) {
            Ok(meta) if meta.is_dir() => {
                return Err(IrisError::invalid(format!(
                    "output path {} is an existing directory",
                    path.display()
                ))
                .with_detail("path", path.to_string_lossy().into_owned())
                .with_hint(DIR_HINT));
            }
            Ok(_) if !overwrite => return Err(output_exists(path)),
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => {
                // A regular file in the way names the real culprit.
                if e.kind() == io::ErrorKind::NotADirectory
                    && let Some(dir) = path.parent()
                {
                    check_ancestors(dir)?;
                }
                return Err(location_error(
                    format_args!("cannot check output path {}", path.display()),
                    path,
                    &e,
                ));
            }
        }
    }
    Ok(())
}

/// Check the directories of planned output paths before any paid request (this
/// runs as local validation before any network call; paid output is never
/// discarded, `-d` is created if missing). For each distinct parent directory:
///
/// * the nearest existing ancestor must be a directory; a regular file (or a
///   broken symbolic link) in the way is `invalid_argument` (exit 2);
/// * with `create` (every real run; pass `false` for `--dry-run`, which must not
///   touch the filesystem) the directory is created if missing and proven
///   writable by creating and removing a `.iris-preflight.iris-part-*` temp file.
///
/// A location that cannot be used as given (no permission, a read-only file
/// system, a parent that cannot be created, a file in the way) is
/// `invalid_argument` with `details.path` and a hint; other I/O failures (a full
/// disk, a device error) stay `io_error`. Nothing is sent before this passes, so an
/// unusable directory costs nothing.
pub fn preflight_dirs(paths: &[PathBuf], create: bool) -> Result<(), IrisError> {
    let mut checked: Vec<&Path> = Vec::new();
    for path in paths {
        let dir = path
            .parent()
            .filter(|d| !d.as_os_str().is_empty())
            .ok_or_else(|| IrisError::internal(format!("output path {} has no directory", path.display())))?;
        if checked.contains(&dir) {
            continue;
        }
        checked.push(dir);
        check_ancestors(dir)?;
        if create {
            fs::create_dir_all(dir).map_err(|e| {
                location_error(format_args!("cannot create output directory {}", dir.display()), dir, &e)
            })?;
            tempfile::Builder::new()
                .prefix(".iris-preflight.iris-part-")
                .rand_bytes(8)
                .tempfile_in(dir)
                .map_err(|e| {
                    location_error(
                        format_args!("output directory {} is not writable", dir.display()),
                        dir,
                        &e,
                    )
                })?;
        }
    }
    Ok(())
}

/// The nearest existing ancestor of `dir` (itself included) must be a directory.
fn check_ancestors(dir: &Path) -> Result<(), IrisError> {
    for ancestor in dir.ancestors().filter(|a| !a.as_os_str().is_empty()) {
        let blocked = |what: &str| {
            IrisError::invalid(format!(
                "output directory {} cannot be used: {} is {what}",
                dir.display(),
                ancestor.display()
            ))
            .with_detail("path", ancestor.to_string_lossy().into_owned())
            .with_hint(DIR_HINT)
        };
        match fs::metadata(ancestor) {
            Ok(meta) if meta.is_dir() => return Ok(()),
            Ok(_) => return Err(blocked("not a directory")),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                if fs::symlink_metadata(ancestor).is_ok() {
                    return Err(blocked("a broken symbolic link"));
                }
            }
            // A path component further up is a file; the loop reaches it next.
            Err(e) if e.kind() == io::ErrorKind::NotADirectory => {}
            Err(e) => {
                return Err(location_error(
                    format_args!("cannot check output directory {}", dir.display()),
                    dir,
                    &e,
                ));
            }
        }
    }
    Ok(())
}

const DIR_HINT: &str = "choose a writable directory with -d/--out-dir, or another -o/--output path";

/// Whether an I/O error on an output location means the location cannot be used as
/// given, so only another path fixes it (`invalid_argument`, exit 2), rather than a
/// runtime failure such as a full disk (`io_error`).
fn is_unusable_location(e: &io::Error) -> bool {
    use io::ErrorKind::*;
    matches!(
        e.kind(),
        NotFound
            | PermissionDenied
            | NotADirectory
            | IsADirectory
            | ReadOnlyFilesystem
            | InvalidFilename
            | AlreadyExists
            | InvalidInput
    )
}

/// The error for an output location `path` that failed with `e` during preflight
/// (see [`is_unusable_location`]); both kinds carry `details.path` and a hint.
fn location_error(context: std::fmt::Arguments<'_>, path: &Path, e: &io::Error) -> IrisError {
    let err = if is_unusable_location(e) {
        IrisError::invalid(format!("{context}: {e}"))
    } else {
        IrisError::io(context, e)
    };
    err.with_detail("path", path.to_string_lossy().into_owned()).with_hint(DIR_HINT)
}

/// The `output_exists` error for `path`.
pub fn output_exists(path: &Path) -> IrisError {
    IrisError::new(ErrorCode::OutputExists, format!("output file {} already exists", path.display()))
        .with_detail("path", path.to_string_lossy().into_owned())
        .with_hint("pass --overwrite to replace it, or choose another -o/--output or -d/--out-dir")
}

/// If `path`'s extension does not match `media_type`, return the path with the
/// canonical extension for that type and an `output_extension_adjusted` warning
/// (paid output is saved under the correct extension rather than discarded).
pub fn adjust_extension(path: &Path, media_type: &str) -> (PathBuf, Option<Warning>) {
    let Some(want) = media::extension_for(media_type) else {
        return (path.to_path_buf(), None);
    };
    let current = path.extension().and_then(|e| e.to_str()).and_then(media::media_type_for_extension);
    if current.is_some_and(|t| same_type(t, media_type)) {
        return (path.to_path_buf(), None);
    }
    let adjusted = path.with_extension(want);
    let warning = Warning::new(
        "output_extension_adjusted",
        format!(
            "the provider returned {media_type}; saving as {} instead of {}",
            adjusted.display(),
            path.display()
        ),
    );
    (adjusted, Some(warning))
}

/// `<stem>.<n>.<ext>` next to `path` (the race fallback name for `output_renamed`).
pub fn numbered(path: &Path, n: u32) -> PathBuf {
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let name = match path.extension() {
        Some(ext) => format!("{stem}.{n}.{}", ext.to_string_lossy()),
        None => format!("{stem}.{n}"),
    };
    path.with_file_name(name)
}

/// `<stem>-<i>.<ext>` next to `path` (several artifacts with one `-o`).
fn indexed(path: &Path, i: u32) -> PathBuf {
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let name = match path.extension() {
        Some(ext) => format!("{stem}-{i}.{}", ext.to_string_lossy()),
        None => format!("{stem}-{i}"),
    };
    path.with_file_name(name)
}

fn absolute(path: &Path) -> Result<PathBuf, IrisError> {
    let abs = std::path::absolute(path)
        .map_err(|e| IrisError::io(format_args!("cannot resolve path {}", path.display()), &e))?;
    if abs.to_str().is_none() {
        return Err(IrisError::invalid(format!("output path {} is not valid UTF-8", abs.to_string_lossy())));
    }
    Ok(abs)
}

fn canonical(media_type: &str) -> Option<&'static str> {
    media::media_type_for_extension(media::extension_for(media_type)?)
}

fn same_type(a: &str, b: &str) -> bool {
    canonical(a).is_some() && canonical(a) == canonical(b)
}

fn extension_list(media_types: &[&str]) -> String {
    let mut exts: Vec<String> = Vec::new();
    for t in media_types {
        let Some(t) = canonical(t) else { continue };
        let names: &[&str] = match t {
            media::JPEG => &["jpg", "jpeg"],
            _ => &[],
        };
        if names.is_empty() {
            if let Some(e) = media::extension_for(t) {
                exts.push(format!(".{e}"));
            }
        } else {
            exts.extend(names.iter().map(|e| format!(".{e}")));
        }
    }
    if exts.is_empty() { "a supported media extension".to_string() } else { exts.join(", ") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unusable_output_locations_are_invalid_arguments_and_other_failures_io_errors() {
        let path = Path::new("/somewhere/out");
        for kind in [
            io::ErrorKind::PermissionDenied,
            io::ErrorKind::ReadOnlyFilesystem,
            io::ErrorKind::NotADirectory,
            io::ErrorKind::NotFound,
            io::ErrorKind::IsADirectory,
        ] {
            let e = location_error(
                format_args!("cannot create output directory /somewhere/out"),
                path,
                &kind.into(),
            );
            assert_eq!(e.code, ErrorCode::InvalidArgument, "{kind:?}");
            assert_eq!(e.exit_code(), crate::error::exit::USAGE, "{kind:?}");
            assert_eq!(e.details.get("path"), Some(&serde_json::json!("/somewhere/out")), "{kind:?}");
            assert!(e.hint.is_some(), "{kind:?}");
        }
        for kind in [io::ErrorKind::StorageFull, io::ErrorKind::Other, io::ErrorKind::Interrupted] {
            let e = location_error(
                format_args!("output directory /somewhere/out is not writable"),
                path,
                &kind.into(),
            );
            assert_eq!(e.code, ErrorCode::IoError, "{kind:?}");
            assert_eq!(e.details.get("path"), Some(&serde_json::json!("/somewhere/out")), "{kind:?}");
        }
    }
}
