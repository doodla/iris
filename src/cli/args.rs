//! clap definitions of the frozen command tree (see `iris --help`).
//!
//! Values that need Iris-specific validation (option values, durations, provider
//! and status names, job ids, prompts) are taken as strings here and validated by
//! the CLI/app layers, so they produce the stable error codes of docs/json-contract.md
//! (`invalid_argument`, `unknown_provider`, ...) rather than generic usage errors.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::LazyLock;

use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum};

use crate::app::info;
use crate::output::CommandName;

/// `iris --version` text: version and build target.
pub static VERSION: LazyLock<String> =
    LazyLock::new(|| format!("{} ({})", env!("CARGO_PKG_VERSION"), info::target()));

const ABOUT: &str = "Generate and edit images and generate videos with OpenAI and Google Gemini/Veo";

const LONG_ABOUT: &str = "\
Iris generates and edits images (OpenAI GPT Image, Google Gemini \"Nano Banana\") and generates \
videos (Google Veo) from one agent-friendly command line.

Every generation command names its model: pass -m/--model, or set the model in the config file \
([image] model, [video] model). Iris never chooses a model for you; `iris models list` shows the \
models it knows.

Image commands are synchronous: the image is saved before the command returns. Video generation \
is a provider-native job: Iris records it locally, waits, and saves the video; with --detach it \
returns a job id that later commands (iris jobs status/wait/download) resume, even from another \
process.

Credentials are read only from the OPENAI_API_KEY and GEMINI_API_KEY environment variables. \
Provider usage is billed by the provider to your API account; Iris reports cost estimates only.

With --json, every command prints exactly one JSON document on stdout (schema: `iris schema`); \
progress and diagnostics go to stderr. Iris never prompts interactively.";

const AFTER_HELP: &str = "\
Examples:
  iris image generate -m gpt-image-2.5-sunburst --quality low --size 1024x1024 \"a fox\" -o fox.png
  iris image edit -m nano-banana-2 -i photo.png \"make it autumn\" --json
  iris video generate -m veo-lite \"waves at dusk\" --duration 4 --detach
  iris jobs wait job_01jbz9k3m4n5p6q7r8s9t0v1w2
  iris models list --operation video.generate
  iris doctor

Exit codes: 0 success, 1 runtime or provider failure, 2 invalid request: fix it before retrying \
(error.provider_status null means nothing was sent; otherwise the provider rejected it), 3 \
credentials/access/quota, 4 job not finished yet (it continues remotely), 5 outcome uncertain (do \
not resubmit blindly), 130 interrupted (Ctrl-C/SIGINT, SIGTERM, or SIGHUP).

Provider usage is billed by the provider; `--dry-run` validates a request without sending it.";

/// Top-level command line.
#[derive(Debug, Parser)]
#[command(
    name = "iris",
    bin_name = "iris",
    version = VERSION.as_str(),
    about = ABOUT,
    long_about = LONG_ABOUT,
    after_help = AFTER_HELP,
    propagate_version = true,
    subcommand_required = true,
    arg_required_else_help = true,
    max_term_width = 100
)]
pub struct Cli {
    #[command(flatten)]
    pub global: GlobalArgs,
    #[command(subcommand)]
    pub command: Command,
}

