//! Atomic finalization, no-clobber/overwrite/rename rules, repeat downloads, and
//! local copies (C-02 "Output paths", C-04 "Downloads").

use std::fs;
use std::io::{Cursor, Write};
use std::path::Path;

use image::{DynamicImage, ImageFormat, Rgb, RgbImage};
use iris::artifacts::{
    DownloadDecision, FinalizeMode, PartFile, RecordedFile, SaveOutcome, copy_local, decide_download,
    finalize_download, save_image, sha256_bytes, sha256_file,
};
use iris::error::ErrorCode;

const VIDEO_TYPES: &[&str] = &["video/mp4"];

fn image(format: ImageFormat, shade: u8) -> Vec<u8> {
    let img = DynamicImage::ImageRgb8(RgbImage::from_pixel(6, 4, Rgb([shade, 10, 10])));
    let mut buf = Cursor::new(Vec::new());
    img.write_to(&mut buf, format).unwrap();
    buf.into_inner()
}

fn bx(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = ((8 + payload.len()) as u32).to_be_bytes().to_vec();
    out.extend_from_slice(kind);
    out.extend_from_slice(payload);
    out
}

fn mp4(duration_ms: u32) -> Vec<u8> {
    let mut mvhd = vec![0u8; 12];
    mvhd.extend_from_slice(&1000u32.to_be_bytes());
    mvhd.extend_from_slice(&duration_ms.to_be_bytes());
    mvhd.extend_from_slice(&[0u8; 80]);
    let ftyp = bx(b"ftyp", b"isom\0\0\0\0isommp42");
    [ftyp, bx(b"moov", &bx(b"mvhd", &mvhd)), bx(b"mdat", &[0x42; 512])].concat()
}

/// Names in `dir`, asserting no temp/part files were left behind.
fn listing(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> =
        fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    assert!(!names.iter().any(|n| n.contains(".iris-part-")), "temp files left behind: {names:?}");
    names
}

#[test]
fn save_image_writes_atomically_and_reports_the_artifact() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("out").join("cat.png");
    let bytes = image(ImageFormat::Png, 1);
    let saved = save_image(&bytes, &target, 0, FinalizeMode::for_generated(false)).unwrap();
    assert_eq!(saved.outcome, SaveOutcome::Written);
    assert!(saved.warnings.is_empty());
    let a = &saved.artifact;
    assert_eq!(a.index, 0);
    assert_eq!(Path::new(&a.path), target);
    assert!(Path::new(&a.path).is_absolute());
    assert_eq!(a.media_type, "image/png");
    assert_eq!(a.bytes, bytes.len() as u64);
    assert_eq!(a.sha256, sha256_bytes(&bytes));
    assert_eq!((a.width, a.height), (Some(6), Some(4)));
    assert_eq!(fs::read(&target).unwrap(), bytes);
    assert_eq!(listing(&dir.path().join("out")), vec!["cat.png"]);
    assert_eq!(sha256_file(&target).unwrap(), (bytes.len() as u64, a.sha256.clone()));
}

#[test]
fn provider_type_mismatch_keeps_the_output_under_the_right_extension() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("cat.png");
    let jpeg = image(ImageFormat::Jpeg, 2);
    let saved = save_image(&jpeg, &target, 1, FinalizeMode::for_generated(false)).unwrap();
    assert_eq!(Path::new(&saved.artifact.path), dir.path().join("cat.jpg"));
    assert_eq!(saved.artifact.media_type, "image/jpeg");
    assert_eq!(saved.warnings[0].code, "output_extension_adjusted");
    assert_eq!(listing(dir.path()), vec!["cat.jpg"]);
}

