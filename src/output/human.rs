//! Human-readable rendering (see `iris --help`; not a machine contract).
//!
//! Concise and plain: no colors, no spinners. Saved artifacts are listed on stdout
//! as `Saved <absolute path>` lines; job submissions print the job id and the
//! follow-up commands; diagnostics (model text, cost estimates, warnings, errors)
//! go to stderr. Machine consumers use `--json`.

use std::fmt::Write as _;

use serde_json::Value;

use crate::domain::{
    Artifact, Billing, CostEstimate, JobStatus, ModelSource, Operation, Warning, WarningCode,
};
use crate::providers::AccountAccess;

use super::envelope::{CommandName, ErrorBody, ResultPayload};
use super::results::*;

/// Text for stdout and stderr (either may be empty; non-empty text ends with `\n`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rendered {
    pub stdout: String,
    pub stderr: String,
}

/// Render a successful result of `command`.
pub fn render(command: Option<CommandName>, payload: &ResultPayload) -> Rendered {
    let mut r = Rendered::default();
    match payload {
        ResultPayload::Image(res) => image(res, &mut r),
        ResultPayload::Job(res) => job(command, res, &mut r),
        ResultPayload::JobList(res) => r.stdout = job_list(res),
        ResultPayload::JobDelete(res) => r.stdout = job_delete(res),
        ResultPayload::ModelList(res) => r.stdout = model_list(res),
        ResultPayload::ModelShow(res) => r.stdout = model_show(&res.model),
        ResultPayload::ProviderList(res) => r.stdout = provider_list(res),
        ResultPayload::ConfigShow(res) => r.stdout = config_show(res),
        ResultPayload::ConfigPath(res) => {
            r.stdout = format!(
                "config file: {}\nstate dir:   {}\njobs dir:    {}\n",
                res.config_file, res.state_dir, res.jobs_dir
            );
        }
        ResultPayload::Doctor(res) => r.stdout = doctor(res),
        ResultPayload::Schema(res) => r.stdout = schema_document(&res.schema),
        ResultPayload::Completions(res) => r.stdout = ensure_newline(res.script.clone()),
        ResultPayload::Version(res) => {
            r.stdout = format!(
                "{} {} ({}; JSON schema v{})\n",
                res.name, res.version, res.target, res.schema_version
            );
        }
        ResultPayload::Help(res) => r.stdout = ensure_newline(res.help.clone()),
        ResultPayload::Plan(res) => r.stdout = plan(res),
    }
    r
}

/// The published schema document exactly as committed in
/// `schema/iris-output.v1.schema.json`: pretty-printed JSON and a trailing newline.
pub fn schema_document(schema: &Value) -> String {
    let mut text = serde_json::to_string_pretty(schema).expect("schema serializes");
    text.push('\n');
    text
}

/// One warning line for stderr. A message written for the JSON result is reworded
/// where human mode shows the thing it points at somewhere else: the message of
/// `provider_text_output` points at the result's `text` field, which human mode
/// prints as a "Model text" line instead.
pub fn warning(w: &Warning) -> String {
    let message = if w.is(WarningCode::ProviderTextOutput) {
        "the model also returned text alongside the image; it is printed as \"Model text\""
    } else {
        w.message.as_str()
    };
    format!("warning[{}]: {message}\n", w.code)
}

/// An error for stderr: code and message, then hint and recovery identifiers.
pub fn error(e: &ErrorBody) -> String {
    let code =
        serde_json::to_value(e.code).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
    let mut out = format!("error[{code}]: {}\n", e.message);
    if let Some(hint) = &e.hint {
        let _ = writeln!(out, "  hint: {hint}");
    }
    if let Some(job) = &e.job_id {
        match e.job_status {
            Some(status) => {
                let _ = writeln!(out, "  job: {job} (status {})", status.as_str());
            }
            None => {
                let _ = writeln!(out, "  job: {job}");
            }
        }
    }
    if let Some(remote) = &e.remote_operation_id {
        let _ = writeln!(out, "  remote operation: {remote}");
    }
    if let Some(id) = &e.provider_request_id {
        let _ = writeln!(out, "  provider request id: {id}");
    }
    if let Some(after) = e.retry_after_seconds {
        let _ = writeln!(out, "  retry after: {after}s");
    }
    out
}

