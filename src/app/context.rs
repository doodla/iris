//! The application context: resolved settings plus every dependency a workflow
//! needs, injected so tests can substitute fake providers, a custom catalog, a
//! manual interrupt, or a fixed clock.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use jiff::Timestamp;
use tokio::sync::watch;

use crate::config::Settings;
use crate::domain::ProviderId;
use crate::error::IrisError;
use crate::http::HttpClient;
use crate::jobs::{self, JobStore};
use crate::providers::{Provider, ProviderContext, Registry};

use super::catalog::Catalog;

/// Replaceable dependencies of an [`AppContext`].
pub struct Deps {
    /// Provider adapters, looked up by [`ProviderId`].
    pub registry: Registry,
    /// Models the app resolves against.
    pub catalog: Catalog,
    /// HTTP client; `None` builds one from the settings on first use.
    pub http: Option<HttpClient>,
    /// Interrupt source (Ctrl-C and termination signals).
    pub interrupt: Interrupt,
    /// Time source for timestamps written by the app.
    pub clock: Clock,
}

impl Deps {
    /// The production set: built-in adapters and catalog, SIGINT/SIGTERM/SIGHUP,
    /// system clock.
    pub fn builtin() -> Deps {
        Deps {
            registry: Registry::builtin(),
            catalog: Catalog::builtin(),
            http: None,
            interrupt: Interrupt::signal(),
            clock: Clock::system(),
        }
    }
}

/// Everything a workflow needs. Workflows never print: progress goes through
/// [`Progress`], results and errors are returned.
pub struct AppContext {
    pub settings: Settings,
    pub registry: Registry,
    pub catalog: Catalog,
    /// Job store over `<state_dir>/jobs`, with the stale-`submitting` threshold
    /// derived from the configured timeouts.
    pub store: JobStore,
    pub progress: Progress,
    pub clock: Clock,
    pub interrupt: Interrupt,
    http: OnceLock<HttpClient>,
}

impl AppContext {
    pub fn new(settings: Settings, deps: Deps, progress: Progress) -> AppContext {
        let budget = ProviderId::ALL
            .iter()
            .map(|p| jobs::paid_submit_budget(&settings.timeouts(*p)))
            .max()
            .unwrap_or_else(|| jobs::paid_submit_budget(&Default::default()));
        let store = JobStore::new(&settings.state_dir.value).with_submit_budget(budget);
        let http = OnceLock::new();
        if let Some(client) = deps.http {
            let _ = http.set(client);
        }
        AppContext {
            settings,
            registry: deps.registry,
            catalog: deps.catalog,
            store,
            progress,
            clock: deps.clock,
            interrupt: deps.interrupt,
            http,
        }
    }

    /// The shared HTTP client (built from the settings on first use).
    pub fn http(&self) -> Result<&HttpClient, IrisError> {
        if let Some(client) = self.http.get() {
            return Ok(client);
        }
        let client = HttpClient::new(&self.settings.http_settings())?;
        Ok(self.http.get_or_init(|| client))
    }

    /// The registered adapter of `id`.
    pub fn provider(&self, id: ProviderId) -> Result<&dyn Provider, IrisError> {
        self.registry
            .get(id)
            .ok_or_else(|| IrisError::internal(format!("provider '{id}' is not registered in this build")))
    }

    /// Context for one provider call: HTTP client, configured base URL, credential
    /// (`missing_credentials` naming the variable if absent), timeouts.
    pub fn provider_context(&self, id: ProviderId) -> Result<ProviderContext, IrisError> {
        let credential = self.settings.require_credential(id)?;
        Ok(ProviderContext {
            http: self.http()?.clone(),
            base_url: self.settings.provider(id).base_url.value.clone(),
            credential,
            timeouts: self.settings.timeouts(id),
        })
    }

    /// Current time from the injected clock.
    pub fn now(&self) -> Timestamp {
        self.clock.now()
    }
}

impl fmt::Debug for AppContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AppContext")
            .field("settings", &self.settings)
            .field("store", &self.store)
            .finish_non_exhaustive()
    }
}

/// A function receiving progress lines.
type LineSink = Arc<dyn Fn(&str) + Send + Sync>;

/// Sink for progress lines (stderr in the CLI, suppressed by `-q`). Lines never
/// contain prompts, keys, or signed URLs.
#[derive(Clone, Default)]
pub struct Progress {
    sink: Option<LineSink>,
}

impl Progress {
    /// Discard progress.
    pub fn silent() -> Progress {
        Progress { sink: None }
    }

    /// Send each line to `sink`.
    pub fn new(sink: impl Fn(&str) + Send + Sync + 'static) -> Progress {
        Progress { sink: Some(Arc::new(sink)) }
    }

    /// Report one line.
    pub fn line(&self, text: impl AsRef<str>) {
        if let Some(sink) = &self.sink {
            sink(text.as_ref());
        }
    }
}

impl fmt::Debug for Progress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Progress").field("enabled", &self.sink.is_some()).finish()
    }
}

/// Time source for timestamps written by the app (whole seconds, UTC).
#[derive(Clone)]
pub struct Clock {
    now: Arc<dyn Fn() -> Timestamp + Send + Sync>,
}

