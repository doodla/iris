//! Output path planning (see `iris --help` and docs/concepts/video-jobs.md).
//!
//! * The output directory precedence (`-d` > `IRIS_OUTPUT_DIR` > config > cwd) is
//!   resolved by the caller and passed in; planned paths are absolute and lexically
//!   normalized (`.` and `..` components removed without resolving symbolic links),
//!   and the real write uses exactly the planned path.
//! * Default names: images `iris-<ulid>.<ext>` (`iris-<ulid>-<i>.<ext>` when several),
//!   videos `<job_id>.<ext>` (`<job_id>-<i>.<ext>`), `i` starting at 1. Generated
//!   names use only `[a-z0-9_.-]`; remote data never contributes to local names.
//!   Each plan generates a fresh ULID (and a video plan uses the job's new id), so a
//!   `--dry-run` shows a generated name as its pattern (`iris-<ulid>.png`,
//!   `<job_id>.mp4`: [`PlannedOutputs::shown`]); the real run generates its own.
//! * `-o PATH` is used literally; with several artifacts it becomes
//!   `<stem>-<i>.<ext>`. Its extension must agree with `--format` and with what
//!   the model can produce; without `--format` it selects the format. It must name
//!   a regular file: `-o -` (standard output), a name of a standard stream or file
//!   descriptor such as `/dev/stdout`, and an existing device, pipe, or socket are
//!   `invalid_argument`, because Iris saves media to files and prints their paths.
//! * [`preflight`] refuses existing files before any paid request unless
//!   `--overwrite`; [`preflight_dirs`] makes sure the output directories exist (or
//!   can be created) and are writable, so a paid result is never lost because it
//!   cannot be saved. An output location that cannot be used as given (a file in
//!   the way, no permission, a read-only file system, a parent that cannot be
//!   created) is `invalid_argument` with `details.path`: nothing was sent, and only
//!   choosing another location fixes it.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use crate::domain::{Warning, WarningCode};
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
    /// `-o/--output`, used literally (relative paths resolve against the cwd); `-`
    /// itself means standard output and is refused.
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
    /// The paths as a dry run shows them: `paths`, except that a name the real run
    /// generates is shown as its pattern (`iris-<ulid>.png`, `<job_id>.mp4`, with
    /// `-<i>` when there are several).
    pub shown: Vec<String>,
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
                    .with_detail("option", "format")
            })?;
            if !media::accepts(req.media_types, t) {
                return Err(IrisError::invalid(format!(
                    "this model cannot produce {f} output (it produces {})",
                    req.media_types.join(", ")
                ))
                .with_detail("option", "format"));
            }
            Some(t)
        }
        None => None,
    };

    let mut warnings = Vec::new();
    let mut implied_format = None;
    let (media_type, paths, shown) = match req.output {
        Some(output) => {
            let names_directory = output.as_os_str().to_string_lossy().ends_with(std::path::MAIN_SEPARATOR);
            let output = resolve_file_target(output)?;
            let path = output.to_string_lossy().into_owned();
            if names_directory || output.is_dir() {
                return Err(IrisError::invalid(format!(
                    "-o/--output expects a file path, but {} is a directory; use -d/--out-dir for directories",
                    output.display()
                ))
                .with_detail("path", path));
            }
            let ext = output.extension().map(|e| e.to_string_lossy().into_owned());
            let given = output.display().to_string();
            let mut extension_added = false;
            let (media_type, base) = match ext {
                None => {
                    let t = format_type.unwrap_or(default_type);
                    let ext = media::extension_for(t).unwrap_or("bin");
                    let mut name = output.file_name().unwrap_or_default().to_os_string();
                    name.push(format!(".{ext}"));
                    extension_added = true;
                    (t, output.with_file_name(name))
                }
                Some(ext) => {
                    let ext_type = media::media_type_for_extension(&ext).ok_or_else(|| {
                        IrisError::invalid(format!(
                            "unrecognized output extension '.{ext}' in {}; use one of {}",
                            output.display(),
                            extension_list(req.media_types)
                        ))
                        .with_detail("path", path.clone())
                    })?;
                    if let Some(t) = format_type {
                        if !same_type(t, ext_type) {
                            return Err(IrisError::invalid(format!(
                                "-o/--output extension '.{ext}' contradicts the requested output format {}",
                                req.format.unwrap_or_default()
                            ))
                            .with_detail("path", path)
                            .with_detail("option", "format"));
                        }
                    } else if !media::accepts(req.media_types, ext_type) {
                        return Err(IrisError::invalid(format!(
                            "this model cannot produce {ext_type} ('.{ext}'); use one of {}",
                            extension_list(req.media_types)
                        ))
                        .with_detail("path", path));
                    } else {
                        implied_format = format_for_media_type(ext_type);
                    }
                    (ext_type, output)
                }
            };
            let paths: Vec<PathBuf> = if req.count == 1 {
                vec![base]
            } else {
                (1..=req.count).map(|i| indexed(&base, i)).collect()
            };
            if extension_added {
                let saving_as = match paths.as_slice() {
                    [one] => one.display().to_string(),
                    [first, last] => format!("{} and {}", first.display(), last.display()),
                    [first, .., last] => {
                        format!("the {} outputs {} … {}", paths.len(), first.display(), last.display())
                    }
                    [] => String::new(),
                };
                warnings.push(Warning::new(
                    WarningCode::OutputExtensionAdjusted,
                    format!("{given} has no extension; saving as {saving_as}"),
                ));
            }
            let shown = paths.iter().map(|p| p.display().to_string()).collect();
            (media_type, paths, shown)
        }
        None => {
            let media_type = format_type.unwrap_or(default_type);
            let ext = media::extension_for(media_type).unwrap_or("bin");
            let dir = absolute(req.dir)?;
            // The generated part of the name, and its pattern.
            let (stem, pattern) = match req.naming {
                Naming::Image => (
                    format!("iris-{}", ulid::Ulid::generate().to_string().to_ascii_lowercase()),
                    "iris-<ulid>",
                ),
                Naming::Video { job_id } => {
                    if job_id.is_empty()
                        || !job_id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
                    {
                        return Err(IrisError::internal(format!(
                            "invalid job id for a file name: {job_id:?}"
                        )));
                    }
                    (job_id.to_string(), "<job_id>")
                }
            };
            let names = |stem: &str| -> Vec<PathBuf> {
                if req.count == 1 {
                    vec![dir.join(format!("{stem}.{ext}"))]
                } else {
                    (1..=req.count).map(|i| dir.join(format!("{stem}-{i}.{ext}"))).collect()
                }
            };
            let shown = names(pattern).iter().map(|p| p.display().to_string()).collect();
            (media_type, names(&stem), shown)
        }
    };
    Ok(PlannedOutputs { paths, shown, media_type, implied_format, warnings })
}

