//! Request and response types for the `OpenAI` Chat Completions API.

use serde::{Deserialize, Serialize};

// ── Request types ───────────────────────────────────────────

/// Request body for `POST /v1/chat/completions`.
#[derive(Debug, Clone, Serialize)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub stream: bool,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_completion_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_format: Option<ResponseFormat>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_options: Option<StreamOptions>,
    /// Functions the model may call (client-side tools).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDef>>,
    /// `"auto"` (server default when omitted), `"none"`, or `"required"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<serde_json::Value>,
    /// Fields the operator adds to the request body as they are, for what
    /// a server takes beyond the common API (llama-server's
    /// `chat_template_kwargs`, vLLM's `extra_body` keys). Serialised at
    /// the top level beside the fields above.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// One declared tool: `{ "type": "function", "function": { ... } }`.
#[derive(Debug, Clone, Serialize)]
pub struct ToolDef {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub function: FunctionDef,
}

#[derive(Debug, Clone, Serialize)]
pub struct FunctionDef {
    pub name: String,
    pub description: String,
    /// JSON Schema of the function parameters.
    pub parameters: serde_json::Value,
}

/// A single message in the chat conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

/// Constrains the output format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseFormat {
    #[serde(rename = "type")]
    pub format_type: String,
}

/// Options for streaming responses.
#[derive(Debug, Clone, Serialize)]
pub struct StreamOptions {
    pub include_usage: bool,
}

// ── Non-streaming response types ────────────────────────────

/// Full response from a non-streaming chat completion.
#[derive(Debug, Deserialize)]
pub struct ChatCompletionResponse {
    #[expect(dead_code, reason = "wire-format field, deserialized but not read")]
    pub id: String,
    pub choices: Vec<ChatChoice>,
    pub usage: Option<Usage>,
}

#[derive(Debug, Deserialize)]
pub struct ChatChoice {
    pub message: ChatChoiceMessage,
    #[expect(dead_code, reason = "wire-format field, deserialized but not read")]
    pub finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ChatChoiceMessage {
    pub content: Option<String>,
}

// ── Streaming response types ────────────────────────────────

/// A single chunk from a streaming chat completion (SSE `data:` line).
#[derive(Debug, Deserialize)]
pub struct ChatCompletionChunk {
    #[expect(dead_code, reason = "wire-format field, deserialized but not read")]
    pub id: Option<String>,
    pub choices: Vec<ChunkChoice>,
    pub usage: Option<Usage>,
}

#[derive(Debug, Deserialize)]
pub struct ChunkChoice {
    pub delta: ChunkDelta,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ChunkDelta {
    pub content: Option<String>,
    /// Streamed function-call deltas: `id`/`name` arrive on the first
    /// delta of each call, `arguments` accumulates as string fragments
    /// correlated by `index`.
    #[serde(default)]
    pub tool_calls: Option<Vec<ToolCallDelta>>,
}

/// One streamed `delta.tool_calls[]` entry.
#[derive(Debug, Clone, Deserialize)]
pub struct ToolCallDelta {
    /// Position of the call in the response (correlates fragments).
    pub index: Option<u32>,
    /// Provider call id — present on the first fragment only.
    pub id: Option<String>,
    #[serde(default)]
    pub function: Option<FunctionCallDelta>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FunctionCallDelta {
    pub name: Option<String>,
    /// Argument JSON, streamed as string fragments.
    pub arguments: Option<String>,
}

// ── Shared types ────────────────────────────────────────────

/// Token usage statistics.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[expect(
    clippy::struct_field_names,
    reason = "the OpenAI usage object's own field names"
)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

// ── Error response ──────────────────────────────────────────

/// Error body returned by the API. Parsed to tell the API's error shape
/// from anything else; its words are not logged.
#[derive(Debug, Deserialize)]
pub struct ApiErrorResponse {
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "wire-format field, deserialized but not read")
    )]
    pub error: ApiError,
}

#[derive(Debug, Deserialize)]
pub struct ApiError {
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "wire-format field, deserialized but not read")
    )]
    pub message: String,
    #[serde(rename = "type")]
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "wire-format field, deserialized but not read")
    )]
    pub error_type: Option<String>,
    #[expect(dead_code, reason = "wire-format field, deserialized but not read")]
    pub code: Option<String>,
}

// ── Tests ───────────────────────────────────────────────────

#[cfg(test)]
#[path = "tests/types.rs"]
mod tests;