impl Clock {
    /// The system clock truncated to whole seconds ([`jobs::now`]).
    pub fn system() -> Clock {
        Clock { now: Arc::new(jobs::now) }
    }

    /// A custom clock (tests).
    pub fn from_fn(now: impl Fn() -> Timestamp + Send + Sync + 'static) -> Clock {
        Clock { now: Arc::new(now) }
    }

    pub fn now(&self) -> Timestamp {
        (self.now)()
    }
}

impl Default for Clock {
    fn default() -> Self {
        Clock::system()
    }
}

impl fmt::Debug for Clock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Clock")
    }
}

/// Counts interrupts so workflows can race provider calls, polls, and downloads
/// against them (dropping the losing future cancels HTTP).
///
/// On Unix, SIGINT (Ctrl-C), SIGTERM, and SIGHUP all count as interrupts: a
/// supervisor's `kill`, a `timeout`, or a closed terminal gets the same handling
/// as Ctrl-C (a paid Veo submission defers the first one until the operation id
/// is recorded; every interruptible phase ends with one `interrupted` result,
/// exit 130). Elsewhere only Ctrl-C does.
///
/// The handlers are installed lazily by [`Interrupt::arm`], when a workflow enters
/// an interruptible phase; until then these signals keep their default effect
/// (terminate), which is right for quick local commands. Once installed, they stay
/// for the rest of the process (tokio cannot uninstall them), so every
/// long-running phase afterwards must watch the interrupt.
#[derive(Clone)]
pub struct Interrupt {
    inner: Arc<InterruptInner>,
}

struct InterruptInner {
    count: watch::Sender<u64>,
    from_signal: bool,
    armed: AtomicBool,
}

impl Interrupt {
    /// Interrupts come from SIGINT (Ctrl-C), SIGTERM, and SIGHUP.
    pub fn signal() -> Interrupt {
        Interrupt::with_source(true)
    }

    /// Interrupts come only from [`Interrupt::trigger`] (tests, embedding).
    pub fn manual() -> Interrupt {
        Interrupt::with_source(false)
    }

    fn with_source(from_signal: bool) -> Interrupt {
        Interrupt {
            inner: Arc::new(InterruptInner {
                count: watch::Sender::new(0),
                from_signal,
                armed: AtomicBool::new(false),
            }),
        }
    }

    /// Record one interrupt, as a Ctrl-C would.
    pub fn trigger(&self) {
        self.inner.count.send_modify(|n| *n += 1);
    }

    /// Interrupts seen so far.
    pub fn count(&self) -> u64 {
        *self.inner.count.borrow()
    }

    /// Install the signal handlers (idempotent; no-op for manual interrupts).
    /// Must be called from within a tokio runtime.
    pub fn arm(&self) {
        if !self.inner.from_signal || self.inner.armed.swap(true, Ordering::SeqCst) {
            return;
        }
        if tokio::runtime::Handle::try_current().is_err() {
            self.inner.armed.store(false, Ordering::SeqCst);
            return;
        }
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            let kinds = [
                ("SIGINT", SignalKind::interrupt()),
                ("SIGTERM", SignalKind::terminate()),
                ("SIGHUP", SignalKind::hangup()),
            ];
            let mut installed = 0;
            for (name, kind) in kinds {
                // `signal()` registers the handler synchronously, before any request is sent.
                match signal(kind) {
                    Ok(mut stream) => {
                        installed += 1;
                        let inner = Arc::clone(&self.inner);
                        tokio::spawn(async move {
                            while stream.recv().await.is_some() {
                                inner.count.send_modify(|n| *n += 1);
                            }
                        });
                    }
                    Err(e) => tracing::warn!("cannot install the {name} handler: {e}"),
                }
            }
            if installed == 0 {
                self.inner.armed.store(false, Ordering::SeqCst);
            }
        }
        #[cfg(not(unix))]
        {
            let inner = Arc::clone(&self.inner);
            tokio::spawn(async move {
                while tokio::signal::ctrl_c().await.is_ok() {
                    inner.count.send_modify(|n| *n += 1);
                }
            });
        }
    }

    /// Resolves once more than `seen` interrupts have been recorded.
    pub async fn after(&self, seen: u64) {
        let mut rx = self.inner.count.subscribe();
        loop {
            if *rx.borrow_and_update() > seen {
                return;
            }
            if rx.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    }
}

impl fmt::Debug for Interrupt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Interrupt")
            .field("from_signal", &self.inner.from_signal)
            .field("count", &self.count())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn manual_interrupts_are_counted_and_observed() {
        let int = Interrupt::manual();
        let seen = int.count();
        let waiter = {
            let int = int.clone();
            tokio::spawn(async move { int.after(seen).await })
        };
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());
        int.trigger();
        waiter.await.unwrap();
        assert_eq!(int.count(), 1);
        // An interrupt that already happened resolves immediately.
        int.after(0).await;
    }

    #[test]
    fn progress_sink_receives_lines() {
        let lines = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = Arc::clone(&lines);
        let p = Progress::new(move |l| sink.lock().unwrap().push(l.to_string()));
        p.line("one");
        Progress::silent().line("dropped");
        assert_eq!(*lines.lock().unwrap(), vec!["one".to_string()]);
    }
}
