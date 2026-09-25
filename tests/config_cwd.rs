//! Commands run from a working directory that no longer exists. Each run is a real
//! `iris` process started by `sh`, which enters a fresh directory, removes it, and
//! then executes `iris` there. Offline: no credentials, providers unreachable.

#![cfg(unix)]

mod support;

use std::path::Path;
use std::process::Command;

use serde_json::Value;
use support::*;

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Run {
    /// The single JSON envelope on stdout, validated against the committed schema.
    fn json(&self) -> Value {
        let lines: Vec<&str> = self.stdout.lines().collect();
        assert_eq!(
            lines.len(),
            1,
            "one JSON line on stdout\nstdout: {}\nstderr: {}",
            self.stdout,
            self.stderr
        );
        let v: Value = serde_json::from_str(lines[0]).unwrap();
        assert_matches_schema(&v);
        v
    }
}

/// Run `iris <args>` with its working directory deleted just before it starts.
fn run_in_deleted_dir(sb: &Sandbox, extra_env: &[(&str, &str)], args: &[&str]) -> Run {
    let gone = sb.root().join("gone");
    std::fs::create_dir(&gone).unwrap();
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(r#"cd "$1" && rmdir "$1" && shift && exec "$0" "$@""#).arg(BIN).arg(&gone).args(args);
    cmd.env_clear().current_dir(sb.root());
    for var in credential_vars() {
        cmd.env_remove(var);
    }
    cmd.env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", sb.home())
        .env("IRIS_STATE_DIR", sb.state())
        .env("HTTPS_PROXY", DEAD_URL)
        .env("NO_PROXY", "127.0.0.1,localhost");
    for &provider in iris::domain::ProviderId::ALL {
        cmd.env(provider.base_url_env(), base_url_at(provider, DEAD_URL));
    }
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("run sh");
    assert!(!gone.exists(), "the working directory was removed");
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8(out.stdout).unwrap(),
        stderr: String::from_utf8(out.stderr).unwrap(),
    }
}

#[test]
fn commands_that_need_no_working_directory_work_without_one() {
    let sb = Sandbox::new();
    for args in [
        &["--json", "version"][..],
        &["schema", "--json"],
        &["completions", "bash", "--json"],
        &["config", "path", "--json"],
        &["config", "show", "--json"],
        &["providers", "list", "--json"],
        &["models", "list", "--json"],
        &["models", "show", VEO_LITE, "--json"],
        &["jobs", "list", "--json"],
        &["doctor", "--json"],
    ] {
        let run = run_in_deleted_dir(&sb, &[], args);
        assert_eq!(run.code, 0, "{args:?}\nstdout: {}\nstderr: {}", run.stdout, run.stderr);
        assert_eq!(run.json()["ok"], true, "{args:?}");
    }
    let run = run_in_deleted_dir(&sb, &[], &["--help"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(run.stdout.contains("Usage:"), "{}", run.stdout);

    // `config show` reports the default output directory as the current directory
    // it cannot resolve, and `doctor` says it cannot be written to.
    let v = run_in_deleted_dir(&sb, &[], &["config", "show", "--json"]).json();
    let row = v["result"]["settings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["key"] == "output_dir")
        .unwrap()
        .clone();
    assert_eq!((row["value"].as_str(), row["source"].as_str()), (Some("."), Some("default")), "{v}");
    let v = run_in_deleted_dir(&sb, &[], &["doctor", "--json"]).json();
    let check =
        v["result"]["checks"].as_array().unwrap().iter().find(|c| c["id"] == "output_dir").unwrap().clone();
    assert_eq!(check["status"], "error", "{v}");
}

#[test]
fn commands_that_need_the_working_directory_fail_before_sending_anything() {
    let sb = Sandbox::new();
    let expect_cwd_error = |run: &Run, what: &str| {
        assert_eq!(run.code, 1, "{what}\nstdout: {}\nstderr: {}", run.stdout, run.stderr);
        let v = run.json();
        assert_eq!(v["error"]["code"], "io_error", "{what}: {v}");
        assert!(v["error"]["message"].as_str().unwrap().contains("current directory"), "{what}: {v}");
        assert!(v["error"]["provider_status"].is_null(), "nothing was sent: {v}");
    };
    // The default output directory is the missing current directory.
    let run = run_in_deleted_dir(&sb, &[], &["image", "generate", "a fox", "--dry-run", "--json"]);
    expect_cwd_error(&run, "default output directory");
    let run = run_in_deleted_dir(&sb, &[], &["video", "generate", "a fox", "--detach", "--json"]);
    expect_cwd_error(&run, "default output directory for a job");
    // A relative path cannot be resolved.
    let run =
        run_in_deleted_dir(&sb, &[], &["image", "generate", "a fox", "-o", "fox.png", "--dry-run", "--json"]);
    expect_cwd_error(&run, "relative -o");
    let run = run_in_deleted_dir(&sb, &[], &["--config", "iris.toml", "config", "show", "--json"]);
    expect_cwd_error(&run, "relative --config");
    assert!(sb.jobs_dir().read_dir().map_or(true, |mut d| d.next().is_none()), "no job was recorded");

    // Absolute locations need no working directory.
    let out = sb.root().join("out");
    let target = out.join("fox.png");
    let run = run_in_deleted_dir(
        &sb,
        &[],
        &["image", "generate", "a fox", "-o", target.to_str().unwrap(), "--dry-run", "--json"],
    );
    assert_eq!(run.code, 0, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.json()["result"]["outputs"][0], target.to_str().unwrap());
    let run = run_in_deleted_dir(
        &sb,
        &[("IRIS_OUTPUT_DIR", out.to_str().unwrap())],
        &["image", "generate", "a fox", "--dry-run", "--json"],
    );
    assert_eq!(run.code, 0, "{}\n{}", run.stdout, run.stderr);
    let planned = run.json()["result"]["outputs"][0].as_str().unwrap().to_string();
    assert!(Path::new(&planned).starts_with(&out), "{planned}");
}
