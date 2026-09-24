//! Atomic finalization, no-clobber/overwrite/rename rules, repeat downloads, and
//! local copies (see `iris --help` and docs/jobs.md "Downloads").

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

/// A minimal valid QuickTime file.
fn mov() -> Vec<u8> {
    [bx(b"ftyp", b"qt  \0\0\0\0qt  "), bx(b"moov", &[])].concat()
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
fn stale_part_files_of_the_same_target_are_removed_and_nothing_else() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("job_x.mp4");
    // A part file really created for the target and then abandoned (as after
    // SIGKILL), plus one named by hand the same way.
    let abandoned = PartFile::create_for(&target).unwrap();
    let abandoned_path = abandoned.path().to_path_buf();
    std::mem::forget(abandoned);
    let stale = dir.path().join(".job_x.mp4.iris-part-AbCd1234");
    fs::write(&stale, b"partial").unwrap();
    let keep = [
        "job_x.mp4",
        ".job_x.mp4.iris-part-short",
        ".job_x.mp4.iris-part-AbCd12345",
        ".job_x.mp4.iris-part-AbCd-234",
        ".job_y.mp4.iris-part-AbCd1234",
        ".iris-preflight.iris-part-AbCd1234",
        "job_x.mp4.iris-part-AbCd1234",
    ];
    for name in keep {
        fs::write(dir.path().join(name), b"keep").unwrap();
    }
    fs::create_dir(dir.path().join(".job_x.mp4.iris-part-DirDir12")).unwrap();
    #[cfg(unix)]
    {
        let outside = dir.path().join("outside.bin");
        fs::write(&outside, b"never touched").unwrap();
        std::os::unix::fs::symlink(&outside, dir.path().join(".job_x.mp4.iris-part-Link1234")).unwrap();
    }

    let mut removed = PartFile::remove_stale(&target);
    removed.sort();
    let mut expected = vec![abandoned_path.clone(), stale.clone()];
    expected.sort();
    assert_eq!(removed, expected);
    assert!(!abandoned_path.exists() && !stale.exists());
    for name in keep {
        assert!(dir.path().join(name).exists(), "{name} was removed");
    }
    assert!(dir.path().join(".job_x.mp4.iris-part-DirDir12").is_dir());
    #[cfg(unix)]
    {
        assert!(fs::symlink_metadata(dir.path().join(".job_x.mp4.iris-part-Link1234")).is_ok());
        assert_eq!(fs::read(dir.path().join("outside.bin")).unwrap(), b"never touched");
    }
    // A missing directory is not an error.
    assert!(PartFile::remove_stale(&dir.path().join("missing").join("job_x.mp4")).is_empty());
}

#[test]
fn part_files_are_reset_and_synced_through_the_open_handle() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("job_x.mp4");
    let mut part = PartFile::create_for(&target).unwrap();
    // A failed first attempt left an error body; the retry starts over.
    part.file_mut().write_all(br#"{"error":"partial"#).unwrap();
    part.reset().unwrap();
    part.file_mut().write_all(&mp4(4000)).unwrap();
    let saved = finalize_download(part, 0, VIDEO_TYPES, FinalizeMode::for_download(false)).unwrap();
    assert_eq!(saved.artifact.bytes, mp4(4000).len() as u64);
    assert_eq!(saved.artifact.sha256, sha256_bytes(&mp4(4000)));
    assert_eq!(fs::read(&target).unwrap(), mp4(4000));
    assert_eq!(listing(dir.path()), vec!["job_x.mp4"]);
}

#[cfg(unix)]
#[test]
fn a_symlink_swapped_in_at_the_part_name_is_never_written_through() {
    // In a shared writable directory another user could replace the random temp
    // name with a symlink. Writes, resets, validation and hashing use the handle
    // opened with O_EXCL, so the link target is never touched.
    let dir = tempfile::tempdir().unwrap();
    let victim = dir.path().join("victim.txt");
    fs::write(&victim, b"precious").unwrap();
    let mut part = PartFile::create_for(&dir.path().join("job_x.mp4")).unwrap();
    let name = part.path().to_path_buf();
    fs::remove_file(&name).unwrap();
    std::os::unix::fs::symlink(&victim, &name).unwrap();

    part.file_mut().write_all(b"garbage").unwrap();
    part.reset().unwrap();
    part.file_mut().write_all(&mp4(4000)).unwrap();
    assert_eq!(fs::read(&victim).unwrap(), b"precious");
    drop(part);
    assert_eq!(fs::read(&victim).unwrap(), b"precious");
}

