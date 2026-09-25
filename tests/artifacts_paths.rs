//! Output path planning and preflight (see `iris --help`).

use std::path::{Path, PathBuf};

use iris::artifacts::paths::{self, Naming, PathRequest};
use iris::artifacts::{adjust_extension, plan_outputs, preflight, preflight_dirs, preflight_other_types};
use iris::error::ErrorCode;

const IMAGE_TYPES: &[&str] = &["image/png", "image/jpeg", "image/webp"];
const VIDEO_TYPES: &[&str] = &["video/mp4"];
const JOB: &str = "job_01k5z8m3q4r5s6t7v8w9x0y1z2";

fn image_req<'a>(
    dir: &'a Path,
    count: u32,
    output: Option<&'a Path>,
    format: Option<&'a str>,
) -> PathRequest<'a> {
    PathRequest { naming: Naming::Image, count, output, dir, format, media_types: IMAGE_TYPES }
}

fn name(p: &Path) -> &str {
    p.file_name().unwrap().to_str().unwrap()
}

/// `iris-<26 lowercase ULID chars>` (+ suffix)
fn assert_image_default(name: &str, suffix: &str) {
    let stem = name.strip_suffix(suffix).unwrap_or_else(|| panic!("{name} should end with {suffix}"));
    let ulid = stem.strip_prefix("iris-").unwrap();
    assert_eq!(ulid.len(), 26, "{name}");
    assert!(ulid.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()), "{name}");
}

#[test]
fn default_image_names_are_absolute_and_safe() {
    let dir = tempfile::tempdir().unwrap();
    let plan = plan_outputs(&image_req(dir.path(), 1, None, None)).unwrap();
    assert_eq!(plan.paths.len(), 1);
    assert_eq!(plan.media_type, "image/png");
    assert_eq!(plan.implied_format, None);
    assert!(plan.warnings.is_empty());
    let p = &plan.paths[0];
    assert!(p.is_absolute());
    assert_eq!(p.parent().unwrap(), dir.path());
    assert_image_default(name(p), ".png");
    assert!(name(p).bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_.-".contains(&b)));
}

#[test]
fn several_images_share_one_ulid_with_indices_from_one() {
    let dir = tempfile::tempdir().unwrap();
    let plan = plan_outputs(&image_req(dir.path(), 3, None, Some("jpeg"))).unwrap();
    assert_eq!(plan.media_type, "image/jpeg");
    let names: Vec<&str> = plan.paths.iter().map(|p| name(p)).collect();
    for (i, n) in names.iter().enumerate() {
        assert_image_default(n, &format!("-{}.jpg", i + 1));
    }
    let stem = |n: &str| n.split('-').nth(1).unwrap().to_string();
    assert_eq!(stem(names[0]), stem(names[2]));
    // Separate plans get distinct names, so a plan shows the generated part as its pattern.
    let again = plan_outputs(&image_req(dir.path(), 1, None, None)).unwrap();
    assert_ne!(again.paths[0], plan_outputs(&image_req(dir.path(), 1, None, None)).unwrap().paths[0]);
    let shown = |n: &str| dir.path().join(n).display().to_string();
    assert_eq!(
        plan.shown,
        [shown("iris-<ulid>-1.jpg"), shown("iris-<ulid>-2.jpg"), shown("iris-<ulid>-3.jpg")]
    );
    assert_eq!(again.shown, [shown("iris-<ulid>.png")]);
}

#[test]
fn video_names_use_the_job_id() {
    let dir = tempfile::tempdir().unwrap();
    let one = PathRequest {
        naming: Naming::Video { job_id: JOB },
        count: 1,
        output: None,
        dir: dir.path(),
        format: None,
        media_types: VIDEO_TYPES,
    };
    assert_eq!(plan_outputs(&one).unwrap().paths, vec![dir.path().join(format!("{JOB}.mp4"))]);
    let shown = |n: &str| dir.path().join(n).display().to_string();
    assert_eq!(plan_outputs(&one).unwrap().shown, [shown("<job_id>.mp4")]);
    let two = PathRequest { count: 2, ..one };
    assert_eq!(
        plan_outputs(&two).unwrap().paths,
        vec![dir.path().join(format!("{JOB}-1.mp4")), dir.path().join(format!("{JOB}-2.mp4"))]
    );
    assert_eq!(plan_outputs(&two).unwrap().shown, [shown("<job_id>-1.mp4"), shown("<job_id>-2.mp4")]);
    let bad = PathRequest { naming: Naming::Video { job_id: "../x" }, ..one };
    assert_eq!(plan_outputs(&bad).unwrap_err().code, ErrorCode::InternalError);
}

#[test]
fn relative_directories_become_absolute() {
    let plan = plan_outputs(&image_req(Path::new("some/rel/dir"), 1, None, None)).unwrap();
    let expected = std::env::current_dir().unwrap().join("some/rel/dir");
    assert!(plan.paths[0].is_absolute());
    assert_eq!(plan.paths[0].parent().unwrap(), expected);
}

#[test]
fn explicit_output_is_used_literally_and_indexed_for_several() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("cat.png");
    let plan = plan_outputs(&image_req(dir.path(), 1, Some(&out), None)).unwrap();
    assert_eq!(plan.paths, vec![out.clone()]);
    assert_eq!(plan.implied_format, Some("png"));

    let plan = plan_outputs(&image_req(Path::new("/unused"), 3, Some(&out), None)).unwrap();
    assert_eq!(
        plan.paths,
        vec![dir.path().join("cat-1.png"), dir.path().join("cat-2.png"), dir.path().join("cat-3.png")]
    );
    // Names given with -o are shown as they are.
    let shown: Vec<String> = plan.paths.iter().map(|p| p.display().to_string()).collect();
    assert_eq!(plan.shown, shown);

    let rel = plan_outputs(&image_req(dir.path(), 1, Some(Path::new("rel/out.webp")), None)).unwrap();
    assert_eq!(rel.paths, vec![std::env::current_dir().unwrap().join("rel/out.webp")]);
}

