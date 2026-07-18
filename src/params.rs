use schemars::JsonSchema;
use serde::Deserialize;

use crate::api::ReasoningEffort;

/// Image detail level for the `chat_with_vision` tool.
#[derive(Debug, Clone, Copy, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ImageDetail {
    /// Low detail — faster, fewer tokens.
    Low,
    /// High detail — more accurate.
    High,
    /// Let the model decide (default).
    #[default]
    Auto,
}

impl ImageDetail {
    /// Return the string representation used by the OpenRouter API.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::High => "high",
            Self::Auto => "auto",
        }
    }
}

/// Parameters for the `chat` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ChatParams {
    #[schemars(description = "The user message / prompt to send")]
    pub prompt: String,

    #[schemars(description = "Optional system prompt to set context/behaviour")]
    pub system_prompt: Option<String>,

    #[schemars(
        description = "Full conversation history as a JSON array of {role, content} objects. When provided, 'prompt' is appended as the final user message."
    )]
    pub messages: Option<String>,

    #[schemars(
        description = "Model ID, e.g. 'moonshotai/kimi-k3', 'anthropic/claude-sonnet-4.5', 'openai/gpt-5'. Defaults to the configured default (moonshotai/kimi-k3). Call list_models to browse."
    )]
    pub model: Option<String>,

    #[schemars(
        description = "Sampling temperature (0.0 - 2.0). Models that don't support it (e.g. kimi-k3) silently ignore it."
    )]
    pub temperature: Option<f32>,

    #[schemars(description = "Maximum tokens to generate")]
    pub max_tokens: Option<u32>,

    #[schemars(
        description = "Reasoning effort (low/medium/high) for reasoning-capable models. Reasoning runs but is hidden from the output unless show_reasoning=true."
    )]
    pub reasoning_effort: Option<ReasoningEffort>,

    #[schemars(
        description = "Include the model's reasoning text in the output as a [reasoning] block (default false)."
    )]
    pub show_reasoning: Option<bool>,

    #[schemars(description = "Optional JSON schema string to enforce structured output.")]
    pub response_schema: Option<String>,
}

/// Parameters for the `chat_with_vision` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct VisionParams {
    #[schemars(description = "Text prompt describing what to analyse in the image")]
    pub prompt: String,

    #[schemars(
        description = "Image to analyse: an http(s) URL, a data: URL, or a local file path (png/jpg/jpeg/webp/gif, max 20 MB — converted to a base64 data URL)."
    )]
    pub image: String,

    #[schemars(description = "Image detail level: low, high, or auto (default auto)")]
    pub detail: Option<ImageDetail>,

    #[schemars(
        description = "Model ID; must be vision-capable. The default moonshotai/kimi-k3 accepts images."
    )]
    pub model: Option<String>,

    #[schemars(description = "Sampling temperature (0.0 - 2.0)")]
    pub temperature: Option<f32>,

    #[schemars(description = "Maximum tokens to generate")]
    pub max_tokens: Option<u32>,
}

/// Parameters for the `chat_with_search` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchParams {
    #[schemars(description = "The user message / prompt")]
    pub prompt: String,

    #[schemars(description = "Optional system prompt")]
    pub system_prompt: Option<String>,

    #[schemars(
        description = "Model ID. Defaults to the configured default (moonshotai/kimi-k3)."
    )]
    pub model: Option<String>,

    #[schemars(
        description = "Max web results to ground on, 1-20 (default 5). Each result costs about $0.004."
    )]
    pub max_results: Option<u32>,

    #[schemars(description = "Sampling temperature (0.0 - 2.0)")]
    pub temperature: Option<f32>,

    #[schemars(description = "Maximum tokens to generate")]
    pub max_tokens: Option<u32>,
}

/// Parameters for the `list_models` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListModelsParams {
    #[schemars(
        description = "Case-insensitive substring matched against model id and name, e.g. 'kimi' or 'anthropic'. Omit to list all (~344 models)."
    )]
    pub filter: Option<String>,
}