/// `Saved <path>` lines for files an error says were saved before the failure
/// (`details.saved`, e.g. the first images of a request whose later image could
/// not be saved). Empty when there are none.
pub fn saved_before_error(e: &ErrorBody) -> String {
    let saved = e.details.as_ref().and_then(|d| d.get("saved")).and_then(Value::as_array);
    saved.into_iter().flatten().filter_map(Value::as_str).map(|p| format!("Saved {p}\n")).collect()
}

fn ensure_newline(mut s: String) -> String {
    if !s.ends_with('\n') {
        s.push('\n');
    }
    s
}

fn saved_lines(artifacts: &[Artifact]) -> String {
    artifacts.iter().map(|a| format!("Saved {}\n", a.path)).collect()
}

/// A cost estimate; the basis always says how it was estimated.
fn cost(c: &CostEstimate) -> String {
    format!("{} {} ({})", amount(c), c.currency, c.basis)
}

/// The amount of a cost estimate, marked as approximate: `~$0.0059`.
fn amount(c: &CostEstimate) -> String {
    format!("~${:.4}", c.amount)
}

fn image(res: &ImageResult, r: &mut Rendered) {
    r.stdout = saved_lines(&res.artifacts);
    if let Some(text) = &res.text {
        let _ = writeln!(r.stderr, "Model text: {text}");
    }
    if let Some(c) = &res.cost_estimate {
        let _ = writeln!(r.stderr, "Estimated cost: {}", cost(c));
    }
}

fn job(command: Option<CommandName>, res: &JobResult, r: &mut Rendered) {
    let j = &res.job;
    if command == Some(CommandName::JobsStatus) {
        r.stdout = job_block(j);
    } else if !j.artifacts.is_empty() {
        r.stdout = saved_lines(&j.artifacts);
    } else {
        let verb = if command == Some(CommandName::VideoGenerate) { "Submitted job" } else { "Job" };
        let _ = writeln!(
            r.stdout,
            "{verb} {}: {} ({} {})",
            j.job_id,
            j.status.as_str(),
            j.provider.as_str(),
            j.model
        );
    }
    if command != Some(CommandName::JobsStatus) {
        for step in &res.next_steps {
            let _ = writeln!(r.stdout, "Next: {step}");
        }
    }
}

/// A model with where it came from when that was the config file:
/// `gemini-3.1-flash-image (config image.model)`. A model named with `-m/--model`
/// needs no annotation.
fn model_with_source(model: &str, source: Option<ModelSource>, op: Operation) -> String {
    match source {
        Some(ModelSource::Config) => format!("{model} (config {})", op.model_config_key()),
        Some(ModelSource::Flag) | None => model.to_string(),
    }
}

fn job_block(j: &JobView) -> String {
    let mut out = format!("{}\n", j.job_id);
    let mut field = |label: &str, value: &str| {
        let _ = writeln!(out, "  {:<11} {value}", format!("{label}:"));
    };
    field("status", j.status.as_str());
    field("provider", j.provider.as_str());
    field("model", &model_with_source(&j.model, j.model_source, j.operation));
    let prompt = &j.prompt_fingerprint;
    field("prompt", &format!("{} characters, sha256 {}", prompt.chars, prompt.sha256));
    field("created", &j.created_at);
    if let Some(t) = &j.submitted_at {
        field("submitted", t);
    }
    if let Some(t) = &j.last_checked_at {
        field("checked", t);
    }
    // Only an outcome the provider reported is a completion; a submission Iris could
    // not confirm (submission_unknown) or a job still running has none.
    if let Some(t) = &j.completed_at
        && matches!(j.status, JobStatus::Succeeded | JobStatus::Failed | JobStatus::Expired)
    {
        field("completed", t);
    }
    // The earliest time the provider may stop serving the outputs, not a deadline
    // it promises: it may keep them longer.
    if let Some(t) = &j.remote_expires_at {
        field("kept until", &format!("at least {t}"));
    }
    if let Some(op) = &j.remote_operation_id {
        field("remote op", op);
    }
    // Where a later `jobs wait` or `jobs download` given neither -o nor -d saves.
    let replacing = if j.output_plan.overwrite { ", replacing an existing file" } else { "" };
    match (&j.output_plan.path, &j.output_plan.dir) {
        (Some(path), _) => field("save to", &format!("{path}{replacing}")),
        (None, Some(dir)) => field("save to", &format!("directory {dir}{replacing}")),
        (None, None) => {}
    }
    if let Some(c) = &j.cost_estimate {
        field("cost", &cost(c));
    }
    for o in &j.outputs {
        let state = serde_json::to_value(o.download_state)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();
        let detail = match (&o.artifact, &o.last_error) {
            (Some(a), _) => format!("{state} {} ({} bytes)", a.path, a.bytes),
            (None, Some(e)) => format!("{state} ({})", e.message),
            (None, None) => state,
        };
        field(&format!("output {}", o.index), &detail);
    }
    if let Some(e) = &j.error {
        field("error", &e.message);
    }
    out
}