#[test]
fn paid_outputs_are_renamed_not_discarded_when_a_file_appears() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("cat.png");
    fs::write(&target, b"someone else's file").unwrap();
    fs::write(dir.path().join("cat.1.png"), b"another one").unwrap();
    let bytes = image(ImageFormat::Png, 3);
    let saved = save_image(&bytes, &target, 0, FinalizeMode::RenameOnConflict).unwrap();
    assert_eq!(saved.outcome, SaveOutcome::Renamed { requested: target.clone() });
    assert_eq!(Path::new(&saved.artifact.path), dir.path().join("cat.2.png"));
    assert_eq!(saved.warnings[0].code, "output_renamed");
    assert_eq!(fs::read(&target).unwrap(), b"someone else's file");
    assert_eq!(fs::read(dir.path().join("cat.2.png")).unwrap(), bytes);
    assert_eq!(listing(dir.path()), vec!["cat.1.png", "cat.2.png", "cat.png"]);
}

#[test]
fn identical_existing_content_is_a_successful_no_op() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("cat.png");
    let bytes = image(ImageFormat::Png, 4);
    fs::write(&target, &bytes).unwrap();
    for mode in [FinalizeMode::NoClobber, FinalizeMode::RenameOnConflict] {
        let saved = save_image(&bytes, &target, 0, mode).unwrap();
        assert_eq!(saved.outcome, SaveOutcome::AlreadyPresent);
        assert_eq!(saved.warnings[0].code, "already_downloaded");
        assert_eq!(Path::new(&saved.artifact.path), target);
    }
    assert_eq!(listing(dir.path()), vec!["cat.png"]);
}

#[test]
fn no_clobber_refuses_and_overwrite_replaces() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("cat.webp");
    fs::write(&target, b"old").unwrap();
    let bytes = image(ImageFormat::WebP, 5);

    let err = save_image(&bytes, &target, 0, FinalizeMode::NoClobber).unwrap_err();
    assert_eq!(err.code, ErrorCode::OutputExists);
    assert_eq!(fs::read(&target).unwrap(), b"old");
    assert_eq!(listing(dir.path()), vec!["cat.webp"]);

    let saved = save_image(&bytes, &target, 0, FinalizeMode::for_generated(true)).unwrap();
    assert_eq!(saved.outcome, SaveOutcome::Written);
    assert_eq!(fs::read(&target).unwrap(), bytes);
    assert_eq!(listing(dir.path()), vec!["cat.webp"]);
}

#[test]
fn invalid_content_is_never_saved() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("cat.png");
    let err = save_image(br#"{"error":"nope"}"#, &target, 0, FinalizeMode::RenameOnConflict).unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidMedia);
    assert!(listing(dir.path()).is_empty());

    let err = save_image(&mp4(1000), &target, 0, FinalizeMode::RenameOnConflict).unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidMedia);
    assert!(err.message.contains("expected an image"), "{}", err.message);
    assert!(listing(dir.path()).is_empty());
}

#[test]
fn part_files_are_hidden_named_and_removed_on_drop() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("new").join("job_x.mp4");
    let part = PartFile::create_for(&target).unwrap();
    let name = part.path().file_name().unwrap().to_str().unwrap().to_string();
    assert!(name.starts_with(".job_x.mp4.iris-part-"), "{name}");
    assert_eq!(part.path().parent().unwrap(), dir.path().join("new"));
    assert_eq!(part.target(), target);
    let path = part.path().to_path_buf();
    assert!(path.exists());
    drop(part);
    assert!(!path.exists());
}

