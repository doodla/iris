//! Command-line interface: argument parsing (clap; see `iris --help`), prompt sources,
//! dispatch to the application workflows, and presentation (docs/json-contract.md envelope or
//! concise human text).
//!
//! [`run`] is the process entry point. [`run_with`] runs one invocation against
//! explicit streams, environment, and dependencies (used by in-process tests).

pub mod args;
pub mod prompt;
pub mod render;

use std::ffi::OsString;
use std::io::{IsTerminal, Read, Write};
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use clap::{CommandFactory, Parser};

use crate::app::doctor::{self, DoctorArgs, DoctorTarget};
use crate::app::image::ImageArgs;
use crate::app::jobs::{ListFilter, Target, WaitArgs};
use crate::app::video::VideoArgs;
use crate::app::{self, AppContext, Deps, GenerationArgs, GenerationOutcome, Progress, info};
use crate::catalog::{OptionSource, RawOption};
use crate::config::{self, CliOverrides, EnvSnapshot, Settings};
use crate::domain::{JobStatus, Operation, ProviderId, Warning};
use crate::error::{ErrorCode, IrisError};
use crate::output::results::{CompletionsResult, SchemaResult};
use crate::output::{self, CommandName, Envelope, ErrorBody, ResultPayload, human};

use args::{
    Cli, Command, ConfigCommand, ImageCommand, JobsCommand, ModelArgs, ModelsCommand, OptionFlags,
    OutputArgs, PromptArgs, ProvidersCommand, Shell, VideoCommand,
};
use render::{Output, Sink};

/// Streams and environment of one invocation.
pub struct Io {
    /// Environment snapshot settings are resolved from.
    pub env: EnvSnapshot,
    /// Source of `--prompt-stdin`.
    pub stdin: Box<dyn Read + Send>,
    /// Whether stdin is a terminal (then `--prompt-stdin` is refused).
    pub stdin_is_tty: bool,
    pub stdout: Sink,
    pub stderr: Sink,
    /// Whether `GOOGLE_API_KEY` is set (presence only; `doctor` warns that it is ignored).
    pub google_api_key_present: bool,
}

/// Run the `iris` process: parse `std::env::args_os`, execute, print, and return
/// the exit code (docs/json-contract.md mapping). An unexpected panic is reported as
/// `internal_error` (exit 1), still as a single JSON document in `--json` mode.
pub fn run() -> i32 {
    let args: Vec<OsString> = std::env::args_os().collect();
    let json = render::json_requested(&args);
    let command = render::guess_command(&args);
    let written = Arc::new(AtomicBool::new(false));
    let stdout: Sink = Arc::new(Mutex::new(TrackedStdout { written: Arc::clone(&written) }));
    let stderr: Sink = Arc::new(Mutex::new(std::io::stderr()));
    let out = Output { json, stdout: stdout.clone(), stderr: stderr.clone() };
    let env = match EnvSnapshot::from_process() {
        Ok(env) => env,
        Err(e) => return out.failure(command, &e, Vec::new()),
    };
    let io = Io {
        env,
        stdin: Box::new(std::io::stdin()),
        stdin_is_tty: std::io::stdin().is_terminal(),
        stdout,
        stderr,
        google_api_key_present: std::env::var_os("GOOGLE_API_KEY").is_some_and(|v| !v.is_empty()),
    };
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            let error = IrisError::io("cannot start the async runtime", &e);
            return out.failure(command, &error, Vec::new());
        }
    };
    guarded(json, command, &written, &mut std::io::stdout(), &mut std::io::stderr(), || {
        runtime.block_on(run_with(args, io, Deps::builtin()))
    })
}

/// Process stdout that records whether anything was written to it.
struct TrackedStdout {
    written: Arc<AtomicBool>,
}

impl Write for TrackedStdout {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if !buf.is_empty() {
            self.written.store(true, Ordering::SeqCst);
        }
        std::io::stdout().write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        std::io::stdout().flush()
    }
}

