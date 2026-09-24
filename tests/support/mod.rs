//! Shared helpers for the end-to-end process tests (`tests/e2e_*.rs`).
//!
//! The tests run the built `iris` binary as a real process, one invocation per
//! command, against 127.0.0.1 wiremock servers that emulate the OpenAI Images API
//! and the Gemini API (images, Veo operations, Files API downloads). Nothing here
//! reads a real credential:
//!
//! * every child process starts from an empty environment (`env_clear`, plus an
//!   explicit `env_remove` of the credential variables) and receives only a temp
//!   `HOME`, `IRIS_STATE_DIR`, and fake keys set through `Command::env`;
//! * provider base URLs point at `127.0.0.1:9`, where nothing listens, unless a
//!   test attaches a mock server, and `HTTPS_PROXY` points there too, so a request
//!   that should not happen fails locally instead of reaching a paid API.
//!
//! Modules: [`process`] (sandbox + process runner), [`mock`] (mock servers and
//! provider wire fixtures), [`media`] (image/video fixtures), [`schema`] (validation
//! of every JSON envelope against the committed schema, including the command's
//! `$defs` result type).

#![allow(dead_code, unused_imports)]

pub mod media;
pub mod mock;
pub mod process;
pub mod schema;

pub use media::*;
pub use mock::*;
pub use process::*;
pub use schema::*;
