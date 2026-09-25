//! Configuration resolution (see docs/configuration.md): discovery, strict parsing, precedence
//! flag > env > file > default for every setting, validation, platform paths, and
//! secret handling. Uses environment snapshots only; the process environment is
//! never read for settings or mutated, and credentials are fake.

use std::path::{Path, PathBuf};
use std::time::Duration;

use iris::catalog;
use iris::config::{
    CliOverrides, EnvSnapshot, Platform, SettingSource, Settings, WARNING_NON_DEFAULT_BASE_URL,
    platform_paths,
};
use iris::domain::{Operation, ProviderId};
use iris::error::{ErrorCode, IrisError};

const FAKE_OPENAI: &str = "test-openai-key-000";
const FAKE_GEMINI: &str = "test-gemini-key-000";

struct Fixture {
    _dir: tempfile::TempDir,
    home: PathBuf,
    cwd: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let cwd = dir.path().join("work");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        Fixture { _dir: dir, home, cwd }
    }

    fn env(&self) -> EnvSnapshot {
        EnvSnapshot::new(Platform::Linux, Some(self.home.clone()), self.cwd.clone())
    }

    fn default_config_path(&self) -> PathBuf {
        self.home.join(".config/iris/config.toml")
    }

    /// Write the platform-default config file.
    fn write_default_config(&self, text: &str) -> PathBuf {
        let p = self.default_config_path();
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, text).unwrap();
        p
    }

    fn write(&self, name: &str, text: &str) -> PathBuf {
        let p = self.cwd.join(name);
        std::fs::write(&p, text).unwrap();
        p
    }
}

fn load(env: &EnvSnapshot) -> Result<Settings, IrisError> {
    Settings::load(&CliOverrides::default(), env)
}

fn load_err(cli: &CliOverrides, env: &EnvSnapshot) -> IrisError {
    Settings::load(cli, env).expect_err("expected a configuration error")
}

#[test]
fn defaults_apply_when_nothing_is_configured_and_the_default_file_is_missing() {
    let fx = Fixture::new();
    let s = load(&fx.env()).unwrap();
    assert_eq!(s.config_file.value, fx.default_config_path());
    assert_eq!(s.config_file.source, SettingSource::Default);
    assert!(!s.config_file_exists);
    assert_eq!(s.output_dir.value, fx.cwd);
    assert_eq!(s.state_dir.value, fx.home.join(".local/state/iris"));
    assert_eq!(s.jobs_dir(), fx.home.join(".local/state/iris/jobs"));
    assert_eq!(s.image_provider.value, ProviderId::OpenAi);
    assert_eq!(s.wait_timeout.value, Duration::from_secs(600));
    assert_eq!(s.poll_interval.value, Duration::from_secs(10));
    assert!(!s.store_prompts.value);
    assert_eq!(s.provider(ProviderId::OpenAi).base_url.value.as_str(), "https://api.openai.com/v1");
    assert_eq!(
        s.provider(ProviderId::Gemini).base_url.value.as_str(),
        "https://generativelanguage.googleapis.com/"
    );
    assert_eq!(s.provider(ProviderId::OpenAi).request_timeout.value, Duration::from_secs(300));
    assert_eq!(s.provider(ProviderId::Gemini).request_timeout.value, Duration::from_secs(300));
    assert_eq!(s.provider(ProviderId::Gemini).submit_timeout.value, Duration::from_secs(60));
    assert_eq!(s.provider(ProviderId::Gemini).submit_timeout.source, SettingSource::Default);
    assert_eq!(
        s.provider(ProviderId::OpenAi).image_model.value,
        catalog::default_model(ProviderId::OpenAi, Operation::ImageGenerate).map(|m| m.id.to_string())
    );
    assert_eq!(
        s.provider(ProviderId::Gemini).video_model.value,
        catalog::default_model(ProviderId::Gemini, Operation::VideoGenerate).map(|m| m.id.to_string())
    );
    assert_eq!(s.log_filter.value, "warn");
    for (key, source) in [
        ("output_dir", &s.output_dir.source),
        ("state_dir", &s.state_dir.source),
        ("image.provider", &s.image_provider.source),
        ("wait_timeout", &s.wait_timeout.source),
        ("poll_interval", &s.poll_interval.source),
        ("store_prompts", &s.store_prompts.source),
        ("openai.base_url", &s.provider(ProviderId::OpenAi).base_url.source),
        ("gemini.base_url", &s.provider(ProviderId::Gemini).base_url.source),
        ("openai.request_timeout", &s.provider(ProviderId::OpenAi).request_timeout.source),
        ("log", &s.log_filter.source),
    ] {
        assert_eq!(*source, SettingSource::Default, "{key}");
    }
    assert!(s.warnings().is_empty());
    let t = s.timeouts(ProviderId::OpenAi);
    assert_eq!(
        (t.generate, t.submit, t.poll),
        (Duration::from_secs(300), Duration::from_secs(60), Duration::from_secs(30))
    );
    // The client-level connect timeout has one source: the provider timeouts.
    let http = s.http_settings();
    assert_eq!(http.connect_timeout, t.connect);
    assert_eq!(http.connect_timeout, s.timeouts(ProviderId::Gemini).connect);
    assert_eq!(http.connect_timeout, Duration::from_secs(15));
    assert!(http.system_proxy, "the application honors the system proxy");
}

