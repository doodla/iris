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

use super::finalize::{FinalizeMode, PartFile, SavedArtifact, build_artifact, place};
use super::{finalize, paths};

/// What the job record says about a previously downloaded output.
#[derive(Debug, Clone, Copy)]
pub struct RecordedFile<'a> {
    /// Absolute path recorded at download time.
    pub path: &'a Path,
    pub bytes: u64,
    /// Lowercase hex SHA-256 recorded at download time.
    pub sha256: &'a str,
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
/// recorded size and SHA-256 and equals `target`; `CopyLocal` if it is intact but
/// `target` differs; otherwise `Fetch`.
pub fn decide_download(recorded: Option<RecordedFile<'_>>, target: &Path) -> DownloadDecision {
    let Some(rec) = recorded else {
        return DownloadDecision::Fetch;
    };
    if !is_intact(&rec) {
        return DownloadDecision::Fetch;
    }
    if same_path(rec.path, target) {
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
/// [`FinalizeMode::for_download`]). If the source changed, `io_error` is returned
/// and the caller should fall back to [`DownloadDecision::Fetch`].
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
    let (final_target, adjusted) = paths::adjust_extension(target, info.media_type);
    let (path, outcome) = place(part, &final_target, &sha256, mode)?;
    let warnings = adjusted.into_iter().chain(finalize::outcome_warning(&path, &outcome)).collect();
    Ok(SavedArtifact { artifact: build_artifact(index, &path, &info, copied, sha256), outcome, warnings })
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
