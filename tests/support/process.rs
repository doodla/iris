//! A temp sandbox per test and a runner for the built `iris` binary.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::Value;

use super::mock::MockApi;
use super::schema::assert_matches_schema;

/// The binary under test.
pub const BIN: &str = env!("CARGO_BIN_EXE_iris");

/// Fake credentials (never real keys). Long enough for Iris's scrubbing (≥ 8 chars)
/// and distinctive enough to search for in every output and file.
pub const OPENAI_KEY: &str = "fake-openai-key-e2e-5b1f0c7d";
pub const GEMINI_KEY: &str = "fake-gemini-key-e2e-93ad7e21";

/// Credential variables that must never reach a child from the developer's
/// environment (the child starts from an empty environment anyway).
pub const CREDENTIAL_VARS: &[&str] = &["OPENAI_API_KEY", "GEMINI_API_KEY", "GOOGLE_API_KEY"];

/// Upper bound for one `iris` invocation; the slowest scenario takes a few seconds.
pub const RUN_TIMEOUT: Duration = Duration::from_secs(90);

/// A port nothing listens on (bound, then released).
pub fn closed_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// A temp directory with `home/`, `work/` (the current directory of every run, so
/// the default output directory), and `state/` (`IRIS_STATE_DIR`). The root is
/// canonicalized so paths compare equal to the ones Iris reports (macOS `/var` →
/// `/private/var`).
pub struct Sandbox {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl Sandbox {
    pub fn new() -> Sandbox {
        let dir = tempfile::Builder::new().prefix("iris-e2e-").tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        for sub in ["home", "work", "state"] {
            std::fs::create_dir_all(root.join(sub)).unwrap();
        }
        Sandbox { _dir: dir, root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn home(&self) -> PathBuf {
        self.root.join("home")
    }
    pub fn work(&self) -> PathBuf {
        self.root.join("work")
    }
    pub fn state(&self) -> PathBuf {
        self.root.join("state")
    }
    pub fn jobs_dir(&self) -> PathBuf {
        self.state().join("jobs")
    }

    /// `work/<rel>`.
    pub fn path(&self, rel: &str) -> PathBuf {
        self.work().join(rel)
    }

    /// Write `work/<rel>` and return its path.
    pub fn write(&self, rel: &str, bytes: impl AsRef<[u8]>) -> PathBuf {
        let path = self.path(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, bytes).unwrap();
        path
    }

    /// Write a config file outside `work/` and return its path.
    pub fn config(&self, name: &str, toml: &str) -> PathBuf {
        let path = self.root.join(name);
        std::fs::write(&path, toml).unwrap();
        path
    }

    /// The job record file of `job_id`.
    pub fn record_path(&self, job_id: &str) -> PathBuf {
        self.jobs_dir().join(format!("{job_id}.json"))
    }

    /// The job record of `job_id`, parsed (panics if it is not valid JSON).
    pub fn record(&self, job_id: &str) -> Value {
        let text = std::fs::read_to_string(self.record_path(job_id)).unwrap();
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("record {job_id} is not JSON ({e}): {text}"))
    }

    /// `iris` in this sandbox with no credentials and unreachable providers.
    pub fn iris(&self) -> Iris {
        Iris::new(self)
    }
}

impl Default for Sandbox {
    fn default() -> Self {
        Sandbox::new()
    }
}

/// Sorted names of the entries of `dir` (empty if it does not exist).
pub fn files_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| rd.map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    names.sort();
    names
}

/// Every regular file below `dir`, recursively.
pub fn walk_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for entry in rd {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// One `iris` invocation: arguments, environment, stdin. Reusable: every
/// [`Iris::run`] starts a fresh process from the same description.
#[derive(Clone)]
pub struct Iris {
    args: Vec<OsString>,
    env: BTreeMap<OsString, OsString>,
    cwd: PathBuf,
    stdin: Option<Vec<u8>>,
}

impl Iris {
    pub fn new(sandbox: &Sandbox) -> Iris {
        let dead = format!("http://127.0.0.1:{}", closed_port());
        let mut env = BTreeMap::new();
        let mut set = |k: &str, v: &OsStr| {
            env.insert(OsString::from(k), v.to_os_string());
        };
        set("HOME", sandbox.home().as_os_str());
        set("IRIS_STATE_DIR", sandbox.state().as_os_str());
        set("IRIS_OPENAI_BASE_URL", OsStr::new(&format!("{dead}/v1")));
        set("IRIS_GEMINI_BASE_URL", OsStr::new(&dead));
        // Safety net: any https request (a real provider) goes to a dead proxy;
        // plain-http 127.0.0.1 mock traffic is exempt.
        set("HTTPS_PROXY", OsStr::new(&dead));
        set("https_proxy", OsStr::new(&dead));
        set("NO_PROXY", OsStr::new("127.0.0.1,localhost"));
        set("no_proxy", OsStr::new("127.0.0.1,localhost"));
        Iris { args: Vec::new(), env, cwd: sandbox.work(), stdin: None }
    }

    pub fn arg(&mut self, arg: impl AsRef<OsStr>) -> &mut Iris {
        self.args.push(arg.as_ref().to_os_string());
        self
    }

    pub fn args<I, S>(&mut self, args: I) -> &mut Iris
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        for a in args {
            self.arg(a);
        }
        self
    }

    pub fn env(&mut self, key: &str, value: impl AsRef<OsStr>) -> &mut Iris {
        self.env.insert(OsString::from(key), value.as_ref().to_os_string());
        self
    }

    pub fn env_remove(&mut self, key: &str) -> &mut Iris {
        self.env.remove(OsStr::new(key));
        self
    }

    pub fn current_dir(&mut self, dir: impl AsRef<Path>) -> &mut Iris {
        self.cwd = dir.as_ref().to_path_buf();
        self
    }

    pub fn stdin(&mut self, bytes: impl Into<Vec<u8>>) -> &mut Iris {
        self.stdin = Some(bytes.into());
        self
    }

    /// Point the OpenAI base URL at `api` (`<uri>/v1`) and set the fake key.
    pub fn openai(&mut self, api: &MockApi) -> &mut Iris {
        self.env("IRIS_OPENAI_BASE_URL", format!("{}/v1", api.uri())).env("OPENAI_API_KEY", OPENAI_KEY)
    }

    /// Point the Gemini base URL (an origin, D-04) at `api` and set the fake key.
    pub fn gemini(&mut self, api: &MockApi) -> &mut Iris {
        self.env("IRIS_GEMINI_BASE_URL", api.uri()).env("GEMINI_API_KEY", GEMINI_KEY)
    }

    /// Set both fake keys (providers stay unreachable unless a mock is attached).
    pub fn keys(&mut self) -> &mut Iris {
        self.env("OPENAI_API_KEY", OPENAI_KEY).env("GEMINI_API_KEY", GEMINI_KEY)
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(BIN);
        cmd.env_clear();
        for var in CREDENTIAL_VARS {
            cmd.env_remove(var);
        }
        cmd.envs(&self.env).args(&self.args).current_dir(&self.cwd);
        cmd
    }

    /// Run to completion (killed after [`RUN_TIMEOUT`]).
    pub fn run(&self) -> Out {
        let started = Instant::now();
        let mut cmd = assert_cmd::Command::from_std(self.command());
        cmd.timeout(RUN_TIMEOUT);
        cmd.write_stdin(self.stdin.clone().unwrap_or_default());
        let output = cmd.output().expect("run iris");
        Out {
            args: self.args.iter().map(|a| a.to_string_lossy().into_owned()).collect(),
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8(output.stdout).expect("stdout is UTF-8"),
            stderr: String::from_utf8(output.stderr).expect("stderr is UTF-8"),
            elapsed: started.elapsed(),
        }
    }

    /// Start without waiting; stdout and stderr are piped, stdin is closed.
    pub fn spawn(&self) -> Running {
        let mut cmd = self.command();
        cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd.spawn().expect("spawn iris");
        // Read both pipes on threads so the child never blocks on a full pipe;
        // stderr lines are also forwarded as they arrive.
        let mut stdout = child.stdout.take().unwrap();
        let stdout_reader = std::thread::spawn(move || {
            let mut buf = String::new();
            std::io::Read::read_to_string(&mut stdout, &mut buf).unwrap();
            buf
        });
        let stderr = child.stderr.take().unwrap();
        let (tx, lines) = mpsc::channel();
        let stderr_reader = std::thread::spawn(move || {
            let mut all = String::new();
            for line in BufReader::new(stderr).lines() {
                let line = line.unwrap();
                all.push_str(&line);
                all.push('\n');
                let _ = tx.send(line);
            }
            all
        });
        Running {
            args: self.args.iter().map(|a| a.to_string_lossy().into_owned()).collect(),
            child,
            started: Instant::now(),
            stdout_reader: Some(stdout_reader),
            stderr_reader: Some(stderr_reader),
            lines,
        }
    }
}

/// A spawned `iris` process.
pub struct Running {
    args: Vec<String>,
    child: Child,
    started: Instant,
    stdout_reader: Option<std::thread::JoinHandle<String>>,
    stderr_reader: Option<std::thread::JoinHandle<String>>,
    lines: mpsc::Receiver<String>,
}

impl Running {
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Block until a stderr line contains `needle` (panics after `timeout` or if
    /// the process closes stderr first). Returns that line.
    pub fn wait_for_stderr(&self, needle: &str, timeout: Duration) -> String {
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left) {
                Ok(line) if line.contains(needle) => return line,
                Ok(_) => {}
                Err(e) => panic!("{:?}: no stderr line containing {needle:?} ({e:?})", self.args),
            }
        }
    }