const FULL_FILE: &str = r#"
output_dir = "/file/out"
state_dir = "/file/state"

[image]
provider = "gemini"

[video]
wait_timeout = "20m"
poll_interval = 30

[jobs]
store_prompts = true

[providers.openai]
base_url = "https://file-openai.example/v1"
request_timeout = "120s"

[providers.gemini]
base_url = "https://file-gemini.example"
request_timeout = 90
"#;

fn full_env(fx: &Fixture) -> EnvSnapshot {
    fx.env()
        .with_var("IRIS_OUTPUT_DIR", "/env/out")
        .with_var("IRIS_STATE_DIR", "/env/state")
        .with_var("IRIS_IMAGE_PROVIDER", "openai")
        .with_var("IRIS_WAIT_TIMEOUT", "30m")
        .with_var("IRIS_POLL_INTERVAL", "45s")
        .with_var("IRIS_STORE_PROMPTS", "false")
        .with_var("IRIS_OPENAI_BASE_URL", "https://env-openai.example/v1")
        .with_var("IRIS_GEMINI_BASE_URL", "https://env-gemini.example")
        .with_var("IRIS_LOG", "info")
}

fn full_flags() -> CliOverrides {
    CliOverrides {
        config_path: None,
        out_dir: Some(PathBuf::from("/flag/out")),
        provider: Some(ProviderId::Gemini),
        wait_timeout: Some(Duration::from_secs(5)),
        poll_interval: Some(Duration::from_secs(3)),
        verbose: 1,
    }
}

#[test]
fn file_values_override_defaults() {
    let fx = Fixture::new();
    fx.write_default_config(FULL_FILE);
    let s = load(&fx.env()).unwrap();
    assert!(s.config_file_exists);
    assert_eq!(s.output_dir.value, Path::new("/file/out"));
    assert_eq!(s.state_dir.value, Path::new("/file/state"));
    assert_eq!(s.image_provider.value, ProviderId::Gemini);
    assert_eq!(s.wait_timeout.value, Duration::from_secs(1200));
    assert_eq!(s.poll_interval.value, Duration::from_secs(30));
    assert!(s.store_prompts.value);
    assert_eq!(s.provider(ProviderId::OpenAi).base_url.value.as_str(), "https://file-openai.example/v1");
    assert_eq!(s.provider(ProviderId::Gemini).base_url.value.host_str(), Some("file-gemini.example"));
    assert_eq!(s.provider(ProviderId::OpenAi).request_timeout.value, Duration::from_secs(120));
    assert_eq!(s.provider(ProviderId::Gemini).request_timeout.value, Duration::from_secs(90));
    assert_eq!(s.timeouts(ProviderId::Gemini).generate, Duration::from_secs(90));
    for source in [
        &s.output_dir.source,
        &s.state_dir.source,
        &s.image_provider.source,
        &s.wait_timeout.source,
        &s.poll_interval.source,
        &s.store_prompts.source,
        &s.provider(ProviderId::OpenAi).base_url.source,
        &s.provider(ProviderId::Gemini).base_url.source,
        &s.provider(ProviderId::OpenAi).request_timeout.source,
        &s.provider(ProviderId::Gemini).request_timeout.source,
    ] {
        assert_eq!(*source, SettingSource::File);
    }
}