/// Flags valid before or after any subcommand.
#[derive(Debug, Args)]
pub struct GlobalArgs {
    /// Machine mode: exactly one JSON document on stdout; progress and diagnostics on stderr
    #[arg(long, global = true)]
    pub json: bool,
    /// Config file (overrides IRIS_CONFIG and the platform default)
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,
    /// Suppress progress lines on stderr
    #[arg(short, long, global = true)]
    pub quiet: bool,
    /// More diagnostics on stderr (repeatable); never prints prompts, keys, or signed URLs
    #[arg(short, long, global = true, action = ArgAction::Count)]
    pub verbose: u8,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Generate and edit images (synchronous; no job record)
    #[command(
        subcommand,
        long_about = "Generate images from a prompt, or edit/compose images from local reference images. \
                      Both are synchronous paid requests: the images are validated and saved before the \
                      command returns, and no job record is created.",
        after_help = "Examples:\n  iris image generate -m gpt-image-2.5-sunburst --quality low --size 1024x1024 \
                      \"a fox\" -o fox.png\n  iris image edit -m nano-banana-2 -i photo.png \"make it autumn\" --json"
    )]
    Image(ImageCommand),
    /// Generate videos (provider-native asynchronous jobs)
    #[command(
        subcommand,
        long_about = "Generate videos as provider-native asynchronous jobs. Iris records each job locally \
                      before submitting it, so it can be resumed by later commands.",
        after_help = "Examples:\n  iris video generate -m veo-lite \"waves at dusk\" --duration 4 -o waves.mp4\n  \
                      iris video generate -m veo-lite \"a paper boat\" --detach --json"
    )]
    Video(VideoCommand),
    /// List, inspect, wait for, download, and delete local video jobs
    #[command(
        subcommand,
        long_about = "Work with local records of provider-native jobs (video generation). Records live in \
                      the state directory (`iris config path`). Deleting a record never cancels or deletes \
                      anything remotely.",
        after_help = "Examples:\n  iris jobs list\n  iris jobs wait job_01jbz9k3m4n5p6q7r8s9t0v1w2\n  iris jobs \
                      download job_01jbz9k3m4n5p6q7r8s9t0v1w2 -o clip.mp4"
    )]
    Jobs(JobsCommand),
    /// List models and inspect their capabilities
    #[command(
        subcommand,
        long_about = "List the models Iris knows and inspect their declared capabilities, options (with \
                      their defaults), and published prices.",
        after_help = "Examples:\n  iris models list\n  iris models show nano-banana-2 --json"
    )]
    Models(ModelsCommand),
    /// List providers, credential variables, and whether they are set
    #[command(
        subcommand,
        long_about = "List providers with the environment variable each reads its API key from and whether \
                      it is set (never its value).",
        after_help = "Examples:\n  iris providers list\n  iris providers list --json"
    )]
    Providers(ProvidersCommand),
    /// Show effective configuration and file locations
    #[command(
        subcommand,
        long_about = "Show the effective non-secret settings with the source of each value (flag, env, file, \
                      default), and where Iris keeps its files.",
        after_help = "Examples:\n  iris config show\n  iris config path --json"
    )]
    Config(ConfigCommand),
    /// Check credentials, configuration, and directories
    #[command(
        long_about = "Check credential presence (never values), configuration validity, state and output \
                      directory writability, and base URL overrides. --check-access additionally makes free \
                      metadata calls to see whether every catalog model of each provider whose API key is set \
                      is visible to your key; they do not check billing tier, prepaid credit, or organization \
                      verification, so a paid request can still be refused.\n\nExit status: \
                      doctor exits 0 whenever its checks ran, even when it finds problems. Read `healthy` \
                      (result.healthy with --json) or look for [error] lines instead of relying on the exit \
                      code.",
        after_help = "Examples:\n  iris doctor\n  iris doctor --check-access --json"
    )]
    Doctor(DoctorArgs),
    /// Print the published JSON output schema
    #[command(
        long_about = "Print the versioned JSON Schema of the --json output envelope, including every result \
                      type in $defs. Without --json the raw schema document is printed.",
        after_help = "Examples:\n  iris schema > iris-output.v1.schema.json\n  iris schema --json"
    )]
    Schema,
    /// Print a shell completion script
    #[command(
        long_about = "Print a completion script for bash, zsh, fish, or elvish on stdout.",
        after_help = "Examples:\n  iris completions bash > ~/.local/share/bash-completion/completions/iris\n  \
                      iris completions zsh > \"${fpath[1]}/_iris\"\n  iris completions fish > \
                      ~/.config/fish/completions/iris.fish"
    )]
    Completions(CompletionsArgs),
    /// Show version, build target, and JSON schema version
    #[command(
        long_about = "Show the Iris version, the target it was built for, and the JSON output schema \
                            version.",
        after_help = "Examples:\n  iris version\n  iris version --json"
    )]
    Version,
}

impl Command {
    /// The envelope `command` name.
    pub fn name(&self) -> CommandName {
        match self {
            Command::Image(ImageCommand::Generate(_)) => CommandName::ImageGenerate,
            Command::Image(ImageCommand::Edit(_)) => CommandName::ImageEdit,
            Command::Video(VideoCommand::Generate(_)) => CommandName::VideoGenerate,
            Command::Jobs(JobsCommand::List(_)) => CommandName::JobsList,
            Command::Jobs(JobsCommand::Status(_)) => CommandName::JobsStatus,
            Command::Jobs(JobsCommand::Wait(_)) => CommandName::JobsWait,
            Command::Jobs(JobsCommand::Download(_)) => CommandName::JobsDownload,
            Command::Jobs(JobsCommand::Delete(_)) => CommandName::JobsDelete,
            Command::Models(ModelsCommand::List(_)) => CommandName::ModelsList,
            Command::Models(ModelsCommand::Show(_)) => CommandName::ModelsShow,
            Command::Providers(ProvidersCommand::List) => CommandName::ProvidersList,
            Command::Config(ConfigCommand::Show) => CommandName::ConfigShow,
            Command::Config(ConfigCommand::Path) => CommandName::ConfigPath,
            Command::Doctor(_) => CommandName::Doctor,
            Command::Schema => CommandName::Schema,
            Command::Completions(_) => CommandName::Completions,
            Command::Version => CommandName::Version,
        }
    }
}