#[test]
fn output_extension_selects_or_must_match_the_format() {
    let dir = tempfile::tempdir().unwrap();
    let jpeg = dir.path().join("photo.JPEG");
    let plan = plan_outputs(&image_req(dir.path(), 1, Some(&jpeg), None)).unwrap();
    assert_eq!(plan.media_type, "image/jpeg");
    assert_eq!(plan.implied_format, Some("jpeg"));
    assert_eq!(plan.paths, vec![jpeg.clone()], "user's spelling is kept");

    // Consistent explicit format: fine, nothing implied.
    let plan = plan_outputs(&image_req(dir.path(), 1, Some(&jpeg), Some("jpeg"))).unwrap();
    assert_eq!(plan.implied_format, None);

    // Contradiction.
    let err = plan_outputs(&image_req(dir.path(), 1, Some(&jpeg), Some("png"))).unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidArgument);
    assert!(err.message.contains("contradicts the requested output format png"), "{}", err.message);

    // An extension the model cannot produce, or no media extension at all.
    let err = plan_outputs(&image_req(dir.path(), 1, Some(&dir.path().join("x.mp4")), None)).unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidArgument);
    assert!(err.message.contains(".png, .jpg, .jpeg, .webp"), "{}", err.message);
    let err = plan_outputs(&image_req(dir.path(), 1, Some(&dir.path().join("x.txt")), None)).unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidArgument);

    // Unknown or unproducible --format.
    assert_eq!(
        plan_outputs(&image_req(dir.path(), 1, None, Some("bmp"))).unwrap_err().code,
        ErrorCode::InvalidArgument
    );
    let png_only =
        PathRequest { media_types: &["image/png"], ..image_req(dir.path(), 1, None, Some("webp")) };
    assert_eq!(plan_outputs(&png_only).unwrap_err().code, ErrorCode::InvalidArgument);
}