#[test]
fn environment_overrides_the_file() {
    let fx = Fixture::new();
    fx.write_default_config(FULL_FILE);
    let s = load(&full_env(&fx)).unwrap();
    assert_eq!(s.output_dir.value, Path::new("/env/out"));
    assert_eq!(s.state_dir.value, Path::new("/env/state"));
    assert_eq!(s.image_provider.value, ProviderId::OpenAi);
    assert_eq!(s.wait_timeout.value, Duration::from_secs(1800));
    assert_eq!(s.poll_interval.value, Duration::from_secs(45));
    assert!(!s.store_prompts.value);
    assert_eq!(s.provider(ProviderId::OpenAi).base_url.value.host_str(), Some("env-openai.example"));
    assert_eq!(s.provider(ProviderId::Gemini).base_url.value.host_str(), Some("env-gemini.example"));
    assert_eq!(s.log_filter.value, "info");
    for source in [
        &s.output_dir.source,
        &s.state_dir.source,
        &s.image_provider.source,
        &s.wait_timeout.source,
        &s.poll_interval.source,
        &s.store_prompts.source,
        &s.provider(ProviderId::OpenAi).base_url.source,
        &s.provider(ProviderId::Gemini).base_url.source,
        &s.log_filter.source,
    ] {
        assert_eq!(*source, SettingSource::Env);
    }
    // Settings without an environment variable still come from the file.
    assert_eq!(s.provider(ProviderId::OpenAi).request_timeout.source, SettingSource::File);
}

#[test]
fn flags_override_the_environment_and_the_file() {
    let fx = Fixture::new();
    fx.write_default_config(FULL_FILE);
    let s = Settings::load(&full_flags(), &full_env(&fx)).unwrap();
    assert_eq!(s.output_dir.value, Path::new("/flag/out"));
    assert_eq!(s.image_provider.value, ProviderId::Gemini);
    assert_eq!(s.wait_timeout.value, Duration::from_secs(5));
    assert_eq!(s.poll_interval.value, Duration::from_secs(3));
    assert_eq!(s.log_filter.value, "warn,iris=debug");
    for source in [
        &s.output_dir.source,
        &s.image_provider.source,
        &s.wait_timeout.source,
        &s.poll_interval.source,
        &s.log_filter.source,
    ] {
        assert_eq!(*source, SettingSource::Flag);
    }
    // No flag exists for these; the environment still wins over the file.
    assert_eq!(s.state_dir.source, SettingSource::Env);
    assert_eq!(s.store_prompts.source, SettingSource::Env);
    let s = Settings::load(&CliOverrides { verbose: 3, ..Default::default() }, &fx.env()).unwrap();
    assert_eq!(s.log_filter.value, "warn,iris=trace");
}

#[test]
fn config_file_location_precedence_is_flag_then_env_then_default() {
    let fx = Fixture::new();
    fx.write_default_config("[image]\nprovider = \"openai\"\n");
    let from_env = fx.write("env.toml", "[image]\nprovider = \"gemini\"\n");
    let from_flag = fx.write("flag.toml", "[jobs]\nstore_prompts = true\n");

    let env = fx.env().with_var("IRIS_CONFIG", from_env.to_str().unwrap());
    let s = load(&env).unwrap();
    assert_eq!(
        (s.config_file.value.clone(), s.config_file.source.clone()),
        (from_env.clone(), SettingSource::Env)
    );
    assert_eq!(s.image_provider.value, ProviderId::Gemini);

    let cli = CliOverrides { config_path: Some(PathBuf::from("flag.toml")), ..Default::default() };
    let s = Settings::load(&cli, &env).unwrap();
    assert_eq!(s.config_file.value, from_flag, "relative --config resolves against the current directory");
    assert_eq!(s.config_file.source, SettingSource::Flag);
    assert!(s.store_prompts.value);
    assert_eq!(s.image_provider.source, SettingSource::Default, "only the selected file is read");
}

#[test]
fn an_explicitly_requested_missing_file_is_config_invalid() {
    let fx = Fixture::new();
    let cli = CliOverrides { config_path: Some(fx.cwd.join("nope.toml")), ..Default::default() };
    let e = load_err(&cli, &fx.env());
    assert_eq!(e.code, ErrorCode::ConfigInvalid);
    assert_eq!(e.exit_code(), 2);
    assert!(e.message.contains("nope.toml") && e.message.contains("--config"), "{}", e.message);

    let env = fx.env().with_var("IRIS_CONFIG", "~/missing.toml");
    let e = load_err(&CliOverrides::default(), &env);
    assert_eq!(e.code, ErrorCode::ConfigInvalid);
    assert!(e.message.contains(&fx.home.join("missing.toml").display().to_string()), "{}", e.message);
    assert!(e.message.contains("IRIS_CONFIG"), "{}", e.message);
}