#[test]
fn downloads_are_validated_then_finalized() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("job_x.mp4");
    // Writers use the open handle, never the temp file's name.
    let mut part = PartFile::create_for(&target).unwrap();
    part.file_mut().write_all(&mp4(4000)).unwrap();
    let saved = finalize_download(part, 0, VIDEO_TYPES, FinalizeMode::for_download(false)).unwrap();
    assert_eq!(saved.outcome, SaveOutcome::Written);
    assert_eq!(saved.artifact.media_type, "video/mp4");
    assert_eq!(saved.artifact.duration_seconds, Some(4.0));
    assert_eq!(saved.artifact.bytes, mp4(4000).len() as u64);
    assert_eq!(listing(dir.path()), vec!["job_x.mp4"]);

    // Repeat download of identical content: safe no-op.
    let mut part = PartFile::create_for(&target).unwrap();
    part.file_mut().write_all(&mp4(4000)).unwrap();
    let again = finalize_download(part, 0, VIDEO_TYPES, FinalizeMode::for_download(false)).unwrap();
    assert_eq!(again.outcome, SaveOutcome::AlreadyPresent);
    assert_eq!(again.warnings[0].code, "already_downloaded");

    // Different content without --overwrite: output_exists, nothing replaced.
    let mut part = PartFile::create_for(&target).unwrap();
    part.file_mut().write_all(&mp4(5000)).unwrap();
    let err = finalize_download(part, 0, VIDEO_TYPES, FinalizeMode::for_download(false)).unwrap_err();
    assert_eq!(err.code, ErrorCode::OutputExists);
    assert_eq!(fs::read(&target).unwrap(), mp4(4000));
    assert_eq!(listing(dir.path()), vec!["job_x.mp4"]);
}

#[test]
fn error_bodies_and_truncated_downloads_never_reach_the_final_name() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("job_x.mp4");

    let mut part = PartFile::create_for(&target).unwrap();
    part.file_mut().write_all(br#"{"error":{"code":404,"status":"NOT_FOUND"}}"#).unwrap();
    let err = finalize_download(part, 0, VIDEO_TYPES, FinalizeMode::Overwrite).unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidMedia);
    assert!(err.message.contains("JSON"));
    assert_eq!(err.details["path"], target.to_str().unwrap());

    let mut part = PartFile::create_for(&target).unwrap();
    let full = mp4(4000);
    part.file_mut().write_all(&full[..full.len() - 100]).unwrap();
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
    let rec = RecordedFile {
        path: &recorded,
        bytes: content.len() as u64,
        sha256: &sha,
        media_type: Some("video/mp4"),
    };

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
fn repeat_downloads_recognize_a_file_saved_under_an_adjusted_extension() {
    // Planned job_x.mp4, the provider returned QuickTime, saved as job_x.mov.
    let dir = tempfile::tempdir().unwrap();
    let saved = dir.path().join("job_x.mov");
    let content = mov();
    fs::write(&saved, &content).unwrap();
    let sha = sha256_bytes(&content);
    let rec = RecordedFile {
        path: &saved,
        bytes: content.len() as u64,
        sha256: &sha,
        media_type: Some("video/quicktime"),
    };
    let planned = dir.path().join("job_x.mp4");
    assert_eq!(decide_download(Some(rec), &planned), DownloadDecision::AlreadyDownloaded);
    assert_eq!(decide_download(Some(rec), &saved), DownloadDecision::AlreadyDownloaded);
    assert_eq!(decide_download(Some(rec), &dir.path().join("other.mp4")), DownloadDecision::CopyLocal);
    // Without the media type Iris cannot know the adjusted name: copy (still no network).
    let untyped = RecordedFile { media_type: None, ..rec };
    assert_eq!(decide_download(Some(untyped), &planned), DownloadDecision::CopyLocal);
}

