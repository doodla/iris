//! Writing results: exactly one JSON envelope on stdout in `--json` mode (docs/json-contract.md),
//! concise text otherwise (`output::human`). clap usage errors, `--help`, and
//! `--version` are converted to envelopes too when `--json` appears in argv.

use std::ffi::OsString;
use std::io::Write;
use std::sync::{Arc, Mutex};

use clap::error::ErrorKind;

use crate::app::info;
use crate::domain::Warning;
use crate::error::IrisError;
use crate::output::results::HelpResult;
use crate::output::{CommandName, Envelope, ErrorBody, ResultPayload, SCHEMA_VERSION, human};
use crate::redact;

/// A shared output stream (stdout or stderr).
pub type Sink = Arc<Mutex<dyn Write + Send>>;

/// Write `text` to a sink, ignoring broken pipes.
pub fn write(sink: &Sink, text: &str) {
    if text.is_empty() {
        return;
    }
    if let Ok(mut w) = sink.lock() {
        let _ = w.write_all(text.as_bytes());
        let _ = w.flush();
    }
}

/// True if `--json` (or a `--json=VALUE` form, which clap then rejects as a
/// usage error) appears in argv before a `--` terminator.
pub fn json_requested(args: &[OsString]) -> bool {
    args.iter()
        .skip(1)
        .take_while(|a| *a != "--")
        .any(|a| a == "--json" || a.to_str().is_some_and(|a| a.starts_with("--json=")))
}

/// The first two command words of argv (skipping flags and the `--config` value).
fn command_words(args: &[OsString]) -> Vec<&str> {
    let mut words = Vec::new();
    let mut iter = args.iter().skip(1).filter_map(|a| a.to_str());
    while let Some(arg) = iter.next() {
        if arg == "--" {
            break;
        }
        if arg == "--config" {
            iter.next();
            continue;
        }
        if arg.starts_with('-') {
            continue;
        }
        words.push(arg);
        if words.len() == 2 {
            break;
        }
    }
    words
}

/// argv for clap. clap's `help` subcommand (`iris help …`, `iris jobs help …`)
/// accepts only command names, so `--json` is removed there; JSON mode was
/// already detected from the original argv.
pub fn clap_args(args: &[OsString]) -> Vec<OsString> {
    let words = command_words(args);
    let help = match words.as_slice() {
        ["help", ..] => true,
        [group, "help", ..] => GROUPS.contains(group),
        _ => false,
    };
    if !help {
        return args.to_vec();
    }
    args.iter()
        .enumerate()
        .filter(|(i, a)| *i == 0 || !(*a == "--json" || a.to_str().is_some_and(|a| a.starts_with("--json="))))
        .map(|(_, a)| a.clone())
        .collect()
}

/// `Deleted <job_id>` lines for the job records a failed `jobs delete` had already
/// deleted (`details.deleted`); empty when there are none.
fn deleted_before_error(e: &ErrorBody) -> String {
    let deleted = e.details.as_ref().and_then(|d| d.get("deleted")).and_then(serde_json::Value::as_array);
    deleted
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .map(|id| format!("Deleted {id}\n"))
        .collect()
}

/// Commands that have subcommands.
const GROUPS: &[&str] = &["image", "video", "jobs", "models", "providers", "config"];

/// A clap error's message without the `error: ` prefix: every line up to the
/// first blank line or `Usage:`, joined with spaces (so a missing-argument error
/// names the argument).
fn clap_message(text: &str) -> String {
    let lines: Vec<&str> = text
        .lines()
        .take_while(|l| !l.trim().is_empty() && !l.trim_start().starts_with("Usage:"))
        .map(str::trim)
        .collect();
    let joined = lines.join(" ");
    let message = joined.strip_prefix("error: ").unwrap_or(&joined).trim();
    if message.is_empty() { "invalid arguments".to_string() } else { message.to_string() }
}