#[test]
fn a_directory_or_unreadable_config_path_is_config_invalid() {
    let fx = Fixture::new();
    let cli = CliOverrides { config_path: Some(fx.cwd.clone()), ..Default::default() };
    assert_eq!(load_err(&cli, &fx.env()).code, ErrorCode::ConfigInvalid);
    std::fs::write(fx.cwd.join("bin.toml"), [0xff, 0xfe, 0x00]).unwrap();
    let cli = CliOverrides { config_path: Some(fx.cwd.join("bin.toml")), ..Default::default() };
    let e = load_err(&cli, &fx.env());
    assert!(e.message.contains("UTF-8"), "{}", e.message);
}

#[test]
fn unknown_keys_are_rejected_naming_the_file_and_key() {
    let fx = Fixture::new();
    for (text, needle) in [
        ("outptu_dir = \"/x\"\n", "outptu_dir"),
        ("[video]\ntimeout = \"1m\"\n", "timeout"),
        ("[providers.openai]\nmodel = \"x\"\n", "model"),
        ("[providers.seedance]\nbase_url = \"https://x\"\n", "seedance"),
    ] {
        let p = fx.write_default_config(text);
        let e = load(&fx.env()).unwrap_err();
        assert_eq!(e.code, ErrorCode::ConfigInvalid, "{text}");
        assert!(e.message.contains(needle), "{}", e.message);
        assert!(e.message.contains(&p.display().to_string()), "{}", e.message);
    }
}

#[test]
fn wrong_types_and_invalid_values_in_the_file_are_rejected_with_the_key() {
    let fx = Fixture::new();
    for (text, key) in [
        ("[jobs]\nstore_prompts = \"yes\"\n", "jobs.store_prompts"),
        ("[video]\nwait_timeout = \"forever\"\n", "video.wait_timeout"),
        ("[video]\nwait_timeout = 0\n", "video.wait_timeout"),
        ("[video]\npoll_interval = \"1s\"\n", "video.poll_interval"),
        ("[image]\nprovider = \"seedance\"\n", "image.provider"),
        ("[providers.openai]\nbase_url = \"ftp://x\"\n", "providers.openai.base_url"),
        ("[providers.gemini]\nrequest_timeout = -5\n", "providers.gemini.request_timeout"),
        ("output_dir = \"relative/dir\"\n", "output_dir"),
        ("[providers.gemini]\nimage_model = \"no-such-model\"\n", "providers.gemini.image_model"),
        ("[providers.openai]\nvideo_model = \"no-such-model\"\n", "providers.openai.video_model"),
        ("[providers.gemini]\nsubmit_timeout = 0\n", "providers.gemini.submit_timeout"),
        ("[providers.gemini]\nsubmit_timeout = \"soon\"\n", "providers.gemini.submit_timeout"),
        // A provider without video models never submits a job: the key would do nothing.
        ("[providers.openai]\nsubmit_timeout = \"5m\"\n", "providers.openai.submit_timeout"),
    ] {
        fx.write_default_config(text);
        let e = load(&fx.env()).unwrap_err();
        assert_eq!(e.code, ErrorCode::ConfigInvalid, "{text}");
        assert!(e.message.contains(key), "{text}: {}", e.message);
        assert_eq!(e.details.get("key").and_then(|v| v.as_str()), Some(key), "{text}");
    }
}

#[test]
fn credential_like_keys_are_rejected_at_any_depth() {
    let fx = Fixture::new();
    for (text, key) in [
        (format!("api_key = \"{FAKE_OPENAI}\"\n"), "api_key"),
        (format!("[providers.gemini]\ngemini_key = \"{FAKE_GEMINI}\"\n"), "providers.gemini.gemini_key"),
        (format!("[image]\nToken = \"{FAKE_OPENAI}\"\n"), "image.Token"),
        (
            "[providers.openai]\nnested = { password = \"hunter2hunter2\" }\n".to_string(),
            "providers.openai.nested.password",
        ),
        ("[jobs]\nsecret = 1\n".to_string(), "jobs.secret"),
    ] {
        fx.write_default_config(&text);
        let e = load(&fx.env()).unwrap_err();
        assert_eq!(e.code, ErrorCode::ConfigInvalid, "{text}");
        assert!(e.message.contains(key), "{}", e.message);
        assert!(e.message.contains("OPENAI_API_KEY / GEMINI_API_KEY"), "{}", e.message);
        for secret in [FAKE_OPENAI, FAKE_GEMINI, "hunter2"] {
            assert!(!e.message.contains(secret), "the value must never be echoed: {}", e.message);
        }
    }
}