    /// Send SIGINT through the `kill` utility (as a terminal's Ctrl-C would).
    pub fn interrupt(&self) {
        let status = Command::new("kill").args(["-INT", &self.pid().to_string()]).status().unwrap();
        assert!(status.success(), "kill -INT failed");
    }

    /// Wait for exit (killed after [`RUN_TIMEOUT`]) and collect the output.
    pub fn finish(mut self) -> Out {
        let deadline = self.started + RUN_TIMEOUT;
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                panic!("{:?} did not exit within {RUN_TIMEOUT:?}", self.args);
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let stdout = self.stdout_reader.take().unwrap().join().unwrap();
        let stderr = self.stderr_reader.take().unwrap().join().unwrap();
        Out {
            args: self.args.clone(),
            code: status.code().unwrap_or(-1),
            stdout,
            stderr,
            elapsed: self.started.elapsed(),
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        // A failed assertion must not leave a child behind.
        if self.stdout_reader.is_some() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// Output of one finished invocation.
#[derive(Debug, Clone)]
pub struct Out {
    pub args: Vec<String>,
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
    pub elapsed: Duration,
}

impl Out {
    fn describe(&self) -> String {
        format!(
            "iris {:?}\nexit {}\nstdout:\n{}\nstderr:\n{}",
            self.args, self.code, self.stdout, self.stderr
        )
    }

    /// The single JSON envelope on stdout: exactly one newline-terminated line,
    /// validated against the committed schema (envelope + the command's `$defs`
    /// result type, or `ErrorBody`), with stderr hygiene checked too.
    pub fn json(&self) -> Value {
        let lines: Vec<&str> = self.stdout.lines().collect();
        assert_eq!(lines.len(), 1, "expected exactly one JSON line on stdout\n{}", self.describe());
        assert!(self.stdout.ends_with('\n'), "the envelope is newline-terminated\n{}", self.describe());
        let v: Value = serde_json::from_str(lines[0])
            .unwrap_or_else(|e| panic!("stdout is not JSON ({e})\n{}", self.describe()));
        assert_matches_schema(&v);
        assert_eq!(v["schema_version"], 1);
        self.assert_hygiene();
        v
    }

    /// Exit 0 and `ok: true`; returns the envelope.
    pub fn ok(&self) -> Value {
        assert_eq!(self.code, 0, "expected success\n{}", self.describe());
        let v = self.json();
        assert_eq!(v["ok"], true, "{}", self.describe());
        v
    }

    /// Exit `exit` and `error.code == code`; returns the envelope.
    pub fn err(&self, exit: i32, code: &str) -> Value {
        assert_eq!(self.code, exit, "expected exit {exit} ({code})\n{}", self.describe());
        let v = self.json();
        assert_eq!(v["ok"], false, "{}", self.describe());
        assert_eq!(v["error"]["code"], code, "{}", self.describe());
        v
    }

    /// Human mode success: exit 0; stdout is not JSON.
    pub fn human(&self) -> &str {
        assert_eq!(self.code, 0, "expected success\n{}", self.describe());
        assert!(!self.stdout.trim_start().starts_with('{'), "human mode printed JSON\n{}", self.describe());
        self.assert_hygiene();
        &self.stdout
    }

    /// Output hygiene of every run: no key anywhere, no terminal escapes, and no
    /// JSON envelope on stderr (diagnostics only).
    pub fn assert_hygiene(&self) {
        for key in [OPENAI_KEY, GEMINI_KEY] {
            assert!(!self.stdout.contains(key), "a key leaked to stdout\n{}", self.describe());
            assert!(!self.stderr.contains(key), "a key leaked to stderr\n{}", self.describe());
        }
        assert!(!self.stderr.contains('\u{1b}'), "terminal escapes on stderr\n{}", self.describe());
        assert!(
            !self.stderr.contains("\"schema_version\""),
            "an envelope went to stderr\n{}",
            self.describe()
        );
    }
}

/// Assert that no file below `dir` contains any of `needles` (bytes).
pub fn assert_no_file_contains(dir: &Path, needles: &[&str]) {
    for file in walk_files(dir) {
        let bytes = std::fs::read(&file).unwrap();
        for needle in needles {
            let found = bytes.windows(needle.len()).any(|w| w == needle.as_bytes());
            assert!(!found, "{} contains {needle:?}", file.display());
        }
    }
}

/// Every `http(s)://` URL printed in `text` (up to whitespace or a quote).
pub fn urls_in(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find("http") {
        let tail = &rest[i..];
        if tail.starts_with("http://") || tail.starts_with("https://") {
            let end = tail
                .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ')' | '(' | '`' | ',' | ';'))
                .unwrap_or(tail.len());
            out.push(tail[..end].trim_end_matches(['.', ':']).to_string());
            rest = &tail[end..];
        } else {
            rest = &tail[4..];
        }
    }
    out
}

/// Assert every printed URL carries no query value other than `REDACTED` (or the
/// allow-listed `alt`).
pub fn assert_printed_urls_redacted(text: &str) {
    for raw in urls_in(text) {
        let Ok(url) = url::Url::parse(&raw) else { continue };
        for (k, v) in url.query_pairs() {
            assert!(
                v == "REDACTED" || k.eq_ignore_ascii_case("alt"),
                "printed URL keeps query value {k}={v}: {raw}\nin:\n{text}"
            );
        }
    }
}