fn table(headers: &[&str], rows: &[Vec<String>]) -> String {
    let mut widths: Vec<usize> = headers.iter().map(|h| h.chars().count()).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if let Some(w) = widths.get_mut(i) {
                *w = (*w).max(cell.chars().count());
            }
        }
    }
    let line = |cells: Vec<&str>| {
        let last = cells.len().saturating_sub(1);
        let mut s = String::new();
        for (i, cell) in cells.iter().enumerate() {
            if i == last {
                s.push_str(cell);
            } else {
                let _ = write!(s, "{:<width$}  ", cell, width = widths[i]);
            }
        }
        s.trim_end().to_string() + "\n"
    };
    let mut out = line(headers.to_vec());
    for row in rows {
        out.push_str(&line(row.iter().map(String::as_str).collect()));
    }
    out
}

fn job_list(res: &JobListResult) -> String {
    if res.jobs.is_empty() {
        return "No jobs.\n".to_string();
    }
    let rows: Vec<Vec<String>> = res
        .jobs
        .iter()
        .map(|j| {
            vec![
                j.job_id.clone(),
                j.status.as_str().to_string(),
                j.provider.as_str().to_string(),
                j.model.clone(),
                j.created_at.clone(),
            ]
        })
        .collect();
    table(&["JOB ID", "STATUS", "PROVIDER", "MODEL", "CREATED"], &rows)
}

fn job_delete(res: &JobDeleteResult) -> String {
    let mut out: String = res.deleted.iter().map(|id| format!("Deleted {id}\n")).collect();
    if res.deleted.is_empty() {
        out.push_str("No jobs deleted.\n");
    }
    let _ = writeln!(out, "{}", res.note);
    out
}

fn join<T: AsRef<str>>(items: &[T]) -> String {
    items.iter().map(AsRef::as_ref).collect::<Vec<_>>().join(", ")
}

fn ops<T: serde::Serialize>(items: &[T]) -> String {
    let names: Vec<String> = items
        .iter()
        .filter_map(|o| serde_json::to_value(o).ok().and_then(|v| v.as_str().map(str::to_string)))
        .collect();
    if names.is_empty() { "-".to_string() } else { names.join(", ") }
}

fn lifecycle<T: serde::Serialize>(l: &T) -> String {
    serde_json::to_value(l).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
}