#[test]
fn bad_environment_values_are_config_invalid_naming_the_variable() {
    let fx = Fixture::new();
    for (var, value) in [
        ("IRIS_WAIT_TIMEOUT", "soon"),
        ("IRIS_WAIT_TIMEOUT", "0"),
        ("IRIS_POLL_INTERVAL", "1s"),
        ("IRIS_POLL_INTERVAL", "abc"),
        ("IRIS_STORE_PROMPTS", "maybe"),
        ("IRIS_IMAGE_PROVIDER", "seedance"),
        ("IRIS_OPENAI_BASE_URL", "not a url"),
        ("IRIS_GEMINI_BASE_URL", "https://user:pw@example.com"),
        ("IRIS_GEMINI_BASE_URL", "https://example.com/?key=abc"),
        ("IRIS_LOG", "iris=notalevel"),
    ] {
        let e = load(&fx.env().with_var(var, value)).unwrap_err();
        assert_eq!(e.code, ErrorCode::ConfigInvalid, "{var}={value}");
        assert!(e.message.contains(var), "{var}: {}", e.message);
        assert_eq!(e.details.get("env_var").and_then(|v| v.as_str()), Some(var), "{var}");
    }
}

#[test]
fn a_bad_environment_value_is_reported_even_when_a_flag_overrides_it() {
    let fx = Fixture::new();
    let cli = CliOverrides { poll_interval: Some(Duration::from_secs(5)), ..Default::default() };
    let e = load_err(&cli, &fx.env().with_var("IRIS_POLL_INTERVAL", "often"));
    assert!(e.message.contains("IRIS_POLL_INTERVAL"), "{}", e.message);
}

#[test]
fn bad_flag_values_are_invalid_argument_naming_the_flag() {
    let fx = Fixture::new();
    let e = load_err(
        &CliOverrides { poll_interval: Some(Duration::from_secs(1)), ..Default::default() },
        &fx.env(),
    );
    assert_eq!(e.code, ErrorCode::InvalidArgument);
    assert!(e.message.contains("--poll-interval"), "{}", e.message);
    let e = load_err(&CliOverrides { wait_timeout: Some(Duration::ZERO), ..Default::default() }, &fx.env());
    assert_eq!(e.code, ErrorCode::InvalidArgument);
    assert!(e.message.contains("--timeout"), "{}", e.message);
}

#[test]
fn blank_environment_variables_count_as_unset() {
    let fx = Fixture::new();
    let s = load(&fx.env().with_var("IRIS_OUTPUT_DIR", "").with_var("IRIS_WAIT_TIMEOUT", "  ")).unwrap();
    assert_eq!(s.output_dir.source, SettingSource::Default);
    assert_eq!(s.wait_timeout.source, SettingSource::Default);
}

#[test]
fn tilde_is_expanded_in_every_layer_and_relative_paths_use_the_current_directory() {
    let fx = Fixture::new();
    fx.write_default_config("output_dir = \"~/Pictures/iris\"\nstate_dir = \"~\"\n");
    let s = load(&fx.env()).unwrap();
    assert_eq!(s.output_dir.value, fx.home.join("Pictures/iris"));
    assert_eq!(s.state_dir.value, fx.home);

    let env = fx.env().with_var("IRIS_OUTPUT_DIR", "~/env-out").with_var("IRIS_STATE_DIR", "rel-state");
    let s = load(&env).unwrap();
    assert_eq!(s.output_dir.value, fx.home.join("env-out"));
    assert_eq!(s.state_dir.value, fx.cwd.join("rel-state"));

    let cli = CliOverrides { out_dir: Some(PathBuf::from("~/flag-out")), ..Default::default() };
    assert_eq!(Settings::load(&cli, &env).unwrap().output_dir.value, fx.home.join("flag-out"));
    let cli = CliOverrides { out_dir: Some(PathBuf::from("out")), ..Default::default() };
    assert_eq!(Settings::load(&cli, &env).unwrap().output_dir.value, fx.cwd.join("out"));
}

