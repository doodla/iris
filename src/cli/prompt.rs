//! Prompt sources (C-02): exactly one of the positional PROMPT, `--prompt-file`,
//! or `--prompt-stdin`. File and stdin content has trailing whitespace trimmed;
//! interior content is kept as is. Prompt text is never logged.

use std::io::Read;
use std::path::Path;

use crate::error::{ErrorCode, IrisError};

use super::args::PromptArgs;

/// Largest prompt file or stdin input read (far above any model's limit; guards
/// against reading a huge file by mistake).
pub const MAX_PROMPT_BYTES: u64 = 4 * 1024 * 1024;

/// Read and validate the prompt. Errors: `usage_error` (no source, several
/// sources, stdin is a terminal), `invalid_argument` (not UTF-8, empty, too
/// large), `input_file_invalid` (prompt file cannot be read).
pub fn read(args: &PromptArgs, stdin: &mut dyn Read, stdin_is_tty: bool) -> Result<String, IrisError> {
    let given: Vec<&str> = [
        args.prompt.is_some().then_some("PROMPT"),
        args.prompt_file.is_some().then_some("--prompt-file"),
        args.prompt_stdin.then_some("--prompt-stdin"),
    ]
    .into_iter()
    .flatten()
    .collect();
    match given.len() {
        0 => return Err(IrisError::usage("a prompt is required (PROMPT, --prompt-file, or --prompt-stdin)")),
        1 => {}
        _ => {
            return Err(IrisError::usage(format!(
                "give exactly one prompt source (PROMPT, --prompt-file, or --prompt-stdin); got {}",
                given.join(" and ")
            )));
        }
    }

    let text = if let Some(inline) = &args.prompt {
        inline
            .to_str()
            .ok_or_else(|| IrisError::invalid("the PROMPT argument is not valid UTF-8"))?
            .to_string()
    } else if let Some(path) = &args.prompt_file {
        let bytes = read_file(path)?;
        let text = String::from_utf8(bytes)
            .map_err(|_| IrisError::invalid(format!("prompt file {} is not valid UTF-8", path.display())))?;
        text.trim_end().to_string()
    } else {
        if stdin_is_tty {
            return Err(IrisError::usage(
                "--prompt-stdin reads the prompt from piped input, but standard input is a terminal",
            )
            .with_hint(
                "pipe the prompt in (e.g. `echo \"a fox\" | iris ... --prompt-stdin`) or pass it as PROMPT",
            ));
        }
        let mut bytes = Vec::new();
        stdin
            .take(MAX_PROMPT_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| IrisError::io("cannot read the prompt from standard input", &e))?;
        if bytes.len() as u64 > MAX_PROMPT_BYTES {
            return Err(too_large("standard input"));
        }
        let text = String::from_utf8(bytes)
            .map_err(|_| IrisError::invalid("the prompt on standard input is not valid UTF-8"))?;
        text.trim_end().to_string()
    };

    if text.trim().is_empty() {
        return Err(IrisError::invalid("the prompt is empty"));
    }
    Ok(text)
}

fn read_file(path: &Path) -> Result<Vec<u8>, IrisError> {
    let invalid = |message: String| {
        IrisError::new(ErrorCode::InputFileInvalid, message).with_detail("path", path.display().to_string())
    };
    let meta = std::fs::metadata(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => invalid(format!("prompt file {} does not exist", path.display())),
        _ => invalid(format!("prompt file {} cannot be read: {e}", path.display())),
    })?;
    if !meta.is_file() {
        return Err(invalid(format!("prompt file {} is not a regular file", path.display())));
    }
    if meta.len() > MAX_PROMPT_BYTES {
        return Err(too_large(&format!("prompt file {}", path.display())));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|f| f.take(MAX_PROMPT_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|e| invalid(format!("prompt file {} cannot be read: {e}", path.display())))?;
    if bytes.len() as u64 > MAX_PROMPT_BYTES {
        return Err(too_large(&format!("prompt file {}", path.display())));
    }
    Ok(bytes)
}

fn too_large(what: &str) -> IrisError {
    IrisError::invalid(format!(
        "{what} is larger than {} MiB; that is not a prompt",
        MAX_PROMPT_BYTES / (1024 * 1024)
    ))
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::PathBuf;

    use super::*;

    fn args(prompt: Option<&str>, file: Option<PathBuf>, stdin: bool) -> PromptArgs {
        PromptArgs { prompt: prompt.map(OsString::from), prompt_file: file, prompt_stdin: stdin }
    }

    #[test]
    fn exactly_one_source_is_required() {
        let e = read(&args(None, None, false), &mut std::io::empty(), false).unwrap_err();
        assert_eq!(e.code, ErrorCode::UsageError);
        assert_eq!(e.message, "a prompt is required (PROMPT, --prompt-file, or --prompt-stdin)");
        let e = read(&args(Some("x"), None, true), &mut std::io::empty(), false).unwrap_err();
        assert_eq!(e.code, ErrorCode::UsageError);
        assert!(e.message.contains("PROMPT and --prompt-stdin"), "{}", e.message);
    }

    #[test]
    fn stdin_is_trimmed_at_the_end_only_and_refused_from_a_terminal() {
        let mut input = "  line one\n\nline two  \n\n".as_bytes();
        assert_eq!(read(&args(None, None, true), &mut input, false).unwrap(), "  line one\n\nline two");
        let e = read(&args(None, None, true), &mut "x".as_bytes(), true).unwrap_err();
        assert_eq!(e.code, ErrorCode::UsageError);
    }

    #[test]
    fn empty_and_non_utf8_prompts_are_invalid() {
        let e = read(&args(Some("   "), None, false), &mut std::io::empty(), false).unwrap_err();
        assert_eq!(e.code, ErrorCode::InvalidArgument);
        let e = read(&args(None, None, true), &mut &[0xff, 0xfe, b'a'][..], false).unwrap_err();
        assert_eq!(e.code, ErrorCode::InvalidArgument);
        let e = read(&args(None, None, true), &mut "\n \n".as_bytes(), false).unwrap_err();
        assert_eq!(e.message, "the prompt is empty");
    }

    #[test]
    fn inline_prompts_are_kept_verbatim() {
        assert_eq!(
            read(&args(Some(" a fox \n"), None, false), &mut std::io::empty(), false).unwrap(),
            " a fox \n"
        );
    }
}