/// Run `f`; if it panics, report `internal_error` (exit 1): in JSON mode as an
/// envelope on `stdout`, but only if nothing was written there yet (never a
/// second document); otherwise as an error on `stderr`. The panic message itself
/// goes to stderr through the default panic hook.
fn guarded(
    json: bool,
    command: Option<CommandName>,
    stdout_written: &AtomicBool,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    f: impl FnOnce() -> i32,
) -> i32 {
    if let Ok(code) = std::panic::catch_unwind(AssertUnwindSafe(f)) {
        return code;
    }
    let error = IrisError::internal("iris stopped because of an internal error (a bug)").with_hint(
        "please report it; before re-running a paid command, check `iris jobs list` for a job this command may \
         have recorded",
    );
    if json {
        if !stdout_written.load(Ordering::SeqCst) {
            let _ =
                stdout.write_all(Envelope::failure(command, &error, Vec::new()).to_json_line().as_bytes());
            let _ = stdout.flush();
        }
    } else {
        let _ = stderr.write_all(human::error(&ErrorBody::from(&error)).as_bytes());
    }
    error.exit_code()
}

/// Run one invocation with explicit streams, environment, and dependencies.
/// Returns the exit code; exactly one JSON envelope is written to `io.stdout` in
/// `--json` mode.
pub async fn run_with(args: Vec<OsString>, mut io: Io, deps: Deps) -> i32 {
    let out =
        Output { json: render::json_requested(&args), stdout: io.stdout.clone(), stderr: io.stderr.clone() };
    let cli = match Cli::try_parse_from(render::clap_args(&args)) {
        Ok(cli) => cli,
        Err(err) => return out.clap_error(err, &args),
    };
    let command = cli.command.name();
    let mut warnings = Vec::new();
    match execute(cli, &mut io, deps, &mut warnings).await {
        Ok(payload) => out.success(Some(command), payload, warnings),
        Err(error) => out.failure(Some(command), &error, warnings),
    }
}

/// A parsed, locally validated request plus the flag-level setting overrides.
enum Request {
    Image(Operation, ImageArgs),
    Video(VideoArgs),
    JobsList(ListFilter),
    JobsStatus { job_id: String, refresh: bool },
    JobsWait { job_id: String, args: WaitArgs },
    JobsDownload { job_id: String, target: Target },
    JobsDelete { job_ids: Vec<String>, all: bool, force: bool },
    ModelsList { provider: Option<ProviderId>, operation: Option<Operation> },
    ModelsShow { model: String, check_access: bool },
    ProvidersList,
    ConfigShow,
    ConfigPath,
    Doctor(DoctorArgs),
}

#[derive(Default)]
struct Overrides {
    out_dir: Option<PathBuf>,
    image_provider: Option<ProviderId>,
    wait_timeout: Option<Duration>,
    poll_interval: Option<Duration>,
}

