//! A real signal delivered to the process after `video generate` has installed
//! its handlers and written the job record, but before the paid request is sent,
//! stops the command without sending anything.
//!
//! This is its own test binary: once installed, the handlers stay for the rest of
//! the process, and a signal sent here reaches every armed interrupt in it.

#![cfg(unix)]

#[path = "app_support.rs"]
mod support;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use iris::app::video::{self, VideoArgs};
use iris::app::{AppContext, GenerationArgs, Interrupt, Progress};
use iris::domain::ProviderId;
use iris::error::ErrorCode;
use iris::jobs::JobStore;
use support::*;

#[tokio::test]
async fn a_signal_before_the_request_is_sent_stops_without_sending_it() {
    // One test for all three signals, so no other test in this binary can see them.
    for sig in ["TERM", "INT", "HUP"] {
        let sandbox = Sandbox::new();
        let gemini = Arc::new(FakeProvider::gemini());
        let interrupt = Interrupt::signal();
        let on_submit = interrupt.clone();
        // Runs on the runtime's only thread, between writing the record and sending
        // the request, and returns without yielding to the runtime.
        let progress = Progress::new(move |line| {
            if !line.starts_with("Submitting job") {
                return;
            }
            let before = on_submit.delivered();
            let status = std::process::Command::new("kill")
                .args([format!("-{sig}"), std::process::id().to_string()])
                .status()
                .unwrap();
            assert!(status.success(), "kill -{sig} failed");
            // `kill` returns once the signal is sent; the kernel may run the handler
            // on another thread of this process, so wait (bounded) until it has.
            let deadline = Instant::now() + Duration::from_secs(10);
            while on_submit.delivered() == before {
                assert!(Instant::now() < deadline, "SIG{sig} was not delivered within 10 s");
                std::thread::sleep(Duration::from_millis(1));
            }
        });
        let ctx = AppContext::new(settings(&sandbox.env()), deps(vec![gemini.clone()], interrupt), progress);
        let args = VideoArgs {
            common: GenerationArgs {
                prompt: "x".into(),
                model: Some("fake-video-1".into()),
                ..GenerationArgs::default()
            },
            ..VideoArgs::default()
        };
        let mut w = Vec::new();
        let e = video::run(&ctx, args, &mut w).await.unwrap_err();
        assert_eq!(e.code, ErrorCode::Interrupted, "SIG{sig}: {e:?}");
        assert_eq!(e.exit_code(), 130);
        // Nothing was sent: running the command again is harmless, and there is no
        // job to point at.
        assert_eq!(e.retryable, Some(true), "SIG{sig}: {e:?}");
        assert!(!e.details.contains_key("charge_possible"), "SIG{sig}: {e:?}");
        assert_eq!(e.provider, Some(ProviderId::Gemini));
        assert!(e.job_id.is_none() && e.job_status.is_none(), "SIG{sig}: {e:?}");
        assert_eq!(
            gemini.videos().submit_calls.load(Ordering::SeqCst),
            0,
            "SIG{sig}: the paid request was sent"
        );
        let listing = JobStore::new(sandbox.state()).list().unwrap();
        assert!(listing.records.is_empty(), "SIG{sig}: record left: {:?}", listing.records);
    }
}
