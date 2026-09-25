//! Local artifact handling (see `iris --help` and docs/jobs.md "Downloads").
//!
//! * [`paths`] — plan absolute output paths (default names, `-o` rules,
//!   extension/format consistency), the `output_exists` preflight, and the
//!   output-directory preflight.
//! * [`media`] — sniff media types from magic bytes, decode images, validate
//!   ISO-BMFF video structure, inspect PNG alpha/dimensions.
//! * [`read_input_image`], [`check_request_inputs`] — validate local input images
//!   (one by one, then the rules that relate them, such as mask dimensions and an
//!   inline request cap) against a model's declared input capabilities before any
//!   paid request.
//! * [`save_image`], [`finalize_download`], [`PartFile`] — atomic, no-clobber (or
//!   `--overwrite`) finalization through `.<name>.iris-part-*` temp files, with the
//!   rename fallback that keeps paid synchronous outputs.
//! * [`decide_download`], [`copy_local`] — repeat downloads without the network.
//! * [`save_unsaved`], [`save_unsaved_raw`] — the last-resort location
//!   (`<state_dir>/unsaved/`) for a paid image that could not be saved where it was
//!   requested, and for paid content that is not a valid image (kept as received).
//!
//! Nothing here talks to the network: streaming a remote artifact is
//! `crate::http::download`, which writes into a [`PartFile`] through its open
//! handle (never by reopening its path).

mod download;
mod fallback;
mod finalize;
mod input;
pub mod media;
pub mod paths;

pub use download::{DownloadDecision, RecordedFile, copy_local, decide_download, is_intact};
pub use fallback::{save_unsaved, save_unsaved_raw};
pub use finalize::{
    FinalizeMode, PartFile, SaveOutcome, SavedArtifact, already_present_warning, build_artifact,
    finalize_download, place, save_image, sha256_bytes, sha256_file,
};
pub use input::{check_request_inputs, read_input_image};
pub use media::{ImageDetails, IsoBmffInfo, MediaInfo};
pub use paths::{
    Naming, PathRequest, PlannedOutputs, adjust_extension, plan_outputs, preflight, preflight_dirs,
    preflight_other_types,
};