async fn execute(
    cli: Cli,
    io: &mut Io,
    deps: Deps,
    warnings: &mut Vec<Warning>,
) -> Result<ResultPayload, IrisError> {
    let Cli { global, command } = cli;
    // Commands that need no settings.
    match &command {
        Command::Version => return Ok(ResultPayload::Version(info::version())),
        Command::Schema => return Ok(ResultPayload::Schema(SchemaResult { schema: output::schema() })),
        Command::Completions(a) => return Ok(ResultPayload::Completions(completions(a.shell))),
        _ => {}
    }

    // Local parsing and validation that needs no settings (prompt sources, option
    // syntax, names, durations).
    let (request, overrides) = build_request(command, io)?;
    let cli_overrides = CliOverrides {
        config_path: global.config.clone(),
        out_dir: overrides.out_dir,
        provider: overrides.image_provider,
        wait_timeout: overrides.wait_timeout,
        poll_interval: overrides.poll_interval,
        verbose: global.verbose,
    };
    let settings = Settings::load(&cli_overrides, &io.env);
    init_tracing(
        settings.as_ref().map(|s| s.log_filter.value.as_str()).unwrap_or(config::DEFAULT_LOG_FILTER),
    );
    let progress = if global.quiet {
        Progress::silent()
    } else {
        let sink = io.stderr.clone();
        Progress::new(move |line| render::write(&sink, &format!("{}\n", crate::redact::scrub(line))))
    };

    if let Request::Doctor(args) = &request {
        let args = DoctorArgs { google_api_key_present: io.google_api_key_present, ..args.clone() };
        let result = match settings {
            Ok(settings) => {
                let ctx = AppContext::new(settings, deps, progress);
                doctor::run(DoctorTarget::Loaded(&ctx), &args, warnings).await
            }
            Err(error) => {
                doctor::run(DoctorTarget::Invalid { error: &error, env: &io.env }, &args, warnings).await
            }
        };
        return Ok(ResultPayload::Doctor(result));
    }

    let ctx = AppContext::new(settings?, deps, progress);
    Ok(match request {
        Request::Image(op, args) => match app::image::run(&ctx, op, args, warnings).await? {
            GenerationOutcome::Completed(r) => ResultPayload::Image(r),
            GenerationOutcome::Planned(p) => ResultPayload::Plan(p),
        },
        Request::Video(args) => match app::video::run(&ctx, args, warnings).await? {
            GenerationOutcome::Completed(r) => ResultPayload::Job(r),
            GenerationOutcome::Planned(p) => ResultPayload::Plan(p),
        },
        Request::JobsList(filter) => ResultPayload::JobList(app::jobs::list(&ctx, &filter, warnings)?),
        Request::JobsStatus { job_id, refresh } => {
            ResultPayload::Job(app::jobs::status(&ctx, &job_id, refresh, warnings).await?)
        }
        Request::JobsWait { job_id, args } => {
            ResultPayload::Job(app::jobs::wait(&ctx, &job_id, &args, warnings).await?)
        }
        Request::JobsDownload { job_id, target } => {
            ResultPayload::Job(app::jobs::download(&ctx, &job_id, &target, warnings).await?)
        }
        Request::JobsDelete { job_ids, all, force } => {
            ResultPayload::JobDelete(app::jobs::delete(&ctx, &job_ids, all, force, warnings)?)
        }
        Request::ModelsList { provider, operation } => {
            ResultPayload::ModelList(app::models::list(&ctx, provider, operation))
        }
        Request::ModelsShow { model, check_access } => {
            ResultPayload::ModelShow(app::models::show(&ctx, &model, check_access, warnings).await?)
        }
        Request::ProvidersList => ResultPayload::ProviderList(app::models::providers(&ctx)),
        Request::ConfigShow => ResultPayload::ConfigShow(info::config_show(&ctx, warnings)),
        Request::ConfigPath => ResultPayload::ConfigPath(info::config_path(&ctx)),
        Request::Doctor(_) => unreachable!("handled above"),
    })
}

