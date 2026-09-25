//! Last-resort saving of paid synchronous output (see docs/json-contract.md,
//! warning `output_saved_elsewhere`).
//!
//! A paid image that reached Iris is never discarded. When a valid image cannot be
//! saved where it was requested because of an I/O failure after preflight (a full
//! disk, an output directory removed or replaced meanwhile, lost permissions), the
//! application writes it to `<state_dir>/unsaved/<run_id>-<index>.<ext>` with
//! [`save_unsaved`] and reports that path. Returned content that is not a valid
//! image (unrecognized or undecodable bytes, a payload that is not base64) is kept
//! as received in `<state_dir>/unsaved/<run_id>-<n>.bin` with [`save_unsaved_raw`].
//! The directory is created with mode 0700 (like the job directory), and every file
//! goes through the same temp-file, sync, and no-clobber rename as every other
//! output, so it is complete when it appears and never replaces an existing file.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::domain::Artifact;
use crate::error::IrisError;

use super::finalize::{self, FinalizeMode, PartFile};
use super::media;

/// Directory under the state directory that holds paid outputs Iris could not save
/// where they were requested.
const UNSAVED_DIR: &str = "unsaved";

/// `<state_dir>/unsaved`.
fn unsaved_dir(state_dir: &Path) -> PathBuf {
    state_dir.join(UNSAVED_DIR)
}

/// Save a paid image to `<state_dir>/unsaved/<run_id>-<index>.<ext>` and return its
/// artifact (final path, sniffed media type, size, SHA-256, dimensions).
///
/// `run_id` groups the outputs of one command (ASCII letters and digits only, e.g. a
/// lowercase ULID). The bytes are validated like any saved image; content that is
/// not a valid image is `invalid_media` and nothing is written. The extension comes
/// from the content. If a different file already has the name, the image goes to
/// `<run_id>-<index>.<n>.<ext>`; nothing is ever replaced.
pub fn save_unsaved(state_dir: &Path, run_id: &str, index: u32, bytes: &[u8]) -> Result<Artifact, IrisError> {
    check_run_id(run_id)?;
    let info = media::validate_bytes(bytes, &[])?;
    let ext = media::extension_for(info.media_type).unwrap_or("bin");
    let dir = unsaved_dir(state_dir);
    create_private_dir(&dir)?;
    let target = dir.join(format!("{run_id}-{index}.{ext}"));
    let mut part = PartFile::create_for(&target)?;
    part.file_mut()
        .write_all(bytes)
        .map_err(|e| IrisError::io(format_args!("cannot write {}", part.path().display()), &e))?;
    let sha256 = finalize::sha256_bytes(bytes);
    let (path, _) = finalize::place(part, &target, &sha256, FinalizeMode::RenameOnConflict)?;
    Ok(finalize::build_artifact(index, &path, &info, bytes.len() as u64, sha256))
}

/// Save paid content that is not a valid image, exactly as received, to
/// `<state_dir>/unsaved/<run_id>-<n>.bin` (`n`: the item's position in the response)
/// and return the final path. Nothing is validated or converted. If a different
/// file already has the name, the content goes to `<run_id>-<n>.<k>.bin`; nothing
/// is ever replaced.
pub fn save_unsaved_raw(state_dir: &Path, run_id: &str, n: u32, bytes: &[u8]) -> Result<PathBuf, IrisError> {
    check_run_id(run_id)?;
    let dir = unsaved_dir(state_dir);
    create_private_dir(&dir)?;
    let target = dir.join(format!("{run_id}-{n}.bin"));
    let mut part = PartFile::create_for(&target)?;
    part.file_mut()
        .write_all(bytes)
        .map_err(|e| IrisError::io(format_args!("cannot write {}", part.path().display()), &e))?;
    let sha256 = finalize::sha256_bytes(bytes);
    let (path, _) = finalize::place(part, &target, &sha256, FinalizeMode::RenameOnConflict)?;
    Ok(path)
}

/// A run id is used in file names: ASCII letters and digits only.
fn check_run_id(run_id: &str) -> Result<(), IrisError> {
    if run_id.is_empty() || !run_id.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(IrisError::internal(format!("invalid run id for a file name: {run_id:?}")));
    }
    Ok(())
}

/// Create `dir` and any missing parents with mode 0700 (on Unix).
fn create_private_dir(dir: &Path) -> Result<(), IrisError> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(dir)
        .map_err(|e| IrisError::io(format_args!("cannot create directory {}", dir.display()), &e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_content_is_kept_as_received_and_never_replaces_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let first = save_unsaved_raw(dir.path(), "run1", 0, b"not an image").unwrap();
        assert_eq!(first, dir.path().join("unsaved").join("run1-0.bin"));
        assert_eq!(fs::read(&first).unwrap(), b"not an image");
        let second = save_unsaved_raw(dir.path(), "run1", 0, b"other content").unwrap();
        assert_ne!(second, first, "a different file under the name is never replaced");
        assert_eq!(fs::read(&first).unwrap(), b"not an image");
        assert_eq!(fs::read(&second).unwrap(), b"other content");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(dir.path().join("unsaved")).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700);
        }
        assert!(save_unsaved_raw(dir.path(), "../x", 0, b"x").is_err(), "run ids are plain names");
    }
}