// ----- generation -----------------------------------------------------------

/// `-m/--model` help of `video generate` (the image commands' help names `image.model`).
const VIDEO_MODEL_HELP: &str =
    "Model id or alias (see `iris models list`); required unless the config file sets video.model";

/// Exactly one prompt source.
#[derive(Debug, Args)]
#[command(next_help_heading = "Prompt (exactly one source)")]
pub struct PromptArgs {
    /// Prompt text
    #[arg(value_name = "PROMPT")]
    pub prompt: Option<OsString>,
    /// Read the prompt from a UTF-8 file (trailing whitespace is trimmed)
    #[arg(short = 'f', long, value_name = "PATH")]
    pub prompt_file: Option<PathBuf>,
    /// Read the prompt from standard input (must not be a terminal)
    #[arg(long)]
    pub prompt_stdin: bool,
}

/// Model selection. The provider is the model's.
#[derive(Debug, Args)]
#[command(next_help_heading = "Model")]
pub struct ModelArgs {
    /// Model id or alias (see `iris models list`); required unless the config file sets image.model
    // `video generate` replaces this help with its own config key (`VIDEO_MODEL_HELP`).
    #[arg(short = 'm', long, value_name = "MODEL")]
    pub model: Option<String>,
    /// Allow an unknown --model by declaring that it has this known model's capabilities
    #[arg(long, value_name = "KNOWN_MODEL")]
    pub capabilities_from: Option<String>,
}

/// Typed option flags of the image commands and the catalog option each sets. Each
/// is accepted only if the resolved model declares that option for the operation;
/// every flag here is declared by at least one catalog image model (a test checks
/// it), and other options are reachable with `-O name=value`.
pub const IMAGE_FLAGS: &[(&str, &str)] = &[
    ("--count", "count"),
    ("--size", "size"),
    ("--aspect-ratio", "aspect_ratio"),
    ("--resolution", "resolution"),
    ("--quality", "quality"),
    ("--format", "format"),
];

/// Typed option flags of `video generate` and the catalog option each sets (see
/// [`IMAGE_FLAGS`] for the rules).
pub const VIDEO_FLAGS: &[(&str, &str)] = &[
    ("--count", "count"),
    ("--duration", "duration"),
    ("--resolution", "resolution"),
    ("--aspect-ratio", "aspect_ratio"),
    ("--negative-prompt", "negative_prompt"),
];

/// The option flags one generation command was given: each typed flag with its
/// catalog option name and value (if set), and the `-O KEY=VALUE` items.
#[derive(Debug, Clone)]
pub struct OptionFlags<'a> {
    pub typed: Vec<(&'static str, &'static str, Option<&'a String>)>,
    pub generic: &'a [String],
}

/// Typed image options (each accepted only if the model declares it) and `-O`.
#[derive(Debug, Args, Default)]
#[command(next_help_heading = "Generation options (validated against the model)")]
pub struct ImageOptions {
    /// Number of images
    #[arg(short = 'n', long, value_name = "N")]
    pub count: Option<String>,
    /// Output size, WxH or auto
    #[arg(long, value_name = "WxH|auto")]
    pub size: Option<String>,
    /// Aspect ratio, e.g. 16:9
    #[arg(long, value_name = "W:H")]
    pub aspect_ratio: Option<String>,
    /// Resolution class, e.g. 1K or 2K
    #[arg(long, value_name = "R")]
    pub resolution: Option<String>,
    /// Quality level, e.g. low, medium, high
    #[arg(long, value_name = "Q")]
    pub quality: Option<String>,
    /// Image format: png, jpeg, or webp
    #[arg(long, value_name = "FORMAT")]
    pub format: Option<String>,
    /// Provider-specific option KEY=VALUE (repeatable); see `iris models show <MODEL>`
    #[arg(short = 'O', long = "option", value_name = "KEY=VALUE")]
    pub options: Vec<String>,
}