fn build_request(command: Command, io: &mut Io) -> Result<(Request, Overrides), IrisError> {
    let mut o = Overrides::default();
    let request = match command {
        Command::Image(ImageCommand::Generate(a)) => {
            let common =
                generation(&a.prompt, &a.model, a.options.flags(), &a.output, a.dry_run, io, &mut o)?;
            o.image_provider = common.provider;
            Request::Image(Operation::ImageGenerate, ImageArgs { common, images: Vec::new(), mask: None })
        }
        Command::Image(ImageCommand::Edit(a)) => {
            let common =
                generation(&a.prompt, &a.model, a.options.flags(), &a.output, a.dry_run, io, &mut o)?;
            o.image_provider = common.provider;
            let images = a.images.into_iter().map(|p| absolute(&io.env, p)).collect();
            let mask = a.mask.map(|p| absolute(&io.env, p));
            Request::Image(Operation::ImageEdit, ImageArgs { common, images, mask })
        }
        Command::Video(VideoCommand::Generate(a)) => {
            let common =
                generation(&a.prompt, &a.model, a.options.flags(), &a.output, a.dry_run, io, &mut o)?;
            o.wait_timeout = duration_flag("--timeout", a.timeout.as_deref())?;
            o.poll_interval = duration_flag("--poll-interval", a.poll_interval.as_deref())?;
            Request::Video(VideoArgs {
                common,
                first_frame: a.first_frame.map(|p| absolute(&io.env, p)),
                last_frame: a.last_frame.map(|p| absolute(&io.env, p)),
                references: a.references.into_iter().map(|p| absolute(&io.env, p)).collect(),
                detach: a.detach,
            })
        }
        Command::Jobs(JobsCommand::List(a)) => Request::JobsList(ListFilter {
            status: a.status.as_deref().map(parse_status).transpose()?,
            provider: a.provider.as_deref().map(parse_provider).transpose()?,
            limit: a.limit,
        }),
        Command::Jobs(JobsCommand::Status(a)) => {
            Request::JobsStatus { job_id: a.job_id, refresh: !a.no_refresh }
        }
        Command::Jobs(JobsCommand::Wait(a)) => {
            o.wait_timeout = duration_flag("--timeout", a.timeout.as_deref())?;
            o.poll_interval = duration_flag("--poll-interval", a.poll_interval.as_deref())?;
            o.out_dir = a.output.out_dir;
            Request::JobsWait {
                job_id: a.job_id,
                args: WaitArgs {
                    download: !a.no_download,
                    target: Target {
                        output: a.output.output.map(|p| absolute(&io.env, p)),
                        overwrite: a.output.overwrite,
                    },
                },
            }
        }
        Command::Jobs(JobsCommand::Download(a)) => {
            o.out_dir = a.output.out_dir;
            Request::JobsDownload {
                job_id: a.job_id,
                target: Target {
                    output: a.output.output.map(|p| absolute(&io.env, p)),
                    overwrite: a.output.overwrite,
                },
            }
        }
        Command::Jobs(JobsCommand::Delete(a)) => {
            Request::JobsDelete { job_ids: a.job_ids, all: a.all, force: a.force }
        }
        Command::Models(ModelsCommand::List(a)) => Request::ModelsList {
            provider: a.provider.as_deref().map(parse_provider).transpose()?,
            operation: a.operation.as_deref().map(parse_operation).transpose()?,
        },
        Command::Models(ModelsCommand::Show(a)) => {
            Request::ModelsShow { model: a.model, check_access: a.check_access }
        }
        Command::Providers(ProvidersCommand::List) => Request::ProvidersList,
        Command::Config(ConfigCommand::Show) => Request::ConfigShow,
        Command::Config(ConfigCommand::Path) => Request::ConfigPath,
        Command::Doctor(a) => {
            Request::Doctor(DoctorArgs { check_access: a.check_access, google_api_key_present: false })
        }
        Command::Schema | Command::Completions(_) | Command::Version => {
            return Err(IrisError::internal("informational commands are handled before settings are loaded"));
        }
    };
    Ok((request, o))
}

/// Shared generation arguments: prompt (read and checked), provider name, option
/// syntax, output target.
fn generation(
    prompt_args: &PromptArgs,
    model: &ModelArgs,
    flags: OptionFlags<'_>,
    output: &OutputArgs,
    dry_run: bool,
    io: &mut Io,
    overrides: &mut Overrides,
) -> Result<GenerationArgs, IrisError> {
    let prompt_args = PromptArgs {
        prompt: prompt_args.prompt.clone(),
        prompt_file: prompt_args.prompt_file.clone().map(|p| absolute(&io.env, p)),
        prompt_stdin: prompt_args.prompt_stdin,
    };
    let prompt = prompt::read(&prompt_args, &mut *io.stdin, io.stdin_is_tty)?;
    let provider = model.provider.as_deref().map(parse_provider).transpose()?;
    let options = raw_options(&flags)?;
    overrides.out_dir = output.out_dir.clone();
    Ok(GenerationArgs {
        prompt,
        provider,
        model: model.model.clone(),
        capabilities_from: model.capabilities_from.clone(),
        options,
        output: output.output.clone().map(|p| absolute(&io.env, p)),
        overwrite: output.overwrite,
        dry_run,
    })
}

