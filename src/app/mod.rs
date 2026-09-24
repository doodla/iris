//! Application workflows: image generate/edit, video generate, jobs, models,
//! providers, config, doctor.
//!
//! This layer knows nothing about clap or terminals: it takes typed arguments and
//! an [`AppContext`], reports progress through [`Progress`], collects warnings in
//! a caller-supplied list, and returns result DTOs (`crate::output::results`) or an
//! [`IrisError`](crate::error::IrisError). It never builds HTTP requests itself;
//! providers are reached only through the registry's adapters.

pub mod catalog;
pub mod context;
pub mod doctor;
pub mod image;
pub mod info;
pub mod jobs;
pub mod models;
mod request;
pub mod video;

pub use catalog::Catalog;
pub use context::{AppContext, Clock, Deps, Interrupt, Progress};
pub use request::{GenerationArgs, GenerationOutcome};