/// One row per model, each followed by its summary, its billing, and the estimate of
/// its cheapest single-output request with the options that give it, indented and
/// wrapped at [`NOTE_WIDTH`] columns.
fn model_list(res: &ModelListResult) -> String {
    if res.models.is_empty() {
        return "No models in the catalog.\n".to_string();
    }
    let rows: Vec<Vec<String>> = res
        .models
        .iter()
        .map(|m| {
            vec![
                m.id.clone(),
                m.provider.as_str().to_string(),
                lifecycle(&m.lifecycle),
                ops(&m.operations),
                if m.aliases.is_empty() { "-".to_string() } else { join(&m.aliases) },
            ]
        })
        .collect();
    let table = table(&["MODEL", "PROVIDER", "LIFECYCLE", "OPERATIONS", "ALIASES"], &rows);
    let mut lines = table.lines();
    let mut out = format!("{}\n", lines.next().unwrap_or_default());
    for (line, m) in lines.zip(&res.models) {
        let cost = match &m.lowest_estimate {
            Some(lowest) => {
                let options: Vec<String> =
                    lowest.options.iter().map(|(name, value)| format!("{name}={value}")).collect();
                let with = if options.is_empty() { "the defaults".to_string() } else { options.join(" ") };
                format!("cheapest single-output request: {} with {with}", amount(&lowest.cost_estimate))
            }
            None => "no estimate before the call".to_string(),
        };
        let _ = writeln!(out, "{line}");
        out.push_str(&wrapped(&m.summary, "  ", NOTE_WIDTH));
        out.push_str(&wrapped(&format!("{}; {cost}", m.billing), "  ", NOTE_WIDTH));
    }
    out
}

/// Width of the indented notes under a table row.
const NOTE_WIDTH: usize = 100;

/// `text` in lines of at most `width` columns, each starting with `indent` (a word
/// longer than a line keeps a line of its own).
fn wrapped(text: &str, indent: &str, width: usize) -> String {
    let mut out = String::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && indent.len() + line.chars().count() + 1 + word.chars().count() > width {
            let _ = writeln!(out, "{indent}{line}");
            line.clear();
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        let _ = writeln!(out, "{indent}{line}");
    }
    out
}

fn model_show(m: &ModelCapabilities) -> String {
    let mut out =
        format!("{} ({}; {}, {})\n", m.id, m.display_name, m.provider.as_str(), lifecycle(&m.lifecycle));
    let mut field = |label: &str, value: String| {
        let _ = writeln!(out, "  {:<12} {value}", format!("{label}:"));
    };
    field("summary", m.summary.clone());
    if !m.aliases.is_empty() {
        field("aliases", join(&m.aliases));
    }
    field("operations", ops(&m.operations));
    field("billing", billing(m.billing));
    let i = &m.inputs;
    if i.max_input_images > 0 || i.first_frame || i.last_frame || i.max_reference_images > 0 {
        field(
            "inputs",
            format!(
                "{} (at most {} bytes each); edit images: {}; mask: {}; first frame: {}; last frame: {}; reference \
                 images: {}",
                join(&i.input_media_types),
                i.max_input_bytes,
                i.max_input_images,
                yes_no(i.mask),
                yes_no(i.first_frame),
                yes_no(i.last_frame),
                i.max_reference_images
            ),
        );
    }
    if let Some(mask) = &i.mask_requirements {
        let mut rules = vec![join(&mask.media_types), format!("at most {} bytes", mask.max_bytes)];
        if mask.alpha_channel_required {
            rules.push("an alpha channel (transparent areas are edited)".into());
        }
        if mask.same_size_as_first_image {
            rules.push("the size of the first --image".into());
        }
        field("mask", rules.join("; "));
    }
    if let Some(max) = i.max_request_bytes {
        field("request", format!("at most {max} bytes encoded (prompt and base64 inputs)"));
    }
    field(
        "outputs",
        format!("{} (at most {} per request)", join(&m.outputs.media_types), m.outputs.max_count),
    );
    field(
        "prompt",
        m.limits
            .max_prompt_chars
            .map(|n| format!("at most {n} characters"))
            .unwrap_or_else(|| "no documented limit".into()),
    );
    let _ = writeln!(out, "  options:");
    if m.options.is_empty() {
        let _ = writeln!(out, "    (none)");
    }
    for o in &m.options {
        let how = o.flag.clone().unwrap_or_else(|| format!("-O {}=…", o.name));
        let values = match (&o.values, o.min, o.max, &o.syntax) {
            (Some(v), ..) => v.iter().map(ToString::to_string).collect::<Vec<_>>().join("|"),
            (None, Some(min), Some(max), _) => format!("{min}..={max}"),
            (None, _, _, Some(syntax)) => syntax.clone(),
            _ => o.kind.clone(),
        };
        let default = o.default.as_ref().map(|d| format!(" (default {d})")).unwrap_or_default();
        let _ = writeln!(out, "    {how}: {values}{default} [{}]", ops(&o.operations));
        if !o.description.is_empty() {
            let _ = writeln!(out, "      {}", o.description);
        }
    }
    if !m.constraints.is_empty() {
        let _ = writeln!(out, "  constraints:");
        for c in &m.constraints {
            let _ = writeln!(out, "    - {} [{}]", c.description, c.id);
        }
    }
    if !m.pricing.is_empty() {
        let _ = writeln!(out, "  pricing (published prices; Iris shows estimates only):");
        for p in &m.pricing {
            let _ = writeln!(
                out,
                "    ${} per {} — {} (as of {}, {})",
                p.usd, p.unit, p.description, p.as_of, p.source_url
            );
        }
    }
    if let Some(lowest) = &m.lowest_estimate {
        let options: Vec<String> = lowest
            .options
            .iter()
            .map(|(name, value)| {
                match m.options.iter().find(|o| &o.name == name).and_then(|o| o.flag.as_ref()) {
                    Some(flag) => format!("{flag} {value}"),
                    None => format!("-O {name}={value}"),
                }
            })
            .collect();
        let with = if options.is_empty() { "the defaults".to_string() } else { options.join(" ") };
        let c = &lowest.cost_estimate;
        let _ = writeln!(out, "  cheapest:    {} {} with {with} ({})", amount(c), c.currency, c.basis);
    }
    let a = &m.access;
    let access = match a.account_access {
        AccountAccess::Available => "visible to this key".to_string(),
        AccountAccess::Unavailable => "not visible to this key".to_string(),
        other => lifecycle(&other),
    };
    let checked = match &a.checked_at {
        Some(t) => format!(" (checked {t}; model metadata only, billing and verification not checked)"),
        None => String::new(),
    };
    let _ = writeln!(
        out,
        "  access:      {} {}; account access: {access}{checked}",
        a.credential_env,
        if a.credential_present { "is set" } else { "is not set" },
    );
    for req in &a.requirements {
        let _ = writeln!(out, "    - {req}");
    }
    let _ = writeln!(out, "  catalog:     checked {} ({})", m.catalog_as_of, m.capabilities_source);
    let _ = writeln!(out, "  docs:        {}", m.docs_url);
    out
}

