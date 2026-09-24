//! Persisted provider-native job records and the job store (see docs/jobs.md).
//!
//! Only provider-native asynchronous operations (`video.generate`) create job
//! records; synchronous image calls never do. A record is enough to resume,
//! diagnose, and download a job from a later process:
//!
//! * [`JobId`] — validated `job_<26 lowercase ULID chars>` identifiers. Every path
//!   join goes through a `JobId`, so traversal (`../x`) is unrepresentable.
//! * [`JobRecord`] — the versioned (v1) on-disk record with transition helpers
//!   that only allow the arrows documented in docs/jobs.md.
//! * [`JobStore`] — `<state_dir>/jobs/`: atomic writes, per-job exclusive locks
//!   for read-modify-write, lock-free listing, local deletion, and download locks.

mod record;
mod store;

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::IrisError;
use crate::redact;

pub use record::{
    JOB_RECORD_VERSION, JobOutput, JobRecord, NewJob, OutputPlan, PollApplied, PromptRecord, SUBMIT_GRACE,
    request_metadata,
};
pub use store::{DownloadLock, JobListing, JobStore, paid_submit_budget};

/// Prefix of every job id.
const JOB_ID_PREFIX: &str = "job_";
/// Number of ULID characters after the prefix.
const JOB_ID_ULID_LEN: usize = 26;

/// A validated local job identifier: `job_` followed by 26 lowercase ULID
/// characters (`^job_[0-9a-z]{26}$`).
///
/// A `JobId` can only be obtained from [`JobId::generate`] or [`JobId::parse`]
/// (also used by `FromStr` and serde), so any `JobId` is safe to join to a path.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct JobId(String);

impl JobId {
    /// A new, unique id from a fresh ULID (lowercased). Ids sort by creation time
    /// at millisecond granularity.
    pub fn generate() -> JobId {
        JobId(format!("{JOB_ID_PREFIX}{}", ulid::Ulid::generate().to_string().to_ascii_lowercase()))
    }

    /// Validate a user- or file-supplied id. Errors are `invalid_argument`.
    pub fn parse(raw: &str) -> Result<JobId, IrisError> {
        if Self::is_valid(raw) {
            Ok(JobId(raw.to_string()))
        } else {
            Err(IrisError::invalid(format!(
                "invalid job id '{}': expected 'job_' followed by 26 lowercase letters or digits",
                redact::truncate(raw, 64)
            ))
            .with_hint("run `iris jobs list` to see local job ids"))
        }
    }

    /// True if `raw` matches `^job_[0-9a-z]{26}$`.
    pub fn is_valid(raw: &str) -> bool {
        raw.strip_prefix(JOB_ID_PREFIX).is_some_and(|rest| {
            rest.len() == JOB_ID_ULID_LEN
                && rest.bytes().all(|b| b.is_ascii_digit() || b.is_ascii_lowercase())
        })
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for JobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for JobId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl FromStr for JobId {
    type Err = IrisError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        JobId::parse(s)
    }
}

impl Serialize for JobId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for JobId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        JobId::parse(&raw).map_err(|e| serde::de::Error::custom(e.message.clone()))
    }
}

/// The current time truncated to whole seconds (docs/json-contract.md timestamps look like
/// `2026-09-24T12:34:56Z`). Transition helpers take `now` explicitly; callers
/// normally pass this.
pub fn now() -> jiff::Timestamp {
    let now = jiff::Timestamp::now();
    jiff::Timestamp::from_second(now.as_second()).unwrap_or(now)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_ids_are_valid_and_lowercase() {
        for _ in 0..100 {
            let id = JobId::generate();
            assert!(JobId::is_valid(id.as_str()), "{id}");
            assert_eq!(id.as_str().len(), 30);
        }
    }

    #[test]
    fn parse_rejects_traversal_and_bad_shapes() {
        for bad in [
            "",
            "../x",
            "job_../..",
            "job_../../../../../etc/passwd",
            "job_01arz3ndektsv4rrffq69g5fa",   // 25 chars
            "job_01arz3ndektsv4rrffq69g5favx", // 27 chars
            "job_01ARZ3NDEKTSV4RRFFQ69G5FAV",  // uppercase
            "JOB_01arz3ndektsv4rrffq69g5fav",
            "job_01arz3ndektsv4rrffq69g5fa/",
            "job_01arz3ndektsv4rrffq69g5fa.",
            "job-01arz3ndektsv4rrffq69g5fav",
            " job_01arz3ndektsv4rrffq69g5fav",
        ] {
            let err = JobId::parse(bad).unwrap_err();
            assert_eq!(err.code, crate::error::ErrorCode::InvalidArgument, "{bad:?}");
        }
        assert!(JobId::parse("job_01arz3ndektsv4rrffq69g5fav").is_ok());
    }

    #[test]
    fn now_has_whole_seconds() {
        assert_eq!(now().subsec_nanosecond(), 0);
        assert!(now().to_string().ends_with('Z'));
    }
}