impl ImageOptions {
    /// The flags as given, in [`IMAGE_FLAGS`] order.
    pub fn flags(&self) -> OptionFlags<'_> {
        let values = [
            self.count.as_ref(),
            self.size.as_ref(),
            self.aspect_ratio.as_ref(),
            self.resolution.as_ref(),
            self.quality.as_ref(),
            self.format.as_ref(),
        ];
        OptionFlags {
            typed: IMAGE_FLAGS.iter().zip(values).map(|(&(flag, name), value)| (flag, name, value)).collect(),
            generic: &self.options,
        }
    }
}

/// Typed video options (each accepted only if the model declares it) and `-O`.
#[derive(Debug, Args, Default)]
#[command(next_help_heading = "Generation options (validated against the model)")]
pub struct VideoOptions {
    /// Number of videos
    #[arg(short = 'n', long, value_name = "N")]
    pub count: Option<String>,
    /// Video length in seconds
    #[arg(long, value_name = "SECONDS")]
    pub duration: Option<String>,
    /// Resolution, e.g. 720p or 1080p
    #[arg(long, value_name = "R")]
    pub resolution: Option<String>,
    /// Aspect ratio, e.g. 16:9
    #[arg(long, value_name = "W:H")]
    pub aspect_ratio: Option<String>,
    /// What the video should not contain
    #[arg(long, value_name = "TEXT")]
    pub negative_prompt: Option<String>,
    /// Provider-specific option KEY=VALUE (repeatable); see `iris models show <MODEL>`
    #[arg(short = 'O', long = "option", value_name = "KEY=VALUE")]
    pub options: Vec<String>,
}

impl VideoOptions {
    /// The flags as given, in [`VIDEO_FLAGS`] order.
    pub fn flags(&self) -> OptionFlags<'_> {
        let values = [
            self.count.as_ref(),
            self.duration.as_ref(),
            self.resolution.as_ref(),
            self.aspect_ratio.as_ref(),
            self.negative_prompt.as_ref(),
        ];
        OptionFlags {
            typed: VIDEO_FLAGS.iter().zip(values).map(|(&(flag, name), value)| (flag, name, value)).collect(),
            generic: &self.options,
        }
    }
}

/// Where to save outputs.
#[derive(Debug, Args, Default)]
#[command(next_help_heading = "Output")]
pub struct OutputArgs {
    /// Exact output file; with several outputs: <stem>-<i>.<ext>
    #[arg(short = 'o', long, value_name = "PATH", conflicts_with = "out_dir")]
    pub output: Option<PathBuf>,
    /// Output directory (created if missing); default: IRIS_OUTPUT_DIR, config output_dir, or the current directory
    #[arg(short = 'd', long, value_name = "DIR")]
    pub out_dir: Option<PathBuf>,
    /// Replace existing files (default: refuse with output_exists)
    #[arg(long)]
    pub overwrite: bool,
}

#[derive(Debug, Subcommand)]
pub enum ImageCommand {
    /// Generate images from a prompt
    #[command(
        long_about = "Generate images from a text prompt. Synchronous: the paid request is sent once (never \
                      retried after it may have been processed), and every returned image is validated and \
                      saved before the command returns. Local validation (model, options, output paths, \
                      credentials) happens before anything is sent.",
        after_help = "Examples:\n  iris image generate -m gpt-image-2.5-sunburst --quality low --size 1024x1024 \
                      \"a fox\" -o fox.png\n  iris image generate -m gpt-image-2 --quality low --size 1024x1024 \
                      \"a red kite\" --dry-run --json\n  iris image generate -m nano-banana-2 --aspect-ratio 16:9 -f \
                      prompt.txt -d out/\n  echo \"a lighthouse\" | iris image generate -m nano-banana-2 \
                      --prompt-stdin --json\n\nProvider usage is billed by the provider."
    )]
    Generate(ImageGenerateArgs),
    /// Edit images, or compose a new image from reference images
    #[command(
        long_about = "Edit one or more local images, or compose a new image from reference images, guided \
                      by a prompt. Inputs are validated locally (format, size, count) before anything is \
                      sent. --mask is accepted only by models that declare mask support.",
        after_help = "Examples:\n  iris image edit -m nano-banana-2 -i photo.png \"make it autumn\" -o fall.jpg\n  \
                      iris image edit -m nano-banana-2 -i a.png -i b.png \"combine these into one poster\"\n  \
                      iris image edit -m gpt-image-2.5-sunburst --quality low --size 1024x1024 -i room.png \\\n    \
                      --mask mask.png \"add a window\" --json\n\nProvider usage is billed by the provider."
    )]
    Edit(ImageEditArgs),
}