#[test]
fn downloads_are_validated_then_finalized() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("job_x.mp4");
    let part = PartFile::create_for(&target).unwrap();
    // The HTTP layer writes by path (truncating on retries).
    fs::write(part.path(), mp4(4000)).unwrap();
    let saved = finalize_download(part, 0, VIDEO_TYPES, FinalizeMode::for_download(false)).unwrap();
    assert_eq!(saved.outcome, SaveOutcome::Written);
    assert_eq!(saved.artifact.media_type, "video/mp4");
    assert_eq!(saved.artifact.duration_seconds, Some(4.0));
    assert_eq!(saved.artifact.bytes, mp4(4000).len() as u64);
    assert_eq!(listing(dir.path()), vec!["job_x.mp4"]);

    // Repeat download of identical content: safe no-op.
    let part = PartFile::create_for(&target).unwrap();
    fs::write(part.path(), mp4(4000)).unwrap();
    let again = finalize_download(part, 0, VIDEO_TYPES, FinalizeMode::for_download(false)).unwrap();
    assert_eq!(again.outcome, SaveOutcome::AlreadyPresent);
    assert_eq!(again.warnings[0].code, "already_downloaded");

    // Different content without --overwrite: output_exists, nothing replaced.
    let part = PartFile::create_for(&target).unwrap();
    fs::write(part.path(), mp4(5000)).unwrap();
    let err = finalize_download(part, 0, VIDEO_TYPES, FinalizeMode::for_download(false)).unwrap_err();
    assert_eq!(err.code, ErrorCode::OutputExists);
    assert_eq!(fs::read(&target).unwrap(), mp4(4000));
    assert_eq!(listing(dir.path()), vec!["job_x.mp4"]);
}

#[test]
fn error_bodies_and_truncated_downloads_never_reach_the_final_name() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("job_x.mp4");

    let part = PartFile::create_for(&target).unwrap();
    fs::write(part.path(), br#"{"error":{"code":404,"status":"NOT_FOUND"}}"#).unwrap();
    let err = finalize_download(part, 0, VIDEO_TYPES, FinalizeMode::Overwrite).unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidMedia);
    assert!(err.message.contains("JSON"));
    assert_eq!(err.details["path"], target.to_str().unwrap());

    let part = PartFile::create_for(&target).unwrap();
    let full = mp4(4000);
    fs::write(part.path(), &full[..full.len() - 100]).unwrap();
    let err = finalize_download(part, 0, VIDEO_TYPES, FinalizeMode::Overwrite).unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidMedia);
    assert!(listing(dir.path()).is_empty());
}

#[cfg(unix)]
#[test]
fn symlinks_at_the_target_are_never_written_through() {
    let dir = tempfile::tempdir().unwrap();
    let victim = dir.path().join("victim.txt");
    fs::write(&victim, b"precious").unwrap();
    let target = dir.path().join("cat.png");
    std::os::unix::fs::symlink(&victim, &target).unwrap();
    let bytes = image(ImageFormat::Png, 6);

    let err = save_image(&bytes, &target, 0, FinalizeMode::NoClobber).unwrap_err();
    assert_eq!(err.code, ErrorCode::OutputExists);
    assert_eq!(fs::read(&victim).unwrap(), b"precious");

    save_image(&bytes, &target, 0, FinalizeMode::Overwrite).unwrap();
    assert!(!fs::symlink_metadata(&target).unwrap().file_type().is_symlink(), "link replaced, not followed");
    assert_eq!(fs::read(&target).unwrap(), bytes);
    assert_eq!(fs::read(&victim).unwrap(), b"precious");
}

#[test]
fn download_decisions() {
    let dir = tempfile::tempdir().unwrap();
    let recorded = dir.path().join("job_x.mp4");
    let content = mp4(4000);
    fs::write(&recorded, &content).unwrap();
    let sha = sha256_bytes(&content);
    let rec = RecordedFile { path: &recorded, bytes: content.len() as u64, sha256: &sha };

    assert_eq!(decide_download(None, &recorded), DownloadDecision::Fetch);
    assert_eq!(decide_download(Some(rec), &recorded), DownloadDecision::AlreadyDownloaded);
    // Same file through a different spelling.
    let dotted = dir.path().join(".").join("job_x.mp4");
    assert_eq!(decide_download(Some(rec), &dotted), DownloadDecision::AlreadyDownloaded);
    assert_eq!(decide_download(Some(rec), &dir.path().join("elsewhere.mp4")), DownloadDecision::CopyLocal);

    let wrong_size = RecordedFile { bytes: 1, ..rec };
    assert_eq!(decide_download(Some(wrong_size), &recorded), DownloadDecision::Fetch);
    let wrong_hash = RecordedFile { sha256: "00", ..rec };
    assert_eq!(decide_download(Some(wrong_hash), &recorded), DownloadDecision::Fetch);

    fs::write(&recorded, mp4(9000)).unwrap(); // same size, different content
    assert_eq!(decide_download(Some(rec), &recorded), DownloadDecision::Fetch);
    fs::remove_file(&recorded).unwrap();
    assert_eq!(decide_download(Some(rec), &recorded), DownloadDecision::Fetch);
}