#[test]
fn platform_default_paths_have_the_documented_shape() {
    let fx = Fixture::new();
    let xdg_config = fx.cwd.join("xdg-config");
    let xdg_state = fx.cwd.join("xdg-state");
    let env = fx
        .env()
        .with_var("XDG_CONFIG_HOME", xdg_config.to_str().unwrap())
        .with_var("XDG_STATE_HOME", xdg_state.to_str().unwrap());
    std::fs::create_dir_all(xdg_config.join("iris")).unwrap();
    std::fs::write(xdg_config.join("iris/config.toml"), "[jobs]\nstore_prompts = true\n").unwrap();
    let s = load(&env).unwrap();
    assert_eq!(s.config_file.value, xdg_config.join("iris/config.toml"));
    assert!(s.config_file_exists && s.store_prompts.value, "the XDG config file is discovered");
    assert_eq!(s.state_dir.value, xdg_state.join("iris"));

    let mac = EnvSnapshot::new(Platform::MacOs, Some(fx.home.clone()), fx.cwd.clone())
        .with_var("XDG_CONFIG_HOME", xdg_config.to_str().unwrap());
    let s = load(&mac).unwrap();
    let support = fx.home.join("Library/Application Support/iris");
    assert_eq!(s.config_file.value, support.join("config.toml"));
    assert_eq!(s.state_dir.value, support);
    assert_eq!(s.jobs_dir(), support.join("jobs"));
}

#[test]
fn platform_paths_agree_with_the_dirs_crate_for_this_process() {
    // Reads only HOME/XDG_* (never credentials) and never mutates the environment.
    let (Some(home), Some(config), Some(data)) = (dirs::home_dir(), dirs::config_dir(), dirs::data_dir())
    else {
        return;
    };
    let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let ours = platform_paths(
        Platform::current(),
        &home,
        var("XDG_CONFIG_HOME").as_deref(),
        var("XDG_STATE_HOME").as_deref(),
    );
    assert_eq!(ours.config_file, config.join("iris").join("config.toml"));
    let state_base = dirs::state_dir().unwrap_or(data);
    assert_eq!(ours.state_dir, state_base.join("iris"));
}

#[test]
fn describe_lists_every_setting_with_sources_and_never_secrets() {
    let fx = Fixture::new();
    fx.write_default_config("[video]\npoll_interval = \"20s\"\n");
    let env = fx
        .env()
        .with_var("OPENAI_API_KEY", FAKE_OPENAI)
        .with_var("IRIS_STATE_DIR", "/env/state")
        .with_var("IRIS_OPENAI_BASE_URL", "http://127.0.0.1:8080/v1");
    let cli = CliOverrides { out_dir: Some(PathBuf::from("/flag/out")), ..Default::default() };
    let s = Settings::load(&cli, &env).unwrap();
    let show = s.config_show();
    let keys: Vec<&str> = show.settings.iter().map(|r| r.key.as_str()).collect();
    assert_eq!(
        keys,
        [
            "config_file",
            "output_dir",
            "state_dir",
            "image.provider",
            "video.wait_timeout",
            "video.poll_interval",
            "jobs.store_prompts",
            "providers.openai.base_url",
            "providers.openai.image_model",
            "providers.openai.request_timeout",
            "providers.gemini.base_url",
            "providers.gemini.image_model",
            "providers.gemini.video_model",
            "providers.gemini.request_timeout",
            "providers.gemini.submit_timeout",
            "log",
        ]
    );
    let find = |k: &str| show.settings.iter().find(|r| r.key == k).unwrap();
    assert_eq!(find("output_dir").source, SettingSource::Flag);
    assert_eq!(find("state_dir").source, SettingSource::Env);
    assert_eq!(find("state_dir").env_var.as_deref(), Some("IRIS_STATE_DIR"));
    assert_eq!(find("video.poll_interval").source, SettingSource::File);
    assert_eq!(find("video.poll_interval").value, serde_json::json!("20s"));
    assert_eq!(find("image.provider").source, SettingSource::Default);
    assert_eq!(find("providers.openai.image_model").env_var, None);
    assert!(show.config_file_exists);

    let creds: Vec<(String, bool)> = show.credentials.iter().map(|c| (c.env.clone(), c.present)).collect();
    assert_eq!(creds, [("OPENAI_API_KEY".to_string(), true), ("GEMINI_API_KEY".to_string(), false)]);
    let json = serde_json::to_string(&show).unwrap();
    assert!(!json.contains(FAKE_OPENAI), "config show must never include a key");
    assert!(!format!("{s:?}").contains(FAKE_OPENAI), "Debug must never include a key");

    let warnings = s.warnings();
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].code, WARNING_NON_DEFAULT_BASE_URL);
    assert!(warnings[0].message.contains("OPENAI_API_KEY") && warnings[0].message.contains("unencrypted"));

    let paths = s.config_path();
    assert_eq!(paths.state_dir, "/env/state");
    assert_eq!(paths.jobs_dir, "/env/state/jobs");
}