#[derive(Debug, Args)]
pub struct ImageGenerateArgs {
    #[command(flatten)]
    pub prompt: PromptArgs,
    #[command(flatten)]
    pub model: ModelArgs,
    #[command(flatten)]
    pub options: ImageOptions,
    #[command(flatten)]
    pub output: OutputArgs,
    /// Validate everything locally and print the plan; nothing is sent or charged
    #[arg(long, help_heading = "Execution")]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct ImageEditArgs {
    /// Input image (repeatable; at least one)
    #[arg(short = 'i', long = "image", value_name = "PATH", required = true, help_heading = "Inputs")]
    pub images: Vec<PathBuf>,
    /// Mask image (only for models that declare mask support)
    #[arg(long, value_name = "PATH", help_heading = "Inputs")]
    pub mask: Option<PathBuf>,
    #[command(flatten)]
    pub prompt: PromptArgs,
    #[command(flatten)]
    pub model: ModelArgs,
    #[command(flatten)]
    pub options: ImageOptions,
    #[command(flatten)]
    pub output: OutputArgs,
    /// Validate everything locally and print the plan; nothing is sent or charged
    #[arg(long, help_heading = "Execution")]
    pub dry_run: bool,
}

#[derive(Debug, Subcommand)]
pub enum VideoCommand {
    /// Generate a video (waits and saves by default; --detach to submit and return)
    #[command(
        long_about = "Generate a video as a provider-native asynchronous job. Iris writes a local job record \
                      before submitting, then waits for the job and saves the video. With --detach it \
                      returns right after submission; resume with `iris jobs wait <JOB_ID>` from any later \
                      process.\n\nIf the caller's wait limit (--timeout) passes or you press Ctrl-C, the job \
                      keeps running remotely and stays resumable (exit 4 or 130). If the outcome of the \
                      submission itself is uncertain, Iris exits 5 and never resubmits automatically.",
        after_help = "Examples:\n  iris video generate -m veo-lite \"waves at dusk\" --duration 4 -o waves.mp4\n  \
                      iris video generate -m veo-lite \"a paper boat\" --detach --json\n  iris video generate \
                      -m veo-lite --image first.png \"the scene comes alive\" --timeout 15m\n  iris video \
                      generate -m veo-lite \"city timelapse\" --dry-run --json\n\nProvider usage is billed by \
                      the provider.",
        mut_arg("model", |arg| arg.help(VIDEO_MODEL_HELP))
    )]
    Generate(VideoGenerateArgs),
}

#[derive(Debug, Args)]
pub struct VideoGenerateArgs {
    /// First frame image (image-to-video), if the model supports it
    #[arg(long = "image", value_name = "PATH", help_heading = "Inputs")]
    pub first_frame: Option<PathBuf>,
    /// Last frame image, if the model supports it
    #[arg(long, value_name = "PATH", help_heading = "Inputs")]
    pub last_frame: Option<PathBuf>,
    /// Reference image (repeatable), if the model supports it
    #[arg(long = "ref", value_name = "PATH", help_heading = "Inputs")]
    pub references: Vec<PathBuf>,
    #[command(flatten)]
    pub prompt: PromptArgs,
    #[command(flatten)]
    pub model: ModelArgs,
    #[command(flatten)]
    pub options: VideoOptions,
    #[command(flatten)]
    pub output: OutputArgs,
    /// Submit, record the job, print its id, and return without waiting
    #[arg(long, conflicts_with_all = ["timeout", "poll_interval"], help_heading = "Waiting")]
    pub detach: bool,
    /// Caller wait limit (e.g. 90s, 10m, 1h, or seconds); the job continues remotely after it
    #[arg(long, value_name = "DURATION", help_heading = "Waiting")]
    pub timeout: Option<String>,
    /// Time between status checks (at least 2s)
    #[arg(long, value_name = "DURATION", help_heading = "Waiting")]
    pub poll_interval: Option<String>,
    /// Validate everything locally and print the plan; nothing is sent or charged
    #[arg(long, help_heading = "Execution")]
    pub dry_run: bool,
}

// ----- jobs -----------------------------------------------------------------