/// Best-effort command name from argv, for envelopes of errors raised before or
/// during parsing (`None` when no known command is recognizable).
pub fn guess_command(args: &[OsString]) -> Option<CommandName> {
    let words = command_words(args);
    let first = *words.first()?;
    let second = words.get(1).copied().unwrap_or("");
    Some(match (first, second) {
        ("image", "generate") => CommandName::ImageGenerate,
        ("image", "edit") => CommandName::ImageEdit,
        ("video", "generate") => CommandName::VideoGenerate,
        ("jobs", "list") => CommandName::JobsList,
        ("jobs", "status") => CommandName::JobsStatus,
        ("jobs", "wait") => CommandName::JobsWait,
        ("jobs", "download") => CommandName::JobsDownload,
        ("jobs", "delete") => CommandName::JobsDelete,
        ("models", "list") => CommandName::ModelsList,
        ("models", "show") => CommandName::ModelsShow,
        ("providers", "list") => CommandName::ProvidersList,
        ("config", "show") => CommandName::ConfigShow,
        ("config", "path") => CommandName::ConfigPath,
        ("doctor", _) => CommandName::Doctor,
        ("schema", _) => CommandName::Schema,
        ("completions", _) => CommandName::Completions,
        ("version", _) => CommandName::Version,
        _ => return None,
    })
}

/// Where and how results are written.
#[derive(Clone)]
pub struct Output {
    pub json: bool,
    pub stdout: Sink,
    pub stderr: Sink,
}

impl Output {
    /// Print a success; returns exit code 0.
    pub fn success(
        &self,
        command: Option<CommandName>,
        payload: ResultPayload,
        warnings: Vec<Warning>,
    ) -> i32 {
        if self.json {
            let envelope = Envelope {
                schema_version: SCHEMA_VERSION,
                ok: true,
                command,
                result: Some(payload),
                error: None,
                warnings,
            };
            write(&self.stdout, &envelope.to_json_line());
        } else {
            self.human_warnings(&warnings);
            let rendered = human::render(command, &payload);
            write(&self.stdout, &redact::scrub(&rendered.stdout));
            write(&self.stderr, &redact::scrub(&rendered.stderr));
        }
        0
    }

    /// Print a failure; returns the error's exit code. In human mode, what was done
    /// before the failure is still listed on stdout: files saved (`details.saved`,
    /// as `Saved <path>` lines) and job records deleted (`details.deleted`, as
    /// `Deleted <job_id>` lines).
    pub fn failure(&self, command: Option<CommandName>, error: &IrisError, warnings: Vec<Warning>) -> i32 {
        if self.json {
            write(&self.stdout, &Envelope::failure(command, error, warnings).to_json_line());
        } else {
            let body = ErrorBody::from(error);
            self.human_warnings(&warnings);
            write(&self.stdout, &redact::scrub(&human::saved_before_error(&body)));
            write(&self.stdout, &redact::scrub(&deleted_before_error(&body)));
            write(&self.stderr, &redact::scrub(&human::error(&body)));
        }
        error.exit_code()
    }

    fn human_warnings(&self, warnings: &[Warning]) {
        let text: String = warnings.iter().map(human::warning).collect();
        write(&self.stderr, &redact::scrub(&text));
    }

