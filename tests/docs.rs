//! The documentation's links, and the house rules of docs/contributing/style-guide.md
//! that a program can check: every relative link and anchor resolves, the files that
//! release archives ship link outside the archive only with absolute URLs, and prose has
//! no Latin abbreviations, spaced em dashes, or "answer" for a provider's response.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

/// Links to files of this repository use this prefix, then `blob/main/` or `tree/main/`.
const REPO: &str = "https://github.com/doodla/iris/";

/// Files exempt from the prose rules: the style guide names the words it bans, the CLI
/// reference is the help text as generated, and the agent instruction files keep their
/// own terse style.
const PROSE_EXEMPT: [&str; 4] =
    ["docs/contributing/style-guide.md", "docs/reference/cli.md", "AGENTS.md", "CLAUDE.md"];

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(root().join(path)).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Every Markdown file of the repository, relative to its root, except build output.
fn markdown_files() -> Vec<PathBuf> {
    fn walk(dir: &Path, relative: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.file_type().unwrap().is_dir() {
                if name != "target" && name != ".git" {
                    walk(&entry.path(), &relative.join(&name), out);
                }
            } else if name.ends_with(".md") {
                out.push(relative.join(&name));
            }
        }
    }
    let mut out = Vec::new();
    walk(&root(), Path::new(""), &mut out);
    out.sort();
    assert!(out.len() > 20, "found the Markdown files: {out:?}");
    out
}

/// The lines outside fenced code blocks, numbered from 1.
fn text_lines(text: &str) -> Vec<(usize, &str)> {
    let mut fence: Option<String> = None;
    let mut out = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        let marker: String = trimmed.chars().take_while(|c| *c == '`' || *c == '~').collect();
        if marker.len() >= 3 {
            match &fence {
                None => fence = Some(marker),
                Some(open) if trimmed.starts_with(open.as_str()) => fence = None,
                Some(_) => {}
            }
        } else if fence.is_none() {
            out.push((index + 1, line));
        }
    }
    out
}

/// A line without its inline code spans.
fn without_code(line: &str) -> String {
    let mut out = String::new();
    let mut rest = line;
    while let Some(start) = rest.find('`') {
        let Some(end) = rest[start + 1..].find('`') else { break };
        out.push_str(&rest[..start]);
        rest = &rest[start + 1 + end + 1..];
    }
    out.push_str(rest);
    out
}

/// The destinations of the inline links and images on a line.
fn link_destinations(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = line;
    while let Some(at) = rest.find("](") {
        let after = &rest[at + 2..];
        let end = after.find(|c: char| c == ')' || c.is_whitespace()).unwrap_or(after.len());
        out.push(after[..end].to_string());
        rest = &after[end..];
    }
    out
}

/// The anchors that GitHub gives the headings of a Markdown file: lowercase, spaces as
/// hyphens, punctuation other than `-` and `_` removed, and `-1`, `-2`, ... on repeats.
fn anchors(text: &str) -> BTreeSet<String> {
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut out = BTreeSet::new();
    for (_, line) in text_lines(text) {
        let level = line.chars().take_while(|c| *c == '#').count();
        let Some(heading) = line.get(level..).and_then(|rest| rest.strip_prefix(' ')) else { continue };
        if !(1..=6).contains(&level) {
            continue;
        }
        let slug: String = heading
            .trim()
            .to_lowercase()
            .chars()
            .filter_map(|c| match c {
                ' ' => Some('-'),
                '-' | '_' => Some(c),
                c if c.is_alphanumeric() => Some(c),
                _ => None,
            })
            .collect();
        let count = seen.entry(slug.clone()).or_insert(0);
        out.insert(if *count == 0 { slug } else { format!("{slug}-{count}") });
        *count += 1;
    }
    out
}

/// Whether release archives ship this file (docs/contributing/releasing.md).
fn archived(path: &Path) -> bool {
    path == Path::new("README.md")
        || path == Path::new("CHANGELOG.md")
        || path == Path::new("LICENSE")
        || path.starts_with("docs")
}

/// `path` without `.` and `..` components, or `None` if it leaves the repository.
fn normalize(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            Component::Normal(part) => out.push(part),
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(out)
}