/// What to do instead of writing media to standard output or a device.
const FILES_HINT: &str = "Iris saves media to files and prints their paths (result.artifacts[].path with --json); \
                          give a file path with -o/--output, or a directory with -d/--out-dir";

/// The absolute, lexically normalized path `-o` names, once it is known to name a
/// file Iris can create or replace. Refused with `invalid_argument` and a hint that
/// Iris writes files and prints their paths (paid output is never streamed or
/// discarded):
///
/// * `-` itself, which would mean standard output (`./-` or `dir/-` name a file
///   called `-`, as usual);
/// * the name of a standard stream or an open file descriptor ([`names_a_stream`]),
///   whatever it currently resolves to: with standard output redirected to a file,
///   `/dev/stdout` is that file, so checking what it is would not be enough;
/// * an existing device, pipe, or socket, such as `/dev/null`.
fn resolve_file_target(output: &Path) -> Result<PathBuf, IrisError> {
    if output.as_os_str() == "-" {
        return Err(IrisError::invalid(
            "-o/--output - would mean standard output, but Iris writes media only to files",
        )
        .with_hint(FILES_HINT));
    }
    let resolved = absolute(output)?;
    let refuse = |what: &str| {
        IrisError::invalid(format!(
            "-o/--output {} {what}; Iris writes media only to files",
            resolved.display()
        ))
        .with_detail("path", resolved.to_string_lossy().into_owned())
        .with_hint(FILES_HINT)
    };
    if names_a_stream(&resolved) {
        return Err(refuse("names a standard stream or file descriptor, not a file"));
    }
    if let Ok(meta) = fs::metadata(&resolved)
        && !meta.is_file()
        && !meta.is_dir()
    {
        return Err(refuse("is not a regular file (a device, pipe, or socket)"));
    }
    Ok(resolved)
}

