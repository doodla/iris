//! Iris: generate and edit images and generate videos through multiple providers
//! from one agent-friendly CLI.
//!
//! Layering (a module only depends on those after it):
//! `cli -> app -> {providers, jobs, artifacts, catalog, config, output} -> {http, redact, error, domain}`.

#![forbid(unsafe_code)]

pub mod catalog;
pub mod domain;
pub mod error;
pub mod http;
pub mod output;
pub mod providers;
pub mod redact;
pub mod secret;