#[test]
fn overwrite_never_replaces_a_file_at_an_extension_adjusted_path() {
    let dir = tempfile::tempdir().unwrap();

    // Paid image: -o photo.png --overwrite, the provider returns a JPEG, and an
    // unrelated photo.jpg exists. It is kept; the output goes next to it.
    let unrelated = dir.path().join("photo.jpg");
    fs::write(&unrelated, b"a holiday picture").unwrap();
    let jpeg = image(ImageFormat::Jpeg, 7);
    let saved =
        save_image(&jpeg, &dir.path().join("photo.png"), 0, FinalizeMode::for_generated(true)).unwrap();
    assert_eq!(fs::read(&unrelated).unwrap(), b"a holiday picture");
    assert_eq!(Path::new(&saved.artifact.path), dir.path().join("photo.1.jpg"));
    assert_eq!(saved.outcome, SaveOutcome::Renamed { requested: unrelated.clone() });
    let codes: Vec<&str> = saved.warnings.iter().map(|w| w.code.as_str()).collect();
    assert_eq!(codes, vec!["output_extension_adjusted", "output_renamed"]);
    assert_eq!(fs::read(dir.path().join("photo.1.jpg")).unwrap(), jpeg);

    // --overwrite still replaces the path the user named when no adjustment happens.
    let named = dir.path().join("named.jpg");
    fs::write(&named, b"old").unwrap();
    save_image(&jpeg, &named, 0, FinalizeMode::for_generated(true)).unwrap();
    assert_eq!(fs::read(&named).unwrap(), jpeg);

    // Download: -o job_x.mp4 --overwrite, QuickTime content, unrelated job_x.mov.
    let other_mov = dir.path().join("job_x.mov");
    fs::write(&other_mov, b"someone's edit").unwrap();
    let mut part = PartFile::create_for(&dir.path().join("job_x.mp4")).unwrap();
    part.file_mut().write_all(&mov()).unwrap();
    let err = finalize_download(part, 0, VIDEO_TYPES, FinalizeMode::for_download(true)).unwrap_err();
    assert_eq!(err.code, ErrorCode::OutputExists);
    assert!(err.message.contains("video/quicktime"), "{}", err.message);
    assert!(err.hint.as_deref().unwrap().contains("only replaces the path you named"));
    assert_eq!(err.details["path"], other_mov.to_str().unwrap());
    assert_eq!(fs::read(&other_mov).unwrap(), b"someone's edit");

    // Identical content there is still a successful no-op.
    fs::write(&other_mov, mov()).unwrap();
    let mut part = PartFile::create_for(&dir.path().join("job_x.mp4")).unwrap();
    part.file_mut().write_all(&mov()).unwrap();
    let again = finalize_download(part, 0, VIDEO_TYPES, FinalizeMode::for_download(true)).unwrap();
    assert_eq!(again.outcome, SaveOutcome::AlreadyPresent);

    // Local copy with the same rule.
    let source = dir.path().join("src.mov");
    fs::write(&source, mov()).unwrap();
    let sha = sha256_bytes(&mov());
    let rec = RecordedFile {
        path: &source,
        bytes: mov().len() as u64,
        sha256: &sha,
        media_type: Some("video/quicktime"),
    };
    let clip_mov = dir.path().join("clip.mov");
    fs::write(&clip_mov, b"unrelated").unwrap();
    let err = copy_local(rec, &dir.path().join("clip.mp4"), 0, VIDEO_TYPES, FinalizeMode::for_download(true))
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::OutputExists);
    assert_eq!(fs::read(&clip_mov).unwrap(), b"unrelated");
    assert!(!dir.path().join("clip.mp4").exists());

    let mut names = listing(dir.path());
    names.sort();
    assert_eq!(names, vec!["clip.mov", "job_x.mov", "named.jpg", "photo.1.jpg", "photo.jpg", "src.mov"]);
}

#[test]
fn local_copies_need_no_network_and_verify_the_source() {
    let dir = tempfile::tempdir().unwrap();
    let recorded = dir.path().join("job_x.mp4");
    let content = mp4(4000);
    fs::write(&recorded, &content).unwrap();
    let sha = sha256_bytes(&content);
    let rec = RecordedFile {
        path: &recorded,
        bytes: content.len() as u64,
        sha256: &sha,
        media_type: Some("video/mp4"),
    };

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
    let mut part = PartFile::create_for(&target).unwrap();
    part.file_mut().write_all(&mov()).unwrap();
    let saved = finalize_download(part, 0, VIDEO_TYPES, FinalizeMode::for_download(false)).unwrap();
    assert_eq!(Path::new(&saved.artifact.path), dir.path().join("job_x.mov"));
    assert_eq!(saved.artifact.media_type, "video/quicktime");
    assert_eq!(saved.warnings[0].code, "output_extension_adjusted");

    // An image where a video was expected: invalid, nothing saved.
    let mut part = PartFile::create_for(&target).unwrap();
    part.file_mut().write_all(&image(ImageFormat::Png, 9)).unwrap();
    let err = finalize_download(part, 0, VIDEO_TYPES, FinalizeMode::for_download(false)).unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidMedia);
    assert!(err.message.contains("expected video/mp4"), "{}", err.message);
    assert_eq!(listing(dir.path()), vec!["job_x.mov"]);
}

/// A minimal valid 1x1 GIF: header, screen, 2-color table, one image, trailer.
const GIF_1X1: &[u8] = b"GIF89a\x01\x00\x01\x00\x80\x00\x00\x00\x00\x00\xff\xff\xff\
    \x2c\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02\x44\x01\x00\x3b";

#[test]
fn undeclared_but_valid_paid_images_are_kept() {
    let dir = tempfile::tempdir().unwrap();
    let saved = save_image(GIF_1X1, &dir.path().join("x.png"), 0, FinalizeMode::RenameOnConflict).unwrap();
    assert_eq!(Path::new(&saved.artifact.path), dir.path().join("x.gif"));
    assert_eq!(saved.artifact.media_type, "image/gif");
    assert_eq!((saved.artifact.width, saved.artifact.height), (Some(1), Some(1)));
}

#[test]
fn truncated_paid_images_of_sniff_only_types_are_not_saved_as_valid() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("x.png");
    let err =
        save_image(&GIF_1X1[..GIF_1X1.len() - 1], &target, 0, FinalizeMode::RenameOnConflict).unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidMedia);
    assert!(err.message.contains("trailer"), "{}", err.message);

    // HEIC whose image data box is cut short.
    let mut heic = bx(b"ftyp", b"heic\0\0\0\0mif1heic");
    heic.extend(bx(b"meta", &[0u8; 24]));
    let mdat = bx(b"mdat", &[0x5A; 64]);
    heic.extend_from_slice(&mdat[..40]);
    let err = save_image(&heic, &target, 0, FinalizeMode::RenameOnConflict).unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidMedia);
    assert!(err.message.contains("truncated"), "{}", err.message);
    assert!(listing(dir.path()).is_empty());
}