/// Resolve a user-supplied path against the invocation's current directory (the
/// environment snapshot's, which is the process's in production). Paths are
/// otherwise used literally.
fn absolute(env: &EnvSnapshot, path: PathBuf) -> PathBuf {
    if path.is_absolute() { path } else { env.cwd().join(path) }
}

/// Typed flags → `RawOption { source: Flag(..) }` using the command's flag → option
/// name table (`args::IMAGE_FLAGS` / `args::VIDEO_FLAGS`), then `-O KEY=VALUE` (`=`
/// required). A typed flag and `-O` for the same option is a usage error; duplicate
/// `-O` keys are rejected by validation.
pub fn raw_options(flags: &OptionFlags<'_>) -> Result<Vec<RawOption>, IrisError> {
    let mut raw: Vec<RawOption> = flags
        .typed
        .iter()
        .filter_map(|&(flag, name, value)| {
            value.map(|v| RawOption {
                name: name.to_string(),
                value: v.clone(),
                source: OptionSource::Flag(flag),
            })
        })
        .collect();
    for item in flags.generic {
        let Some((key, value)) = item.split_once('=') else {
            return Err(IrisError::usage(format!("-O/--option expects KEY=VALUE, got '{item}'"))
                .with_hint("run `iris models show <MODEL>` to see the options a model accepts"));
        };
        let key = key.trim();
        if key.is_empty() {
            return Err(IrisError::usage(format!(
                "-O/--option expects KEY=VALUE with a non-empty KEY, got '{item}'"
            )));
        }
        if let Some(OptionSource::Flag(flag)) =
            raw.iter().find(|o| o.name == key && matches!(o.source, OptionSource::Flag(_))).map(|o| o.source)
        {
            return Err(IrisError::usage(format!(
                "{flag} and -O {key}=… set the same option; give it only once"
            ))
            .with_detail("option", key));
        }
        raw.push(RawOption {
            name: key.to_string(),
            value: value.to_string(),
            source: OptionSource::Generic,
        });
    }
    Ok(raw)
}

fn parse_provider(raw: &str) -> Result<ProviderId, IrisError> {
    raw.parse::<ProviderId>().map_err(|message| {
        IrisError::new(ErrorCode::UnknownProvider, message).with_hint("run `iris providers list`")
    })
}

fn parse_operation(raw: &str) -> Result<Operation, IrisError> {
    raw.parse::<Operation>().map_err(IrisError::invalid)
}

fn parse_status(raw: &str) -> Result<JobStatus, IrisError> {
    raw.parse::<JobStatus>().map_err(|message| {
        IrisError::invalid(format!(
            "{message} (expected submitting, submission_unknown, running, succeeded, failed, or expired)"
        ))
    })
}

fn duration_flag(flag: &str, raw: Option<&str>) -> Result<Option<Duration>, IrisError> {
    raw.map(|v| {
        config::parse_duration(v)
            .map_err(|m| IrisError::invalid(format!("{flag}: {m}")).with_detail("flag", flag))
    })
    .transpose()
}

fn completions(shell: Shell) -> CompletionsResult {
    let mut command = Cli::command();
    let mut script = Vec::new();
    clap_complete::generate(shell.generator(), &mut command, "iris", &mut script);
    CompletionsResult {
        shell: shell.name().to_string(),
        script: String::from_utf8_lossy(&script).into_owned(),
    }
}