#[test]
fn output_without_extension_gets_one_with_a_warning() {
    let dir = tempfile::tempdir().unwrap();
    let plan = plan_outputs(&image_req(dir.path(), 1, Some(&dir.path().join("cat")), Some("webp"))).unwrap();
    assert_eq!(plan.paths, vec![dir.path().join("cat.webp")]);
    assert_eq!(plan.warnings.len(), 1);
    assert_eq!(plan.warnings[0].code, "output_extension_adjusted");
    let shown = |p: PathBuf| p.display().to_string();
    assert!(plan.warnings[0].message.ends_with(&format!("saving as {}", shown(dir.path().join("cat.webp")))));

    // With several outputs the warning names the files actually written.
    let plan = plan_outputs(&image_req(dir.path(), 2, Some(&dir.path().join("cat")), None)).unwrap();
    assert_eq!(plan.paths, vec![dir.path().join("cat-1.png"), dir.path().join("cat-2.png")]);
    let message = &plan.warnings[0].message;
    assert!(
        message.ends_with(&format!(
            "saving as {} and {}",
            shown(dir.path().join("cat-1.png")),
            shown(dir.path().join("cat-2.png"))
        )),
        "{message}"
    );
    let plan = plan_outputs(&image_req(dir.path(), 4, Some(&dir.path().join("cat")), None)).unwrap();
    let message = &plan.warnings[0].message;
    assert!(
        message.ends_with(&format!(
            "saving as the 4 outputs {} … {}",
            shown(dir.path().join("cat-1.png")),
            shown(dir.path().join("cat-4.png"))
        )),
        "{message}"
    );
    assert!(!message.contains(&format!("{} ", shown(dir.path().join("cat.png")))), "{message}");
}

#[test]
fn planned_paths_are_lexically_normalized() {
    let dir = tempfile::tempdir().unwrap();
    let work = dir.path().join("work");
    let up = work.join("..").join("x.png");
    let plan = plan_outputs(&image_req(dir.path(), 1, Some(&up), None)).unwrap();
    assert_eq!(plan.paths, vec![dir.path().join("x.png")]);

    let odd_dir = work.join(".").join("a").join("..").join("renders");
    let plan = plan_outputs(&PathRequest {
        naming: Naming::Video { job_id: "job_01jbz9k3m4n5p6q7r8s9t0v1w2" },
        count: 1,
        output: None,
        dir: &odd_dir,
        format: None,
        media_types: &["video/mp4"],
    })
    .unwrap();
    assert_eq!(plan.paths, vec![work.join("renders").join("job_01jbz9k3m4n5p6q7r8s9t0v1w2.mp4")]);
    assert_eq!(paths::normalize_lexically(Path::new("/a/b/../../../c")), Path::new("/c"));
}

#[test]
fn output_naming_a_directory_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let err = plan_outputs(&image_req(dir.path(), 1, Some(dir.path()), None)).unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidArgument);
    assert!(err.message.contains("-d/--out-dir"));
    let slash = PathBuf::from(format!("{}/newdir/", dir.path().display()));
    assert_eq!(
        plan_outputs(&image_req(dir.path(), 1, Some(&slash), None)).unwrap_err().code,
        ErrorCode::InvalidArgument
    );
}

/// Iris writes files and prints their paths: `-o -` (standard output), a name of a
/// standard stream or file descriptor, and an existing device are refused with a
/// hint saying so, for images and videos alike.
#[test]
fn output_to_standard_output_or_a_device_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let mut targets = vec![PathBuf::from("-")];
    if cfg!(unix) {
        // Refused by name, whatever the stream is when the tests run (a terminal, a
        // pipe, or a regular file standard output is redirected to).
        for stream in [
            "/dev/stdout",
            "/dev/stderr",
            "/dev/stdin",
            "/dev/fd/1",
            "/dev/./stdout",
            "/dev/fd/../stdout",
            "/proc/self/fd/1",
            "/proc/thread-self/fd/2",
            "/proc/1/task/1/fd/1",
        ] {
            targets.push(PathBuf::from(stream));
        }
        targets.push(PathBuf::from("/dev/null"));
    }
    for output in &targets {
        for req in [
            image_req(dir.path(), 1, Some(output), None),
            PathRequest {
                naming: Naming::Video { job_id: JOB },
                count: 1,
                output: Some(output),
                dir: dir.path(),
                format: None,
                media_types: VIDEO_TYPES,
            },
        ] {
            let err = plan_outputs(&req).unwrap_err();
            assert_eq!(err.code, ErrorCode::InvalidArgument, "{}", output.display());
            assert!(err.hint.as_deref().unwrap().contains("prints their paths"), "{:?}", err.hint);
            if output != Path::new("-") {
                let named = paths::normalize_lexically(output);
                assert_eq!(err.details["path"], named.to_str().unwrap(), "{}", output.display());
            }
        }
    }
    if cfg!(unix) {
        let err = plan_outputs(&image_req(dir.path(), 1, Some(Path::new("/dev/stdout")), None)).unwrap_err();
        assert!(err.message.contains("standard stream"), "{}", err.message);
    }
}

