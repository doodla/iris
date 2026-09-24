//! Google wire types (private to the Gemini adapter).
//!
//! Field names follow the v1/v1beta discovery documents (lowerCamelCase JSON).
//! Response types accept missing and unknown fields: only what Iris interprets is
//! declared, and everything is optional so a partial body is interpreted by the
//! adapter instead of failing deserialization.

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ----- generateContent (images) ------------------------------------------------

/// `POST /v1/models/{model}:generateContent` body.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerateContentRequest<'a> {
    pub contents: Vec<Content<'a>>,
    pub generation_config: GenerationConfig<'a>,
    /// Always `false`: Iris asks Google not to store the request.
    pub store: bool,
}

#[derive(Debug, Serialize)]
pub struct Content<'a> {
    pub role: &'static str,
    pub parts: Vec<RequestPart<'a>>,
}

/// A request part: exactly one of `text` or `inlineData` is set.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestPart<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inline_data: Option<Blob<'a>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Blob<'a> {
    pub mime_type: &'a str,
    /// Standard base64 with padding.
    pub data: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerationConfig<'a> {
    pub response_modalities: [&'static str; 1],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_config: Option<ImageConfig<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_config: Option<ThinkingConfig>,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageConfig<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aspect_ratio: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_size: Option<&'a str>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingConfig {
    /// `MINIMAL` or `HIGH`.
    pub thinking_level: &'static str,
}

/// `GenerateContentResponse`.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct GenerateContentResponse {
    pub candidates: Option<Vec<Candidate>>,
    pub prompt_feedback: Option<PromptFeedback>,
    /// Kept as JSON: it becomes `Usage::provider_usage` after sanitizing.
    pub usage_metadata: Option<Value>,
    pub response_id: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Candidate {
    pub content: Option<CandidateContent>,
    pub finish_reason: Option<String>,
    pub finish_message: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct CandidateContent {
    pub parts: Option<Vec<ResponsePart>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ResponsePart {
    pub text: Option<String>,
    pub inline_data: Option<ResponseBlob>,
    /// `true` for thought parts (interim "thought images" and reasoning text).
    pub thought: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ResponseBlob {
    pub mime_type: Option<String>,
    pub data: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PromptFeedback {
    pub block_reason: Option<String>,
    pub block_reason_message: Option<String>,
}

// ----- predictLongRunning (Veo) ------------------------------------------------

/// `POST /v1beta/models/{model}:predictLongRunning` body (official SDK wire form).
#[derive(Debug, Serialize)]
pub struct PredictLongRunningRequest<'a> {
    pub instances: [VeoInstance<'a>; 1],
    pub parameters: VeoParameters<'a>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VeoInstance<'a> {
    pub prompt: &'a str,
    /// First frame (image-to-video).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<VeoImage<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_frame: Option<VeoImage<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference_images: Option<Vec<VeoReference<'a>>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VeoImage<'a> {
    pub bytes_base64_encoded: String,
    pub mime_type: &'a str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VeoReference<'a> {
    pub image: VeoImage<'a>,
    /// `ASSET` (the SDK enum value; the guide's REST sample shows lowercase).
    pub reference_type: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VeoParameters<'a> {
    pub aspect_ratio: &'a str,
    pub resolution: &'a str,
    /// JSON integer (the SDKs send an int; the guide's table shows strings).
    pub duration_seconds: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub negative_prompt: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub person_generation: Option<&'a str>,
}

/// `google.longrunning.Operation`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Operation {
    pub name: Option<String>,
    pub done: Option<bool>,
    pub metadata: Option<Value>,
    pub error: Option<RpcStatus>,
    pub response: Option<OperationResponse>,
}

/// `google.rpc.Status` inside an operation (`code` is a `google.rpc.Code` number).
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct RpcStatus {
    pub code: Option<i64>,
    pub message: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct OperationResponse {
    pub generate_video_response: Option<GenerateVideoResponse>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct GenerateVideoResponse {
    pub generated_samples: Option<Vec<GeneratedSample>>,
    pub rai_media_filtered_count: Option<i64>,
    pub rai_media_filtered_reasons: Option<Vec<String>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct GeneratedSample {
    pub video: Option<GeneratedVideo>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct GeneratedVideo {
    pub uri: Option<String>,
    /// Inline bytes (not supported by the Gemini API per the SDK; detected only).
    pub encoded_video: Option<String>,
}

// ----- errors ------------------------------------------------------------------

/// HTTP error body: `{"error": google.rpc.Status}` with HTTP `code`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ErrorBody {
    pub error: Option<ErrorStatus>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ErrorStatus {
    pub message: Option<String>,
    /// Canonical status name, e.g. `INVALID_ARGUMENT`, `RESOURCE_EXHAUSTED`.
    pub status: Option<String>,
    /// `google.rpc` detail messages (`ErrorInfo`, `RetryInfo`, …), each with `@type`.
    pub details: Option<Vec<Value>>,
}