/// What is wrong with a link in `file`, if anything.
fn link_problem(file: &Path, destination: &str) -> Option<String> {
    let (path, fragment) = match destination.split_once('#') {
        Some((path, fragment)) => (path, Some(fragment)),
        None => (destination, None),
    };
    let target = if let Some(rest) = path.strip_prefix(REPO) {
        let file_path = rest.strip_prefix("blob/main/").or_else(|| rest.strip_prefix("tree/main/"))?;
        PathBuf::from(file_path)
    } else if path.contains(':') {
        return None;
    } else if path.is_empty() {
        file.to_path_buf()
    } else {
        let Some(target) = normalize(&file.parent().unwrap().join(path)) else {
            return Some("points outside the repository".to_string());
        };
        if archived(file) && !archived(&target) {
            return Some(format!(
                "release archives don't ship {}; link to it with an absolute {REPO}blob/main/ URL",
                target.display()
            ));
        }
        target
    };
    if !root().join(&target).exists() {
        return Some(format!("{} doesn't exist", target.display()));
    }
    match fragment {
        Some(fragment) if target.extension().is_some_and(|extension| extension == "md") => {
            (!anchors(&read(&target)).contains(fragment))
                .then(|| format!("{} has no heading with the anchor #{fragment}", target.display()))
        }
        _ => None,
    }
}

#[test]
fn every_link_and_anchor_resolves() {
    let mut problems = Vec::new();
    for file in markdown_files() {
        let text = read(&file);
        for (number, line) in text_lines(&text) {
            for destination in link_destinations(&without_code(line)) {
                if let Some(problem) = link_problem(&file, &destination) {
                    problems.push(format!("{}:{number}: ({destination}) {problem}", file.display()));
                }
            }
        }
    }
    assert!(problems.is_empty(), "broken links:\n{}", problems.join("\n"));
}

/// Whether `word` occurs in `text` as a whole word, with one of the optional suffixes.
fn has_word(text: &str, word: &str, suffixes: &[&str]) -> bool {
    text.match_indices(word).any(|(at, _)| {
        let before = text[..at].chars().next_back();
        let after = &text[at + word.len()..];
        let rest =
            suffixes.iter().find(|suffix| after.starts_with(**suffix)).map_or(after, |s| &after[s.len()..]);
        !before.is_some_and(char::is_alphanumeric) && !rest.chars().next().is_some_and(char::is_alphanumeric)
    })
}

#[test]
fn prose_follows_the_style_guide() {
    let mut problems = Vec::new();
    for file in markdown_files() {
        if PROSE_EXEMPT.iter().any(|exempt| file == Path::new(exempt)) {
            continue;
        }
        let text = read(&file);
        for (number, line) in text_lines(&text) {
            let prose = without_code(line);
            let lower = prose.to_lowercase();
            let mut report =
                |rule: &str| problems.push(format!("{}:{number}: {rule}: {}", file.display(), line.trim()));
            for latin in ["e.g.", "i.e.", "etc."] {
                if has_word(&lower, latin, &[]) {
                    report("a Latin abbreviation (write \"for example\", \"that is\", or a complete list)");
                }
            }
            if prose.contains(" — ") {
                report("a spaced em dash (write it without spaces, or use two sentences)");
            }
            if has_word(&lower, "answer", &["ing", "ed", "s"]) {
                report("\"answer\" (a provider sends a response)");
            }
        }
    }
    assert!(problems.is_empty(), "style guide violations:\n{}", problems.join("\n"));
}

/// The checks catch what they are meant to catch.
#[test]
fn the_checks_catch_problems() {
    assert!(link_problem(Path::new("README.md"), "docs/missing.md").is_some());
    assert!(link_problem(Path::new("README.md"), "docs/README.md#no-such-heading").is_some());
    assert!(link_problem(Path::new("README.md"), "CONTRIBUTING.md").is_some(), "outside the archive");
    assert!(link_problem(Path::new("CONTRIBUTING.md"), "SECURITY.md").is_none());
    assert!(
        link_problem(Path::new("README.md"), "https://github.com/doodla/iris/blob/main/nope.md").is_some()
    );
    assert!(link_problem(Path::new("docs/reference/errors.md"), "#label_in_use").is_none());
    assert!(link_problem(Path::new("docs/guides/videos.md"), "../../../elsewhere.md").is_some());
    assert!(has_word("the provider answered", "answer", &["ing", "ed", "s"]));
    assert!(!has_word("fetch.", "etc.", &[]));
    assert!(has_word("tools, etc.", "etc.", &[]));
    assert_eq!(without_code("run `a` and `b` now"), "run  and  now");
    let headings =
        anchors("# A b\n## Step 1: Do it, now\n### `label_in_use`\n## A b\n```\n# not a heading\n```\n");
    let expected: BTreeSet<String> =
        ["a-b", "step-1-do-it-now", "label_in_use", "a-b-1"].into_iter().map(String::from).collect();
    assert_eq!(headings, expected);
}