/// Only `-` itself means standard output: `./-` and `dir/-` are the usual ways to
/// name a file called `-`, and other names under `/dev` are ordinary paths.
#[test]
fn a_file_named_dash_or_under_dev_is_an_ordinary_output() {
    let dir = tempfile::tempdir().unwrap();
    let dash = dir.path().join("-");
    let plan = plan_outputs(&image_req(dir.path(), 1, Some(&dash), None)).unwrap();
    assert_eq!(plan.paths, vec![dir.path().join("-.png")]);
    assert!(!dash.exists() && !dir.path().join("-.png").exists(), "planning writes nothing");
    let plan = plan_outputs(&image_req(dir.path(), 1, Some(Path::new("./-")), None)).unwrap();
    assert_eq!(plan.paths, vec![std::env::current_dir().unwrap().join("-.png")]);
    assert!(plan_outputs(&image_req(dir.path(), 1, Some(&dir.path().join("a-b.png")), None)).is_ok());
    if cfg!(unix) {
        let shm = Path::new("/dev/shm/iris-plan-test-7f3a.png");
        assert_eq!(plan_outputs(&image_req(dir.path(), 1, Some(shm), None)).unwrap().paths, vec![shm]);
        let named = Path::new("/dev/stdout.png");
        assert_eq!(plan_outputs(&image_req(dir.path(), 1, Some(named), None)).unwrap().paths, vec![named]);
    }
}

#[test]
fn preflight_refuses_existing_files_unless_overwrite() {
    let dir = tempfile::tempdir().unwrap();
    let existing = dir.path().join("a.png");
    std::fs::write(&existing, b"old").unwrap();
    let fresh = dir.path().join("b.png");
    let paths = vec![fresh.clone(), existing.clone()];

    let err = preflight(&paths, false).unwrap_err();
    assert_eq!(err.code, ErrorCode::OutputExists);
    assert_eq!(err.exit_code(), 2);
    assert_eq!(err.details["path"], existing.to_str().unwrap());
    assert!(err.hint.as_deref().unwrap().contains("--overwrite"));

    preflight(&paths, true).unwrap();
    preflight(std::slice::from_ref(&fresh), false).unwrap();

    let err = preflight(&[dir.path().to_path_buf()], true).unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidArgument);

    #[cfg(unix)]
    {
        // A dangling symlink still occupies the name.
        let link = dir.path().join("link.png");
        std::os::unix::fs::symlink(dir.path().join("nowhere"), &link).unwrap();
        assert_eq!(preflight(&[link], false).unwrap_err().code, ErrorCode::OutputExists);
    }
    assert_eq!(std::fs::read(&existing).unwrap(), b"old");
}