/// A model's billing and what it means: `paid (requests are billed ...)`.
fn billing(b: Billing) -> String {
    format!("{b} ({})", b.description())
}

fn yes_no(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

fn provider_list(res: &ProviderListResult) -> String {
    let rows: Vec<Vec<String>> = res
        .providers
        .iter()
        .map(|p| {
            vec![
                p.id.as_str().to_string(),
                p.credential_env.clone(),
                if p.credential_present { "set".into() } else { "not set".into() },
                ops(&p.operations),
                p.base_url.clone(),
            ]
        })
        .collect();
    table(&["PROVIDER", "CREDENTIAL", "PRESENT", "OPERATIONS", "BASE URL"], &rows)
}

fn value_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "(none)".to_string(),
        other => other.to_string(),
    }
}

fn config_show(res: &ConfigShowResult) -> String {
    let mut out = format!(
        "config file: {} ({})\n",
        res.config_file,
        if res.config_file_exists { "loaded" } else { "not found" }
    );
    let rows: Vec<Vec<String>> = res
        .settings
        .iter()
        .map(|s| {
            let source = lifecycle(&s.source);
            let source = match &s.env_var {
                Some(var) if source == "env" => format!("env {var}"),
                _ => source,
            };
            vec![s.key.clone(), value_text(&s.value), source]
        })
        .collect();
    out.push_str(&table(&["SETTING", "VALUE", "SOURCE"], &rows));
    out.push_str("credentials (presence only):\n");
    for c in &res.credentials {
        let _ = writeln!(out, "  {}: {}", c.env, if c.present { "set" } else { "not set" });
    }
    out
}