#[test]
fn a_base_url_equal_to_the_default_is_not_flagged() {
    let fx = Fixture::new();
    let env = fx.env().with_var("IRIS_OPENAI_BASE_URL", "https://api.openai.com/v1/");
    let s = load(&env).unwrap();
    assert_eq!(s.provider(ProviderId::OpenAi).base_url.source, SettingSource::Env);
    assert!(s.warnings().is_empty());
}

#[test]
fn credentials_come_only_from_the_two_variables() {
    let fx = Fixture::new();
    let env = fx.env().with_var("GEMINI_API_KEY", FAKE_GEMINI).with_var("GOOGLE_API_KEY", "not-used-000000");
    let s = load(&env).unwrap();
    assert_eq!(s.require_credential(ProviderId::Gemini).unwrap().expose(), FAKE_GEMINI);
    assert!(!s.credential_present(ProviderId::OpenAi));
    let e = s.require_credential(ProviderId::OpenAi).unwrap_err();
    assert_eq!(e.code, ErrorCode::MissingCredentials);
    assert_eq!(e.exit_code(), 3);
    assert!(e.message.contains("OPENAI_API_KEY"), "{}", e.message);
    assert_eq!(e.provider, Some(ProviderId::OpenAi));
}

#[test]
fn catalog_models_are_accepted_as_file_defaults_and_cross_provider_models_rejected() {
    // Meaningful once the model catalog is populated; vacuous for an empty catalog.
    let fx = Fixture::new();
    for spec in catalog::all() {
        for (kind, ops) in [
            ("image_model", &[Operation::ImageGenerate, Operation::ImageEdit][..]),
            ("video_model", &[Operation::VideoGenerate][..]),
        ] {
            let supported = ops.iter().any(|op| spec.supports(*op));
            let own = spec.provider;
            let other = if own == ProviderId::OpenAi { ProviderId::Gemini } else { ProviderId::OpenAi };
            let alias_or_id = spec.aliases.first().copied().unwrap_or(spec.id);
            fx.write_default_config(&format!("[providers.{own}]\n{kind} = \"{alias_or_id}\"\n"));
            match load(&fx.env()) {
                Ok(s) if supported => {
                    let got = if kind == "image_model" {
                        &s.provider(own).image_model
                    } else {
                        &s.provider(own).video_model
                    };
                    assert_eq!(got.value.as_deref(), Some(spec.id), "aliases resolve to the canonical id");
                    assert_eq!(got.source, SettingSource::File);
                }
                Err(e) if !supported => assert_eq!(e.code, ErrorCode::ConfigInvalid),
                other => panic!("{} as {kind}: unexpected {other:?}", spec.id),
            }
            fx.write_default_config(&format!("[providers.{other}]\n{kind} = \"{}\"\n", spec.id));
            assert_eq!(
                load(&fx.env()).unwrap_err().code,
                ErrorCode::ConfigInvalid,
                "{} under {other}",
                spec.id
            );
        }
    }
}