/// When the provider chooses the image type, each planned path may be saved under
/// the extension of another declared type: without `--overwrite`, a file under any
/// of those names is `output_exists` naming it (for every one of several outputs),
/// while the named extension's own variants (`.JPG`, `.jpeg`) are the path itself.
#[test]
fn preflight_refuses_a_file_under_another_extension_the_provider_may_choose() {
    let dir = tempfile::tempdir().unwrap();
    let types = &["image/jpeg", "image/png"];
    let plan = |output: &str, count: u32| {
        let output = dir.path().join(output);
        let req = PathRequest {
            naming: Naming::Image,
            count,
            output: Some(&output),
            dir: dir.path(),
            format: None,
            media_types: types,
        };
        plan_outputs(&req).unwrap().paths
    };
    std::fs::write(dir.path().join("photo.png"), b"earlier").unwrap();
    std::fs::write(dir.path().join("p-2.jpg"), b"earlier").unwrap();

    for (output, count, existing) in [
        ("photo.jpg", 1, "photo.png"),
        ("photo.JPEG", 1, "photo.png"),
        ("photo", 1, "photo.png"),
        ("p.png", 3, "p-2.jpg"),
    ] {
        let paths = plan(output, count);
        preflight(&paths, false).unwrap();
        let err = preflight_other_types(&paths, types, false).unwrap_err();
        assert_eq!(err.code, ErrorCode::OutputExists, "{output}");
        assert_eq!(err.exit_code(), 2);
        let existing = dir.path().join(existing);
        assert_eq!(err.details["path"], existing.to_str().unwrap(), "{output}");
        assert!(err.message.contains(existing.to_str().unwrap()), "{}", err.message);
        let hint = err.hint.as_deref().unwrap();
        assert!(hint.contains("never replaces") && hint.contains("output_renamed"), "{hint}");
        // With --overwrite the run goes ahead; the file is never replaced when saving.
        preflight_other_types(&paths, types, true).unwrap();
    }
    for (output, count) in [("photo.png", 1), ("other.jpg", 1), ("p.jpg", 1), ("q.png", 3)] {
        preflight_other_types(&plan(output, count), types, false).unwrap();
    }
    // A model with one output type has no other name to check.
    preflight_other_types(&plan("photo.jpg", 1), &["image/jpeg"], false).unwrap();
    assert_eq!(std::fs::read(dir.path().join("photo.png")).unwrap(), b"earlier");
}

/// Entries of `dir`, asserting no preflight or part files were left behind.
fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert!(!names.iter().any(|n| n.contains(".iris-part-")), "temp files left behind: {names:?}");
    names
}

#[test]
fn preflight_dirs_creates_missing_directories_and_proves_them_writable() {
    let dir = tempfile::tempdir().unwrap();
    let deep = dir.path().join("renders").join("today");
    let paths = vec![deep.join("a-1.png"), deep.join("a-2.png"), dir.path().join("b.png")];
    preflight_dirs(&paths, true).unwrap();
    assert!(deep.is_dir(), "-d is created if missing");
    assert!(entries(&deep).is_empty());
    assert_eq!(entries(dir.path()), vec!["renders"]);

    // --dry-run checks without creating a directory or leaving anything behind.
    let planned = dir.path().join("later").join("x.png");
    preflight_dirs(std::slice::from_ref(&planned), false).unwrap();
    assert!(!dir.path().join("later").exists());
    preflight_dirs(&[deep.join("c.png")], false).unwrap();
    assert!(entries(&deep).is_empty());
    assert_eq!(entries(dir.path()), vec!["renders"]);
}

#[test]
fn preflight_dirs_rejects_a_file_in_the_way_before_anything_is_sent() {
    let dir = tempfile::tempdir().unwrap();
    let blocker = dir.path().join("out");
    std::fs::write(&blocker, b"a file, not a directory").unwrap();
    for path in [blocker.join("cat.png"), blocker.join("sub").join("deeper").join("cat.png")] {
        for create in [true, false] {
            let err = preflight_dirs(std::slice::from_ref(&path), create).unwrap_err();
            assert_eq!(err.code, ErrorCode::InvalidArgument, "{path:?}");
            assert_eq!(err.exit_code(), 2);
            assert!(err.message.contains("not a directory"), "{}", err.message);
            assert_eq!(err.details["path"], blocker.to_str().unwrap());
        }
    }
    assert_eq!(std::fs::read(&blocker).unwrap(), b"a file, not a directory");

    #[cfg(unix)]
    {
        let link = dir.path().join("gone");
        std::os::unix::fs::symlink(dir.path().join("nowhere"), &link).unwrap();
        let err = preflight_dirs(&[link.join("cat.png")], true).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidArgument);
        assert!(err.message.contains("broken symbolic link"), "{}", err.message);
    }
}

