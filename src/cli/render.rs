//! Writing results: exactly one JSON envelope on stdout in `--json` mode (C-03),
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

/// True if `--json` appears in argv before a `--` terminator.
pub fn json_requested(args: &[OsString]) -> bool {
    args.iter().skip(1).take_while(|a| *a != "--").any(|a| a == "--json")
}

/// Best-effort command name from argv, for envelopes of errors raised before or
/// during parsing (`None` when no known command is recognizable).
pub fn guess_command(args: &[OsString]) -> Option<CommandName> {
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

    /// Print a failure; returns the error's exit code.
    pub fn failure(&self, command: Option<CommandName>, error: &IrisError, warnings: Vec<Warning>) -> i32 {
        if self.json {
            write(&self.stdout, &Envelope::failure(command, error, warnings).to_json_line());
        } else {
            self.human_warnings(&warnings);
            write(&self.stderr, &redact::scrub(&human::error(&ErrorBody::from(error))));
        }
        error.exit_code()
    }

    fn human_warnings(&self, warnings: &[Warning]) {
        let text: String = warnings.iter().map(human::warning).collect();
        write(&self.stderr, &redact::scrub(&text));
    }

    /// Handle a clap parse outcome that is not a successful parse: help, version,
    /// or a usage error (exit 2).
    pub fn clap_error(&self, err: clap::Error, args: &[OsString]) -> i32 {
        let text = err.render().to_string();
        let command = guess_command(args);
        match err.kind() {
            ErrorKind::DisplayHelp => {
                if self.json {
                    self.success(command, ResultPayload::Help(HelpResult { help: text }), Vec::new())
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
                    let first = text.lines().next().unwrap_or("invalid arguments");
                    let message = first.strip_prefix("error: ").unwrap_or(first).to_string();
                    let error = IrisError::usage(message)
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
