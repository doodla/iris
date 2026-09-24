//! Output: the JSON contract (envelope + result DTOs + schema) and human rendering.

pub mod envelope;
pub mod human;
pub mod results;

pub use envelope::{CommandName, Envelope, ErrorBody, ResultPayload, SCHEMA_VERSION, schema};