#[derive(Debug, Subcommand)]
pub enum JobsCommand {
    /// List local job records (newest first)
    #[command(
        long_about = "List local job records, newest first. Records that cannot be read are skipped with a \
                      job_record_unreadable warning.",
        after_help = "Examples:\n  iris jobs list\n  iris jobs list --status running --json\n  iris jobs list \
                      --provider gemini --limit 5"
    )]
    List(JobsListArgs),
    /// Show one job, refreshing its remote status once
    #[command(
        long_about = "Show one job. A running job's remote status is checked once (a free status call) and \
                      recorded, unless --no-refresh is given.",
        after_help = "Examples:\n  iris jobs status job_01jbz9k3m4n5p6q7r8s9t0v1w2\n  iris jobs status \
                      job_01jbz9k3m4n5p6q7r8s9t0v1w2 --no-refresh --json"
    )]
    Status(JobsStatusArgs),
    /// Wait for a job to finish, then download its outputs
    #[command(
        long_about = "Wait for a job to finish, then download its outputs (unless --no-download). Works from \
                      any process: the job is resumed from its local record. The wait limit and Ctrl-C only \
                      stop waiting; the job keeps running remotely (exit 4 or 130). With --overwrite, \
                      outputs downloaded earlier are fetched again and replace the saved files.",
        after_help = "Examples:\n  iris jobs wait job_01jbz9k3m4n5p6q7r8s9t0v1w2\n  iris jobs wait \
                      job_01jbz9k3m4n5p6q7r8s9t0v1w2 --timeout 30m -d videos/ --json\n  iris jobs wait \
                      job_01jbz9k3m4n5p6q7r8s9t0v1w2 --no-download"
    )]
    Wait(JobsWaitArgs),
    /// Download the outputs of a finished job (never resubmits)
    #[command(
        long_about = "Download the outputs of a succeeded job. Nothing is ever regenerated or resubmitted. \
                      Repeating a download is safe: an intact file already at the target is reported as \
                      already_downloaded, and an intact earlier download is copied locally. With \
                      --overwrite, every output is fetched from the provider again and replaces the saved \
                      file atomically.",
        after_help = "Examples:\n  iris jobs download job_01jbz9k3m4n5p6q7r8s9t0v1w2\n  iris jobs download \
                      job_01jbz9k3m4n5p6q7r8s9t0v1w2 -o clip.mp4 --json"
    )]
    Download(JobsDownloadArgs),
    /// Delete LOCAL job records (no remote cancellation or deletion)
    #[command(
        long_about = "Delete local job records only. Remote jobs are not cancelled and downloaded files are \
                      not deleted. Deletion is all or nothing. Without --force, jobs that are still \
                      submitting or running, and succeeded jobs whose outputs were not downloaded while the \
                      provider still keeps them, are refused (they would become unrecoverable). With --all \
                      --force, unreadable job record files are deleted too.",
        after_help = "Examples:\n  iris jobs delete job_01jbz9k3m4n5p6q7r8s9t0v1w2\n  iris jobs delete --all\n  \
                      iris jobs delete --all --force --json"
    )]
    Delete(JobsDeleteArgs),
}

#[derive(Debug, Args)]
pub struct JobsListArgs {
    /// Only jobs with this status (submitting, submission_unknown, running, succeeded, failed, expired)
    #[arg(long, value_name = "STATUS")]
    pub status: Option<String>,
    /// Only jobs of this provider
    #[arg(long, value_name = "PROVIDER")]
    pub provider: Option<String>,
    /// At most this many jobs
    #[arg(long, value_name = "N")]
    pub limit: Option<usize>,
}

#[derive(Debug, Args)]
pub struct JobsStatusArgs {
    /// Job id (job_ followed by 26 lowercase letters or digits)
    #[arg(value_name = "JOB_ID")]
    pub job_id: String,
    /// Show the local record without checking the provider
    #[arg(long)]
    pub no_refresh: bool,
}

#[derive(Debug, Args)]
pub struct JobsWaitArgs {
    /// Job id
    #[arg(value_name = "JOB_ID")]
    pub job_id: String,
    /// Caller wait limit (e.g. 90s, 10m, 1h, or seconds); the job continues remotely after it
    #[arg(long, value_name = "DURATION")]
    pub timeout: Option<String>,
    /// Time between status checks (at least 2s)
    #[arg(long, value_name = "DURATION")]
    pub poll_interval: Option<String>,
    /// Only wait; do not download the outputs
    #[arg(long, conflicts_with_all = ["output", "out_dir", "overwrite"])]
    pub no_download: bool,
    #[command(flatten)]
    pub output: OutputArgs,
}

#[derive(Debug, Args)]
pub struct JobsDownloadArgs {
    /// Job id
    #[arg(value_name = "JOB_ID")]
    pub job_id: String,
    #[command(flatten)]
    pub output: OutputArgs,
}