fn doctor(res: &DoctorResult) -> String {
    let mut out = String::new();
    for c in &res.checks {
        let status = lifecycle(&c.status);
        let _ = writeln!(out, "{:<9} {}: {}", format!("[{status}]"), c.id, c.message);
    }
    out.push_str(if res.healthy { "Healthy.\n" } else { "Problems found (see [error] lines).\n" });
    out
}

/// A wait setting's value and, unless it is the built-in value, where it came from:
/// `20m (config video.wait_timeout)`.
fn wait_setting(s: &WaitSetting) -> String {
    let value = humantime::format_duration(std::time::Duration::from_secs_f64(s.seconds));
    match s.source {
        SettingSource::Default => value.to_string(),
        SettingSource::Flag => format!("{value} ({})", s.flag),
        SettingSource::Env => format!("{value} (env {})", s.env_var),
        SettingSource::File => format!("{value} (config {})", s.key),
    }
}

fn plan(res: &PlanResult) -> String {
    let mut out = String::from("Dry run: nothing was sent and nothing was charged.\n");
    let mut field = |label: &str, value: String| {
        let _ = writeln!(out, "  {:<11} {value}", format!("{label}:"));
    };
    field("operation", res.operation.as_str().to_string());
    field("provider", res.provider.as_str().to_string());
    field("model", model_with_source(&res.model, Some(res.model_source), res.operation));
    field("async job", yes_no(res.async_job).to_string());
    if res.async_job {
        field("detach", yes_no(res.detach).to_string());
    }
    if let Some(wait) = &res.wait {
        field(
            "wait",
            format!(
                "up to {}, polling every {}",
                wait_setting(&wait.timeout),
                wait_setting(&wait.poll_interval)
            ),
        );
    }
    field("billing", billing(res.billing));
    let options: Vec<String> = res.options.iter().map(|(k, v)| format!("{k}={}", value_text(v))).collect();
    // Explicit values plus declared defaults (the values the request runs with).
    field("options", if options.is_empty() { "(none)".into() } else { options.join(" ") });
    for i in &res.inputs {
        field("input", format!("{} {} ({}, {} bytes)", i.role, i.path, i.media_type, i.bytes));
    }
    for o in &res.outputs {
        field("output", o.clone());
    }
    field(
        "credential",
        format!(
            "{} {}",
            res.provider.credential_env(),
            if res.credential_present { "is set" } else { "is NOT set (required for the real run)" }
        ),
    );
    field(
        "cost",
        res.cost_estimate.as_ref().map(cost).unwrap_or_else(|| "no estimate available".to_string()),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_wrap_at_word_boundaries() {
        assert_eq!(wrapped("aa bb cc", "  ", 7), "  aa bb\n  cc\n");
        assert_eq!(wrapped("aa averyverylongword b", "  ", 7), "  aa\n  averyverylongword\n  b\n");
        assert_eq!(wrapped("", "  ", 7), "");
    }

    #[test]
    fn tables_align_columns_and_trim_trailing_space() {
        let t = table(&["A", "BB"], &[vec!["xyz".into(), "1".into()], vec!["q".into(), "".into()]]);
        assert_eq!(t, "A    BB\nxyz  1\nq\n");
    }

    #[test]
    fn errors_show_code_hint_and_job() {
        let e = crate::error::IrisError::new(crate::error::ErrorCode::WaitTimeout, "still running")
            .with_hint("iris jobs wait job_x")
            .with_job("job_x", Some(crate::domain::JobStatus::Running));
        let text = error(&ErrorBody::from(&e));
        assert!(text.starts_with("error[wait_timeout]: still running\n"), "{text}");
        assert!(text.contains("  hint: iris jobs wait job_x\n"));
        assert!(text.contains("  job: job_x (status running)\n"));
        assert_eq!(saved_before_error(&ErrorBody::from(&e)), "");
    }

    #[test]
    fn files_saved_before_an_error_are_listed() {
        let e = crate::error::IrisError::new(
            crate::error::ErrorCode::InvalidMedia,
            "second image is not an image",
        )
        .with_detail("saved", vec!["/w/a-1.png".to_string()]);
        assert_eq!(saved_before_error(&ErrorBody::from(&e)), "Saved /w/a-1.png\n");
    }
}
