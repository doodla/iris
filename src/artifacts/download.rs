//! Local side of job downloads (C-04 "Downloads" step 2): decide whether an output
//! needs the network at all, and copy an already-downloaded file to a new target
//! without touching the network.
//!
//! The network fetch itself (`crate::http::download`) streams into a
//! [`PartFile`](super::PartFile) path; [`finalize_download`](super::finalize_download)
//! then validates and places it.

use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::error::{ErrorCode, IrisError};

use super::finalize::{FinalizeMode, PartFile, SavedArtifact};
use super::{finalize, paths};

/// What the job record says about a previously downloaded output (see
/// `JobOutput::recorded_file`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordedFile<'a> {
    /// Absolute path recorded at download time.
    pub path: &'a Path,
    pub bytes: u64,
    /// Lowercase hex SHA-256 recorded at download time.
    pub sha256: &'a str,
    /// Media type of the saved file (sniffed at download time), used to recognize a
    /// file saved under an adjusted extension.
    pub media_type: Option<&'a str>,
}

/// How to obtain one output at `target`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadDecision {
    /// The recorded file is intact and is the target: nothing to do (report
    /// warning `already_downloaded`; safe to repeat).
    AlreadyDownloaded,
    /// The recorded file is intact but the target differs: [`copy_local`] (no network).
    CopyLocal,
    /// No intact local copy: download from the remote URI.
    Fetch,
}

/// Decide per C-04: `AlreadyDownloaded` if the recorded file exists with the
/// recorded size and SHA-256 and is the file `target` would become; `CopyLocal` if
/// it is intact but `target` differs; otherwise `Fetch`.
///
/// "The file `target` would become" is `target` itself or, when the recorded media
/// type needs another extension, `target` with that extension: a MOV planned as
/// `job.mp4` was saved as `job.mov`, and repeating the download must not copy it.
/// Report the `already_downloaded` warning with the recorded path.
pub fn decide_download(recorded: Option<RecordedFile<'_>>, target: &Path) -> DownloadDecision {
    let Some(rec) = recorded else {
        return DownloadDecision::Fetch;
    };
    if !is_intact(&rec) {
        return DownloadDecision::Fetch;
    }
    let adjusted_target = rec.media_type.and_then(|t| match paths::adjust_extension(target, t) {
        (adjusted, Some(_)) => Some(adjusted),
        (_, None) => None,
    });
    if same_path(rec.path, target) || adjusted_target.is_some_and(|t| same_path(rec.path, &t)) {
        DownloadDecision::AlreadyDownloaded
    } else {
        DownloadDecision::CopyLocal
    }
}

/// True if the recorded file exists as a regular file with the recorded size and hash.
pub fn is_intact(rec: &RecordedFile<'_>) -> bool {
    match std::fs::metadata(rec.path) {
        Ok(m) if m.is_file() && m.len() == rec.bytes => {
            finalize::sha256_file(rec.path).is_ok_and(|(_, sha)| sha.eq_ignore_ascii_case(rec.sha256))
        }
        _ => false,
    }
}

/// Copy an intact downloaded file to `target` without network access: temp file in
/// the target directory (hashing while copying) → verify the hash still matches the
/// record → validate media → finalize with `mode` (normally
/// [`FinalizeMode::for_download`]; at an extension-adjusted path `Overwrite` acts
/// as `NoClobber`, see [`finalize_download`](super::finalize_download)). If the
/// source changed, `io_error` is returned and the caller should fall back to
/// [`DownloadDecision::Fetch`]. Every error here is local: the job record's
/// download state must not be changed because of it.
pub fn copy_local(
    source: RecordedFile<'_>,
    target: &Path,
    index: u32,
    expected: &[&str],
    mode: FinalizeMode,
) -> Result<SavedArtifact, IrisError> {
    let mut part = PartFile::create_for(target)?;
    let mut input = File::open(source.path)
        .map_err(|e| IrisError::io(format_args!("cannot read {}", source.path.display()), &e))?;
    let (copied, sha256) = copy_hashing(&mut input, part.file_mut())
        .map_err(|e| IrisError::io(format_args!("cannot copy {}", source.path.display()), &e))?;
    if copied != source.bytes || !sha256.eq_ignore_ascii_case(source.sha256) {
        return Err(IrisError::new(
            ErrorCode::IoError,
            format!("{} changed since it was downloaded; it will not be copied", source.path.display()),
        )
        .with_detail("path", source.path.to_string_lossy().into_owned()));
    }
    let info = finalize::validate_output_file(part.path(), expected)?;
    finalize::finish(part, index, info, copied, sha256, mode, FinalizeMode::NoClobber)
}

fn copy_hashing(input: &mut impl Read, output: &mut impl Write) -> io::Result<(u64, String)> {
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let n = input.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        output.write_all(&buf[..n])?;
        total += n as u64;
    }
    output.flush()?;
    Ok((total, hex::encode(hasher.finalize())))
}

/// Paths refer to the same file: canonical paths when both resolve, otherwise the
/// absolute paths.
fn same_path(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => std::path::absolute(a).ok() == std::path::absolute(b).ok(),
    }
}