/// A location that cannot be used as given is a request to fix (exit 2), like a file
/// in the way: nothing was sent, and retrying the same path cannot succeed.
#[cfg(target_os = "linux")]
#[test]
fn preflight_dirs_reports_uncreatable_and_unwritable_directories_as_invalid_arguments() {
    // procfs refuses new entries even for root, so this works in any sandbox.
    if !Path::new("/proc/self").exists() {
        return;
    }
    let uncreatable = PathBuf::from("/proc/iris-preflight-test/cat.png");
    let err = preflight_dirs(std::slice::from_ref(&uncreatable), true).unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidArgument);
    assert_eq!(err.exit_code(), 2);
    assert!(
        err.message.contains("cannot create output directory /proc/iris-preflight-test"),
        "{}",
        err.message
    );
    assert!(err.hint.as_deref().unwrap().contains("-d/--out-dir"));
    assert_eq!(err.details["path"], "/proc/iris-preflight-test");

    let unwritable = PathBuf::from("/proc/cat.png");
    let err = preflight_dirs(std::slice::from_ref(&unwritable), true).unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidArgument);
    // The file system's own reason, and the directory, not the check file's random name.
    assert_eq!(err.message, "output directory /proc is not writable: No such file or directory (os error 2)");
    assert_eq!(err.details["path"], "/proc");

    // Without creating a directory (a dry run), the same locations are refused: the
    // nearest existing directory, where the real run would create or write, is
    // proven writable instead.
    let err = preflight_dirs(std::slice::from_ref(&uncreatable), false).unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidArgument);
    assert_eq!(
        err.message,
        "cannot create output directory /proc/iris-preflight-test: /proc is not writable: No such file or \
         directory (os error 2)"
    );
    assert_eq!(err.details["path"], "/proc/iris-preflight-test");
    let err = preflight_dirs(std::slice::from_ref(&unwritable), false).unwrap_err();
    assert_eq!(err.message, "output directory /proc is not writable: No such file or directory (os error 2)");
    assert_eq!(err.details["path"], "/proc");
}

#[test]
fn preflight_names_the_file_in_the_way_of_an_output_path() {
    let dir = tempfile::tempdir().unwrap();
    let blocker = dir.path().join("afile");
    std::fs::write(&blocker, b"x").unwrap();
    for path in [blocker.join("x.png"), blocker.join("sub").join("x.png")] {
        let err = preflight(std::slice::from_ref(&path), false).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidArgument, "{path:?}");
        assert_eq!(err.details["path"], blocker.to_str().unwrap(), "{path:?}");
        assert!(err.message.contains("not a directory"), "{}", err.message);
    }
}

#[test]
fn extension_adjustment_for_provider_media_types() {
    let (p, w) = adjust_extension(Path::new("/o/cat.png"), "image/jpeg");
    assert_eq!(p, Path::new("/o/cat.jpg"));
    assert_eq!(w.unwrap().code, "output_extension_adjusted");
    let (p, w) = adjust_extension(Path::new("/o/cat.jpeg"), "image/jpeg");
    assert_eq!(p, Path::new("/o/cat.jpeg"));
    assert!(w.is_none());
    let (p, w) = adjust_extension(Path::new("/o/job.mp4"), "video/quicktime");
    assert_eq!(p, Path::new("/o/job.mov"));
    assert!(w.is_some());
    let (p, w) = adjust_extension(Path::new("/o/x.bin"), "application/octet-stream");
    assert_eq!(p, Path::new("/o/x.bin"));
    assert!(w.is_none());
}

#[test]
fn numbered_fallback_names() {
    assert_eq!(paths::numbered(Path::new("/o/cat.png"), 1), Path::new("/o/cat.1.png"));
    assert_eq!(paths::numbered(Path::new("/o/cat-2.png"), 12), Path::new("/o/cat-2.12.png"));
    assert_eq!(paths::numbered(Path::new("/o/cat"), 3), Path::new("/o/cat.3"));
}

#[test]
fn format_media_type_mapping() {
    assert_eq!(paths::format_for_media_type("image/jpeg"), Some("jpeg"));
    assert_eq!(paths::format_for_media_type("image/webp"), Some("webp"));
    assert_eq!(paths::format_for_media_type("video/mp4"), None);
    assert_eq!(paths::media_type_for_format("png"), Some("image/png"));
    assert_eq!(paths::media_type_for_format("gif"), None);
}

#[test]
fn zero_count_or_missing_output_types_are_internal_errors() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        plan_outputs(&image_req(dir.path(), 0, None, None)).unwrap_err().code,
        ErrorCode::InternalError
    );
    let none = PathRequest { media_types: &[], ..image_req(dir.path(), 1, None, None) };
    assert_eq!(plan_outputs(&none).unwrap_err().code, ErrorCode::InternalError);
}