    /// Handle a clap parse outcome that is not a successful parse: help, version,
    /// or a usage error (exit 2). A help result's envelope has `command: null`
    /// (the help text is not the named command's result).
    pub fn clap_error(&self, err: clap::Error, args: &[OsString]) -> i32 {
        let text = err.render().to_string();
        let command = guess_command(args);
        match err.kind() {
            ErrorKind::DisplayHelp => {
                if self.json {
                    self.success(None, ResultPayload::Help(HelpResult { help: text }), Vec::new())
                } else {
                    write(&self.stdout, &text);
                    0
                }
            }
            ErrorKind::DisplayVersion => {
                if self.json {
                    self.success(
                        Some(CommandName::Version),
                        ResultPayload::Version(info::version()),
                        Vec::new(),
                    )
                } else {
                    write(&self.stdout, &text);
                    0
                }
            }
            ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => {
                if self.json {
                    let error = IrisError::usage("a subcommand is required")
                        .with_hint("run `iris --help` (or `iris <COMMAND> --help`) to see the commands")
                        .with_detail("help", text);
                    self.failure(command, &error, Vec::new())
                } else {
                    write(&self.stderr, &text);
                    crate::error::exit::USAGE
                }
            }
            _ => {
                if self.json {
                    let error = IrisError::usage(clap_message(&text))
                        .with_hint("run the command with --help for usage")
                        .with_detail("usage", text.trim_end().to_string());
                    self.failure(command, &error, Vec::new())
                } else {
                    write(&self.stderr, &text);
                    crate::error::exit::USAGE
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(items: &[&str]) -> Vec<OsString> {
        items.iter().map(OsString::from).collect()
    }

    #[test]
    fn json_flag_is_found_anywhere_before_the_terminator() {
        assert!(json_requested(&argv(&["iris", "jobs", "list", "--json"])));
        assert!(json_requested(&argv(&["iris", "--json", "jobs", "list"])));
        assert!(!json_requested(&argv(&["iris", "image", "generate", "--", "--json"])));
        assert!(!json_requested(&argv(&["iris", "--jsonx"])));
        assert!(json_requested(&argv(&["iris", "version", "--json=true"])));
    }

    #[test]
    fn json_is_dropped_only_for_the_help_subcommand() {
        assert_eq!(clap_args(&argv(&["iris", "help", "--json"])), argv(&["iris", "help"]));
        assert_eq!(
            clap_args(&argv(&["iris", "jobs", "help", "wait", "--json"])),
            argv(&["iris", "jobs", "help", "wait"])
        );
        let prompt = argv(&["iris", "image", "generate", "help", "--json"]);
        assert_eq!(clap_args(&prompt), prompt, "a prompt that reads 'help' is not the help command");
    }

    #[test]
    fn clap_messages_keep_every_line_before_the_usage() {
        let text = "error: the following required arguments were not provided:\n  <JOB_ID>\n\nUsage: iris jobs \
                    status <JOB_ID>\n\nFor more information, try '--help'.\n";
        assert_eq!(clap_message(text), "the following required arguments were not provided: <JOB_ID>");
        let text = "error: unexpected argument '--bogus' found\n\n  tip: to pass '--bogus' as a value, use '-- \
                    --bogus'\n\nUsage: iris image generate [OPTIONS] [PROMPT]\n";
        assert_eq!(clap_message(text), "unexpected argument '--bogus' found");
        assert_eq!(clap_message(""), "invalid arguments");
    }

    #[test]
    fn human_failures_list_the_records_deleted_before_them() {
        let stdout: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        let stderr: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        let output = Output { json: false, stdout: stdout.clone(), stderr: stderr.clone() };
        let error = IrisError::new(crate::error::ErrorCode::IoError, "cannot delete job record")
            .with_detail("deleted", vec!["job_a".to_string(), "job_b".to_string()]);
        let code = output.failure(Some(CommandName::JobsDelete), &error, Vec::new());
        assert_eq!(code, 1);
        let out = String::from_utf8(stdout.lock().unwrap().clone()).unwrap();
        assert_eq!(out, "Deleted job_a\nDeleted job_b\n");
        let err = String::from_utf8(stderr.lock().unwrap().clone()).unwrap();
        assert!(err.contains("error[io_error]: cannot delete job record"), "{err}");

        // A refusal deleted nothing and prints nothing on stdout.
        stdout.lock().unwrap().clear();
        let refused =
            IrisError::invalid("job x is still running").with_detail("deleted", Vec::<String>::new());
        output.failure(Some(CommandName::JobsDelete), &refused, Vec::new());
        assert!(stdout.lock().unwrap().is_empty());
    }

    #[test]
    fn command_names_are_guessed_from_argv() {
        assert_eq!(
            guess_command(&argv(&["iris", "--config", "c.toml", "image", "generate", "x"])),
            Some(CommandName::ImageGenerate)
        );
        assert_eq!(guess_command(&argv(&["iris", "-q", "doctor"])), Some(CommandName::Doctor));
        assert_eq!(guess_command(&argv(&["iris", "jobs"])), None);
        assert_eq!(guess_command(&argv(&["iris", "bogus"])), None);
        assert_eq!(guess_command(&argv(&["iris"])), None);
    }
}