#[derive(Debug, Args)]
pub struct JobsDeleteArgs {
    /// Job ids to delete
    #[arg(value_name = "JOB_ID", required_unless_present = "all", conflicts_with = "all")]
    pub job_ids: Vec<String>,
    /// Delete every local job record
    #[arg(long)]
    pub all: bool,
    /// Also delete jobs that are still submitting or running, or whose outputs were not downloaded
    /// (with --all: also unreadable job records)
    #[arg(long)]
    pub force: bool,
}

// ----- models, providers, config, doctor, completions -----------------------

#[derive(Debug, Subcommand)]
pub enum ModelsCommand {
    /// List known models
    #[command(
        long_about = "List the models in Iris's catalog with their provider, lifecycle, operations, and \
                      aliases. It reads only the catalog, so it runs even when the config file is invalid.",
        after_help = "Examples:\n  iris models list\n  iris models list --provider gemini --json\n  iris models \
                      list --operation image.edit"
    )]
    List(ModelsListArgs),
    /// Show one model's capabilities, options, and prices
    #[command(
        long_about = "Show one model's declared capabilities: operations, inputs, options (with the typed \
                      flag or -O key and the default of each), output types, limits, published prices, and \
                      documented access requirements. Without --check-access it reads only the catalog and \
                      whether the API key is set, so it runs even when the config file is invalid. \
                      --check-access also asks the provider with a free metadata call whether the model is \
                      visible to your key; billing tier, prepaid credit, and organization verification are \
                      not checked.",
        after_help = "Examples:\n  iris models show nano-banana-2\n  iris models show gpt-image-2 --json\n  iris \
                      models show veo-fast --check-access"
    )]
    Show(ModelsShowArgs),
}

#[derive(Debug, Args)]
pub struct ModelsListArgs {
    /// Only models of this provider
    #[arg(long, value_name = "PROVIDER")]
    pub provider: Option<String>,
    /// Only models supporting this operation (image.generate, image.edit, video.generate)
    #[arg(long, value_name = "OPERATION")]
    pub operation: Option<String>,
}

#[derive(Debug, Args)]
pub struct ModelsShowArgs {
    /// Model id or alias
    #[arg(value_name = "MODEL")]
    pub model: String,
    /// Check with a free metadata call whether the model is visible to your key (needs the provider's API key)
    #[arg(long)]
    pub check_access: bool,
}

#[derive(Debug, Subcommand)]
pub enum ProvidersCommand {
    /// List providers, credential variables, presence, and base URLs
    #[command(
        long_about = "List every provider Iris supports with its operations, the environment variable its \
                      API key is read from (OPENAI_API_KEY or GEMINI_API_KEY), whether that variable is set \
                      (never its value), and the base URL requests go to. The key is sent to that base URL, \
                      including an override from IRIS_OPENAI_BASE_URL, IRIS_GEMINI_BASE_URL, or the config \
                      file.",
        after_help = "Examples:\n  iris providers list\n  iris providers list --json"
    )]
    List,
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Show effective settings and the source of each value
    #[command(
        long_about = "Show every non-secret setting Iris would use, with its value and where it came from: a \
                      command-line flag, an environment variable (named), the config file, or the built-in \
                      default. Credentials are listed by presence only, never by value. A non-default \
                      provider base URL is flagged with a warning, since API keys are sent there.",
        after_help = "Examples:\n  iris config show\n  iris --config ./iris.toml config show --json"
    )]
    Show,
    /// Show the config file, state directory, and jobs directory paths
    #[command(
        long_about = "Show the absolute paths Iris uses: the config file (--config, IRIS_CONFIG, or the \
                      platform default; it need not exist), the state directory (IRIS_STATE_DIR, config \
                      state_dir, or the platform default), and the jobs directory inside it where local job \
                      records are kept.",
        after_help = "Examples:\n  iris config path\n  iris config path --json"
    )]
    Path,
}

#[derive(Debug, Args)]
pub struct DoctorArgs {
    /// Also check with free metadata calls whether each model of a provider whose API key is set is
    /// visible to your key
    #[arg(long)]
    pub check_access: bool,
}

#[derive(Debug, Args)]
pub struct CompletionsArgs {
    /// Shell to generate completions for
    #[arg(value_enum, value_name = "SHELL")]
    pub shell: Shell,
}

/// Shells with completion support.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
    Elvish,
}