/// Whether `path` (absolute, lexically normalized) is, on Unix, a name of a
/// standard stream or an open file descriptor: `/dev/stdin`, `/dev/stdout`,
/// `/dev/stderr`, anything under `/dev/fd`, or anything under a process's `fd`
/// directory in `/proc` (`/proc/self/fd/1`, `/proc/<pid>/task/<tid>/fd/1`, …).
/// Other names under `/dev` (e.g. `/dev/shm/…`, which holds regular files) are not
/// refused by name.
fn names_a_stream(path: &Path) -> bool {
    if !cfg!(unix) {
        return false;
    }
    let mut parts = path.components();
    if parts.next() != Some(Component::RootDir) {
        return false;
    }
    let mut names = parts.map(Component::as_os_str);
    match (names.next(), names.next()) {
        (Some(top), Some(name)) if top == "dev" => {
            ["stdin", "stdout", "stderr", "fd"].iter().any(|s| name == *s)
        }
        (Some(top), Some(_)) if top == "proc" => names.any(|name| name == "fd"),
        _ => false,
    }
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

/// Check the other names planned paths may be saved under when the provider
/// chooses the content's type among `media_types` (a model without a `format`
/// option): each path with the extension of another of those types, as
/// [`adjust_extension`] would give it. Without `overwrite`, an existing entry at any
/// of them is `output_exists` naming it, as for the path itself, so a run that may
/// save next to an earlier image under another extension is refused before it
/// pays again. With `overwrite` nothing is checked: the run goes ahead, and an
/// existing file under an adjusted name is never replaced (the output goes to
/// `<stem>.<n>.<ext>` with `output_renamed`).
pub fn preflight_other_types(
    paths: &[PathBuf],
    media_types: &[&str],
    overwrite: bool,
) -> Result<(), IrisError> {
    if overwrite {
        return Ok(());
    }
    for path in paths {
        for media_type in media_types {
            let (other, adjusted) = adjust_extension(path, media_type);
            if adjusted.is_some() && fs::symlink_metadata(&other).is_ok() {
                return Err(IrisError::new(
                    ErrorCode::OutputExists,
                    format!(
                        "output file {} already exists, and the provider chooses the image type: {} is saved \
                         there if it comes back as {media_type}",
                        other.display(),
                        path.display()
                    ),
                )
                .with_detail("path", other.to_string_lossy().into_owned())
                .with_hint(
                    "move the existing file away, or choose another -o/--output or -d/--out-dir; --overwrite lets \
                     the run go ahead but never replaces a file under another extension than the one named (such \
                     an image is saved under a numbered name, output_renamed)",
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
/// * with `create` (a real run, once the credential is known to be present) the
///   directory is created if missing and proven writable by creating a check file
///   in it and removing it at once;
/// * without `create` (`--dry-run`, and the checks a real run makes before it
///   knows the credential is present) no directory is created and nothing is left
///   behind: the nearest existing directory is proven writable the same way, since
///   that is where the real run creates the directory or writes the file.
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
        let existing = check_ancestors(dir)?;
        if create {
            fs::create_dir_all(dir).map_err(|e| {
                location_error(format_args!("cannot create output directory {}", dir.display()), dir, &e)
            })?;
            prove_writable(dir, dir)?;
        } else if let Some(existing) = existing {
            prove_writable(existing, dir)?;
        }
    }
    Ok(())
}

/// Prove `probe` writable, where the output directory `dir` is (`probe` itself) or
/// would be created (its nearest existing ancestor), by creating a check file
/// `.iris-preflight.iris-part-<ulid>` in it and removing it at once. Errors name
/// the directory (`details.path`: `dir`), not the check file, unless the check file
/// could not be removed (`details.path`: the check file).
/// Hand-rolled on `std::fs` rather than `tempfile`, whose errors name the random
/// file and whose drop ignores a failed removal.
fn prove_writable(probe: &Path, dir: &Path) -> Result<(), IrisError> {
    let unwritable = |e: io::Error| {
        let context = if probe == dir {
            format!("output directory {} is not writable", dir.display())
        } else {
            format!("cannot create output directory {}: {} is not writable", dir.display(), probe.display())
        };
        location_error(format_args!("{context}"), dir, &e)
    };
    let id = ulid::Ulid::generate().to_string().to_ascii_lowercase();
    let check = probe.join(format!(".iris-preflight.iris-part-{id}"));
    fs::OpenOptions::new().write(true).create_new(true).open(&check).map_err(unwritable)?;
    fs::remove_file(&check).map_err(|e| {
        location_error(
            format_args!(
                "cannot remove the check file {} Iris created in {}",
                check.display(),
                probe.display()
            ),
            &check,
            &e,
        )
    })
}

/// The nearest existing ancestor of `dir` (itself included) must be a directory;
/// returns it (`None` only for a relative `dir` none of whose ancestors exists).
fn check_ancestors(dir: &Path) -> Result<Option<&Path>, IrisError> {
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
            Ok(meta) if meta.is_dir() => return Ok(Some(ancestor)),
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
    Ok(None)
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
        WarningCode::OutputExtensionAdjusted,
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

/// `path` made absolute (against the current directory) and lexically normalized.
pub(crate) fn absolute(path: &Path) -> Result<PathBuf, IrisError> {
    let abs = std::path::absolute(path)
        .map_err(|e| IrisError::io(format_args!("cannot resolve path {}", path.display()), &e))?;
    let abs = normalize_lexically(&abs);
    if abs.to_str().is_none() {
        return Err(IrisError::invalid(format!("output path {} is not valid UTF-8", abs.to_string_lossy())));
    }
    Ok(abs)
}

/// Remove `.` and resolve `..` against the preceding component, without touching
/// the file system (symbolic links are not followed; `..` at the root stays at the
/// root). Planned paths are shown and used in this form.
pub fn normalize_lexically(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    out.pop();
                } else if !out.has_root() {
                    out.push(component.as_os_str());
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
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
    fn lexical_normalization_removes_dot_and_dot_dot() {
        for (raw, normalized) in [
            ("/w/../x.png", "/x.png"),
            ("/w/a/./b/../c.png", "/w/a/c.png"),
            ("/../../x.png", "/x.png"),
            ("/w/a/b/../../c.png", "/w/c.png"),
            ("/w", "/w"),
            ("../x", "../x"),
            ("a/../../x", "../x"),
        ] {
            assert_eq!(normalize_lexically(Path::new(raw)), Path::new(normalized), "{raw}");
        }
    }

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