#[test]
fn every_provider_gets_its_config_table_base_url_variable_and_show_rows() {
    // Provider settings are resolved for each id in ProviderId::ALL, named after it.
    let fx = Fixture::new();
    let mut text = String::new();
    for p in ProviderId::ALL {
        text.push_str(&format!(
            "[providers.{p}]\nbase_url = \"https://file-{p}.example\"\nrequest_timeout = 42\n"
        ));
    }
    fx.write_default_config(&text);
    let s = load(&fx.env()).unwrap();
    assert_eq!(s.providers().map(|p| p.provider).collect::<Vec<_>>(), ProviderId::ALL);
    let mut env = fx.env();
    for p in ProviderId::ALL {
        let got = s.provider(*p);
        assert_eq!(got.base_url.value.host_str(), Some(format!("file-{p}.example").as_str()));
        assert_eq!(got.base_url.source, SettingSource::File);
        assert_eq!(got.request_timeout.value, Duration::from_secs(42));
        env = env.with_var(p.base_url_env(), &format!("https://env-{p}.example"));
    }

    let s = load(&env).unwrap();
    let show = s.config_show();
    for p in ProviderId::ALL {
        assert_eq!(s.provider(*p).base_url.source, SettingSource::Env, "{p}");
        let key = format!("providers.{p}.base_url");
        let row = show.settings.iter().find(|r| r.key == key).unwrap();
        assert_eq!(row.env_var.as_deref(), Some(p.base_url_env()));
    }
    assert_eq!(s.warnings().len(), ProviderId::ALL.len(), "every overridden base URL is flagged");

    // A table for a provider Iris does not have names the known ones.
    fx.write_default_config("[providers.seedance]\nbase_url = \"https://x.example\"\n");
    let e = load(&fx.env()).unwrap_err();
    assert_eq!(e.details.get("key").and_then(|v| v.as_str()), Some("providers.seedance"));
    let known: Vec<String> = ProviderId::ALL.iter().map(|p| format!("`{p}`")).collect();
    assert!(
        e.message.contains(&format!("unknown key; expected one of {}", known.join(", "))),
        "{}",
        e.message
    );
}

/// `[providers.gemini] submit_timeout` sets the Veo submission timeout (before the
/// upload allowance), and the stale-submission budget follows it.
#[test]
fn the_submit_timeout_is_configurable_and_moves_the_submit_budget() {
    let fx = Fixture::new();
    let default_budget =
        iris::jobs::paid_submit_budget(&load(&fx.env()).unwrap().timeouts(ProviderId::Gemini));
    fx.write_default_config("[providers.gemini]\nsubmit_timeout = \"5m\"\nrequest_timeout = 90\n");
    let s = load(&fx.env()).unwrap();
    let gemini = s.provider(ProviderId::Gemini);
    assert_eq!(
        (gemini.submit_timeout.value, gemini.submit_timeout.source.clone()),
        (Duration::from_secs(300), SettingSource::File)
    );
    let t = s.timeouts(ProviderId::Gemini);
    assert_eq!((t.submit, t.generate), (Duration::from_secs(300), Duration::from_secs(90)));
    assert_eq!(s.timeouts(ProviderId::OpenAi).submit, Duration::from_secs(60));
    let budget = iris::jobs::paid_submit_budget(&t);
    assert_eq!(budget, default_budget + Duration::from_secs(3 * 240));
    let row =
        s.config_show().settings.into_iter().find(|r| r.key == "providers.gemini.submit_timeout").unwrap();
    assert_eq!((row.value, row.source), (serde_json::json!("5m"), SettingSource::File));
}

/// Plain http only for loopback hosts, from any layer; the warning helper used by
/// credentialed commands adds a provider's warning once, and only when its base
/// URL is not the default.
#[test]
fn plain_http_base_urls_must_be_loopback_and_are_warned_about_once() {
    let fx = Fixture::new();
    let e = load(&fx.env().with_var("IRIS_OPENAI_BASE_URL", "http://api.example.invalid/v1")).unwrap_err();
    assert_eq!(e.code, ErrorCode::ConfigInvalid);
    assert_eq!(e.details.get("env_var").and_then(|v| v.as_str()), Some("IRIS_OPENAI_BASE_URL"));
    assert!(e.message.contains("loopback"), "{}", e.message);
    fx.write_default_config("[providers.gemini]\nbase_url = \"http://192.168.1.20:8080\"\n");
    let e = load(&fx.env()).unwrap_err();
    assert_eq!(e.details.get("key").and_then(|v| v.as_str()), Some("providers.gemini.base_url"));
    fx.write_default_config("");

    let s = load(&fx.env()).unwrap();
    let mut warnings = Vec::new();
    s.warn_non_default_base_url(ProviderId::Gemini, &mut warnings);
    assert!(warnings.is_empty(), "the default base URL is not flagged");

    let s = load(&fx.env().with_var("IRIS_GEMINI_BASE_URL", "http://localhost:8080")).unwrap();
    for _ in 0..3 {
        s.warn_non_default_base_url(ProviderId::Gemini, &mut warnings);
        s.warn_non_default_base_url(ProviderId::OpenAi, &mut warnings);
    }
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert_eq!(warnings[0].code, WARNING_NON_DEFAULT_BASE_URL);
    assert!(warnings[0].message.contains("GEMINI_API_KEY is sent to that host over unencrypted HTTP"));
    assert_eq!(Some(warnings[0].clone()), s.base_url_warning(ProviderId::Gemini));
}