impl Shell {
    pub fn name(self) -> &'static str {
        match self {
            Shell::Bash => "bash",
            Shell::Zsh => "zsh",
            Shell::Fish => "fish",
            Shell::Elvish => "elvish",
        }
    }

    pub fn generator(self) -> clap_complete::Shell {
        match self {
            Shell::Bash => clap_complete::Shell::Bash,
            Shell::Zsh => clap_complete::Shell::Zsh,
            Shell::Fish => clap_complete::Shell::Fish,
            Shell::Elvish => clap_complete::Shell::Elvish,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn command_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    /// The command lines of an "Examples:" section: the indented lines after it, up
    /// to the first blank line, a line ending in `\` continued on the next.
    fn examples(help: &str) -> Vec<String> {
        let Some((_, rest)) = help.split_once("Examples:\n") else {
            return Vec::new();
        };
        let (mut lines, mut line) = (Vec::new(), String::new());
        for l in rest.lines().take_while(|l| !l.trim().is_empty()) {
            match l.trim().strip_suffix('\\') {
                Some(head) => line.push_str(head),
                None => lines.push(std::mem::take(&mut line) + l.trim()),
            }
        }
        lines
    }

    /// Split a command line into words the way a POSIX shell would for these
    /// examples: whitespace separates words, and single or double quotes (with `\"`
    /// inside double quotes) group them.
    fn shell_words(line: &str) -> Vec<String> {
        let (mut words, mut word, mut quote, mut in_word) = (Vec::new(), String::new(), None, false);
        let mut chars = line.chars();
        while let Some(c) = chars.next() {
            match (quote, c) {
                (None, c) if c.is_whitespace() => {
                    if in_word {
                        words.push(std::mem::take(&mut word));
                        in_word = false;
                    }
                }
                (None, '"' | '\'') => (quote, in_word) = (Some(c), true),
                (Some(q), c) if c == q => quote = None,
                (Some('"'), '\\') => word.extend(chars.next()),
                (_, c) => {
                    word.push(c);
                    in_word = true;
                }
            }
        }
        assert!(quote.is_none(), "unbalanced quotes in {line:?}");
        if in_word {
            words.push(word);
        }
        words
    }

    /// Every help page's examples parse: each command line (the part after the last
    /// `|`, without shell redirections) is accepted by the command-line parser. Each
    /// generation example names its model with `-m`, since Iris never chooses one and
    /// an example cannot rely on a config file.
    #[test]
    fn every_help_example_parses_and_names_its_model() {
        fn collect(cmd: &clap::Command, page: &str, out: &mut Vec<(String, String)>) {
            for help in [cmd.get_after_help(), cmd.get_after_long_help()].into_iter().flatten() {
                out.extend(examples(&help.to_string()).into_iter().map(|e| (page.to_string(), e)));
            }
            for sub in cmd.get_subcommands() {
                collect(sub, &format!("{page} {}", sub.get_name()), out);
            }
        }
        let mut all = Vec::new();
        collect(&Cli::command(), "iris", &mut all);
        assert!(all.len() >= 40, "found only {} examples", all.len());
        for (page, line) in all {
            let words = shell_words(&line);
            let words = match words.iter().rposition(|w| w == "|") {
                Some(pipe) => words[pipe + 1..].to_vec(),
                None => words,
            };
            let mut argv: Vec<String> = Vec::new();
            let mut words = words.into_iter();
            while let Some(word) = words.next() {
                match word.as_str() {
                    ">" | ">>" | "<" | "2>" => {
                        words.next();
                    }
                    w if w.starts_with('>') || w.starts_with("2>") => {}
                    _ => argv.push(word),
                }
            }
            assert_eq!(argv.first().map(String::as_str), Some("iris"), "{page}: {line}");
            let cli = Cli::try_parse_from(&argv)
                .unwrap_or_else(|e| panic!("the example of `{page}` does not parse: {line}\n{e}"));
            let model = match &cli.command {
                Command::Image(ImageCommand::Generate(a)) => Some(&a.model),
                Command::Image(ImageCommand::Edit(a)) => Some(&a.model),
                Command::Video(VideoCommand::Generate(a)) => Some(&a.model),
                _ => None,
            };
            assert!(
                model.is_none_or(|m| m.model.is_some()),
                "the example of `{page}` names no model: {line}"
            );
        }
    }

    #[test]
    fn shell_words_follow_quotes() {
        assert_eq!(shell_words(r#"iris a "b c" 'd e' "f\"g" h"#), ["iris", "a", "b c", "d e", "f\"g", "h"]);
        assert_eq!(shell_words(r#"x > "${fpath[1]}/_iris""#), ["x", ">", "${fpath[1]}/_iris"]);
    }
}