#[test]
fn local_copies_need_no_network_and_verify_the_source() {
    let dir = tempfile::tempdir().unwrap();
    let recorded = dir.path().join("job_x.mp4");
    let content = mp4(4000);
    fs::write(&recorded, &content).unwrap();
    let sha = sha256_bytes(&content);
    let rec = RecordedFile { path: &recorded, bytes: content.len() as u64, sha256: &sha };

    let target = dir.path().join("copies").join("clip.mp4");
    let saved = copy_local(rec, &target, 0, VIDEO_TYPES, FinalizeMode::for_download(false)).unwrap();
    assert_eq!(saved.outcome, SaveOutcome::Written);
    assert_eq!(fs::read(&target).unwrap(), content);
    assert_eq!(saved.artifact.sha256, sha);
    assert_eq!(saved.artifact.duration_seconds, Some(4.0));
    assert_eq!(listing(&dir.path().join("copies")), vec!["clip.mp4"]);

    // Repeat: identical content already there.
    let again = copy_local(rec, &target, 0, VIDEO_TYPES, FinalizeMode::for_download(false)).unwrap();
    assert_eq!(again.outcome, SaveOutcome::AlreadyPresent);

    // The source changed after it was recorded: refuse to copy.
    let mut f = fs::OpenOptions::new().append(true).open(&recorded).unwrap();
    f.write_all(b"tamper").unwrap();
    let other = dir.path().join("copies").join("other.mp4");
    let err = copy_local(rec, &other, 0, VIDEO_TYPES, FinalizeMode::for_download(false)).unwrap_err();
    assert_eq!(err.code, ErrorCode::IoError);
    assert!(!other.exists());
    assert_eq!(listing(&dir.path().join("copies")), vec!["clip.mp4"]);
}

#[test]
fn same_kind_types_are_kept_and_other_kinds_rejected_for_downloads() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("job_x.mp4");

    // A valid QuickTime file where MP4 was declared: kept, under .mov.
    let mut mov = bx(b"ftyp", b"qt  \0\0\0\0qt  ");
    mov.extend(bx(b"moov", &[]));
    let part = PartFile::create_for(&target).unwrap();
    fs::write(part.path(), &mov).unwrap();
    let saved = finalize_download(part, 0, VIDEO_TYPES, FinalizeMode::for_download(false)).unwrap();
    assert_eq!(Path::new(&saved.artifact.path), dir.path().join("job_x.mov"));
    assert_eq!(saved.artifact.media_type, "video/quicktime");
    assert_eq!(saved.warnings[0].code, "output_extension_adjusted");

    // An image where a video was expected: invalid, nothing saved.
    let part = PartFile::create_for(&target).unwrap();
    fs::write(part.path(), image(ImageFormat::Png, 9)).unwrap();
    let err = finalize_download(part, 0, VIDEO_TYPES, FinalizeMode::for_download(false)).unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidMedia);
    assert!(err.message.contains("expected video/mp4"), "{}", err.message);
    assert_eq!(listing(dir.path()), vec!["job_x.mov"]);
}

#[test]
fn undeclared_but_valid_paid_images_are_kept() {
    let dir = tempfile::tempdir().unwrap();
    let gif = b"GIF89a\x01\x00\x01\x00\x00\x00\x00;";
    let saved = save_image(gif, &dir.path().join("x.png"), 0, FinalizeMode::RenameOnConflict).unwrap();
    assert_eq!(Path::new(&saved.artifact.path), dir.path().join("x.gif"));
    assert_eq!(saved.artifact.width, None);
}