/// Install the stderr log subscriber once per process (`IRIS_LOG`, `-v`). Logs
/// carry request metadata only, never prompts, keys, or signed URLs.
fn init_tracing(filter: &str) {
    let filter = tracing_subscriber::EnvFilter::try_new(filter)
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(config::DEFAULT_LOG_FILTER));
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_panic_becomes_one_internal_error_document_and_never_a_second_one() {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code =
            guarded(true, Some(CommandName::JobsList), &AtomicBool::new(false), &mut out, &mut err, || {
                panic!("test panic (expected)")
            });
        assert_eq!(code, 1);
        let text = String::from_utf8(out).unwrap();
        assert_eq!(text.lines().count(), 1, "{text}");
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["ok"], false);
        assert_eq!(v["command"], "jobs.list");
        assert_eq!(v["error"]["code"], "internal_error");

        // A document was already written: nothing more on stdout.
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = guarded(true, None, &AtomicBool::new(true), &mut out, &mut err, || {
            panic!("test panic (expected)")
        });
        assert_eq!(code, 1);
        assert!(out.is_empty());

        // Human mode: the error goes to stderr; a normal run keeps its exit code.
        let (mut out, mut err) = (Vec::new(), Vec::new());
        guarded(false, None, &AtomicBool::new(false), &mut out, &mut err, || panic!("test panic (expected)"));
        assert!(out.is_empty());
        assert!(String::from_utf8(err).unwrap().starts_with("error[internal_error]: "));
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert_eq!(guarded(true, None, &AtomicBool::new(false), &mut out, &mut err, || 4), 4);
    }

    #[test]
    fn typed_flags_map_to_option_names_and_conflict_with_generic_options() {
        let t = args::ImageOptions {
            quality: Some("low".into()),
            options: vec!["background=transparent".into(), "empty=".into()],
            ..Default::default()
        };
        let raw = raw_options(&t.flags()).unwrap();
        let view: Vec<(&str, &str)> = raw.iter().map(|o| (o.name.as_str(), o.value.as_str())).collect();
        assert_eq!(view, [("quality", "low"), ("background", "transparent"), ("empty", "")]);
        assert_eq!(raw[0].source, OptionSource::Flag("--quality"));
        assert_eq!(raw[1].source, OptionSource::Generic);

        let t = args::ImageOptions {
            quality: Some("low".into()),
            options: vec!["quality=high".into()],
            ..Default::default()
        };
        let e = raw_options(&t.flags()).unwrap_err();
        assert_eq!(e.code, ErrorCode::UsageError);
        assert!(e.message.contains("--quality"), "{}", e.message);

        for bad in ["novalue", "=x"] {
            let t = args::VideoOptions { options: vec![bad.into()], ..Default::default() };
            assert_eq!(raw_options(&t.flags()).unwrap_err().code, ErrorCode::UsageError, "{bad}");
        }
    }

    /// Every entry of a command's flag table is a real flag of that command and sets
    /// the option the table names (the tables and the clap structs cannot drift).
    #[test]
    fn flag_tables_match_the_parsed_flags_of_each_command() {
        type FlagTable = &'static [(&'static str, &'static str)];
        let commands: [(&[&str], FlagTable); 3] = [
            (&["image", "generate", "p"], args::IMAGE_FLAGS),
            (&["image", "edit", "-i", "a.png", "p"], args::IMAGE_FLAGS),
            (&["video", "generate", "p"], args::VIDEO_FLAGS),
        ];
        for (words, table) in commands {
            for &(flag, name) in table {
                let mut argv = vec!["iris"];
                argv.extend_from_slice(words);
                argv.extend([flag, "V"]);
                let cli = Cli::try_parse_from(&argv).unwrap_or_else(|e| panic!("{argv:?}: {e}"));
                let flags = match &cli.command {
                    Command::Image(ImageCommand::Generate(a)) => a.options.flags(),
                    Command::Image(ImageCommand::Edit(a)) => a.options.flags(),
                    Command::Video(VideoCommand::Generate(a)) => a.options.flags(),
                    other => panic!("unexpected command {other:?}"),
                };
                let raw = raw_options(&flags).unwrap();
                assert_eq!(raw.len(), 1, "{argv:?}");
                assert_eq!((raw[0].name.as_str(), raw[0].value.as_str()), (name, "V"), "{argv:?}");
                assert_eq!(raw[0].source, OptionSource::Flag(flag), "{argv:?}");
            }
        }
    }
}
