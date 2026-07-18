use reqwest::{Client, Method};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fmt;
use std::time::Duration;
use thiserror::Error;
use tracing::instrument;

/// Reasoning effort levels accepted by OpenRouter's unified `reasoning` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    Low,
    Medium,
    High,
}

/// Errors returned by the OpenRouter API client.
#[derive(Error, Debug)]
pub enum ApiError {
    #[error("HTTP request failed: {0}")]
    Reqwest(#[from] reqwest::Error),

    #[error("OpenRouter API error ({status}): {body}")]
    Api {
        status: reqwest::StatusCode,
        body: String,
    },
}

/// Shared HTTP client for all OpenRouter API calls.
pub struct OpenRouterClient {
    api_key: String,
    base_url: String,
    site_url: Option<String>,
    app_name: Option<String>,
    http: Client,
}

impl OpenRouterClient {
    /// Create a new client with optional attribution headers.
    pub fn new(
        api_key: String,
        base_url: String,
        site_url: Option<String>,
        app_name: Option<String>,
    ) -> Self {
        Self {
            api_key,
            base_url,
            site_url,
            app_name,
            // Reasoning models routed via OpenRouter can run for several minutes.
            http: Client::builder()
                .timeout(Duration::from_secs(600))
                .build()
                .expect("Failed to build reqwest client"),
        }
    }

    /// Unified HTTP request method — handles GET and POST with optional body.
    /// Adds Authorization plus optional OpenRouter attribution headers.
    #[instrument(skip(self, body), fields(path = %path))]
    pub async fn request<Req: Serialize, Resp: for<'de> Deserialize<'de>>(
        &self,
        method: Method,
        path: &str,
        body: Option<&Req>,
    ) -> Result<Resp, ApiError> {
        let url = format!("{}{path}", self.base_url);
        let mut builder = self
            .http
            .request(method, &url)
            .header("Authorization", format!("Bearer {}", self.api_key));
        if let Some(u) = &self.site_url {
            builder = builder.header("HTTP-Referer", u);
        }
        if let Some(n) = &self.app_name {
            builder = builder.header("X-Title", n);
        }
        if let Some(b) = body {
            builder = builder.json(b);
        }
        let response = builder.send().await?;
        let status = response.status();
        if !status.is_success() {
            let body = match response.text().await {
                Ok(text) => text,
                Err(e) => format!("<failed to read response body: {e}>"),
            };
            tracing::warn!(status = %status, "API request failed");
            return Err(ApiError::Api { status, body });
        }
        Ok(response.json::<Resp>().await?)
    }
}

// ---------------------------------------------------------------------------
// Chat Completions API types
// ---------------------------------------------------------------------------

/// A chat completion request to the OpenRouter API.
#[derive(Debug, Serialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_format: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plugins: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Value>,
}

impl ChatRequest {
    /// Create a new chat request. Always opts into actual-cost usage accounting.
    pub fn new(model: &str, messages: Vec<ChatMessage>) -> Self {
        Self {
            model: model.into(),
            messages,
            temperature: None,
            max_tokens: None,
            response_format: None,
            reasoning: None,
            plugins: None,
            usage: Some(json!({"include": true})),
        }
    }
}

/// A single message in a chat conversation.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ChatMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl ChatMessage {
    /// Create a system message.
    pub fn system(text: &str) -> Self {
        Self {
            role: "system".into(),
            content: Some(Value::String(text.into())),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    /// Create a user text message.
    pub fn user(text: &str) -> Self {
        Self {
            role: "user".into(),
            content: Some(Value::String(text.into())),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    /// Create a user message with both text and an image URL.
    pub fn user_with_image(text: &str, image_url: &str, detail: &str) -> Self {
        Self {
            role: "user".into(),
            content: Some(json!([
                { "type": "text", "text": text },
                {
                    "type": "image_url",
                    "image_url": { "url": image_url, "detail": detail }
                }
            ])),
            tool_calls: None,
            tool_call_id: None,
        }
    }
}

/// The response from a chat completion request.
///
/// `choices` defaults to empty because OpenRouter can return HTTP 200 with only
/// a top-level `error` object (no `choices` at all).
#[derive(Debug, Deserialize)]
pub struct ChatResponse {
    #[serde(default)]
    pub choices: Vec<ChatChoice>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub usage: Option<Usage>,
    /// The HTTP-200-with-error quirk — body is `{"error":...}` with no choices.
    #[serde(default)]
    pub error: Option<ApiErrorBody>,
}

/// A single choice in a chat completion response.
#[derive(Debug, Deserialize)]
pub struct ChatChoice {
    pub message: ChatResponseMessage,
    #[serde(default)]
    pub finish_reason: Option<String>,
    /// Per-choice provider error (upstream failure while HTTP status is still 200).
    #[serde(default)]
    pub error: Option<ApiErrorBody>,
}

/// The message content within a chat choice.
#[derive(Debug, Deserialize)]
pub struct ChatResponseMessage {
    pub role: String,
    #[serde(default)]
    pub content: Option<String>,
    /// Unified reasoning text from reasoning-capable models.
    #[serde(default)]
    pub reasoning: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<Value>>,
    /// url_citation blocks from the web plugin.
    #[serde(default)]
    pub annotations: Option<Vec<Value>>,
}

/// OpenRouter error body (top-level or per-choice).
#[derive(Debug, Deserialize)]
pub struct ApiErrorBody {
    #[serde(default)]
    pub code: Option<Value>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub metadata: Option<Value>,
}

impl ApiErrorBody {
    /// Render a human-readable error string.
    pub fn render(&self) -> String {
        let code = self
            .code
            .as_ref()
            .map(|c| {
                let s = c.to_string();
                s.trim_matches('"').to_string()
            })
            .unwrap_or_else(|| "unknown".into());
        let message = self
            .message
            .as_deref()
            .unwrap_or("unknown");
        let mut out = format!("OpenRouter error ({code}): {message}");
        if let Some(meta) = &self.metadata
            && !meta.is_null()
        {
            let pretty = serde_json::to_string_pretty(meta).unwrap_or_else(|_| meta.to_string());
            out.push_str(&format!("\nmetadata: {pretty}"));
        }
        out
    }
}

/// Token usage statistics, including actual USD cost when `usage.include` was sent.
#[derive(Debug, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    /// Actual USD charged by OpenRouter (because usage.include was sent).
    #[serde(default)]
    pub cost: Option<f64>,
    #[serde(default)]
    pub prompt_tokens_details: Option<PromptTokensDetails>,
    #[serde(default)]
    pub completion_tokens_details: Option<CompletionTokensDetails>,
}

#[derive(Debug, Deserialize)]
pub struct PromptTokensDetails {
    #[serde(default)]
    pub cached_tokens: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct CompletionTokensDetails {
    #[serde(default)]
    pub reasoning_tokens: Option<u32>,
}

// ---------------------------------------------------------------------------
// Models catalog
// ---------------------------------------------------------------------------

/// The response from listing available models.
#[derive(Debug, Deserialize)]
pub struct ModelsResponse {
    pub data: Vec<ModelInfo>,
}

/// Information about a single model (unknown fields are skipped by serde).
#[derive(Debug, Clone, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub context_length: Option<u64>,
    #[serde(default)]
    pub pricing: Option<ModelPricing>,
}

/// Pricing as USD-per-token strings (multiply by 1e6 for $/M).
#[derive(Debug, Clone, Deserialize)]
pub struct ModelPricing {
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub completion: Option<String>,
}

// ---------------------------------------------------------------------------
// Credits
// ---------------------------------------------------------------------------

/// The response from GET /credits.
#[derive(Debug, Deserialize)]
pub struct CreditsResponse {
    pub data: CreditsData,
}

/// Account credit balances.
#[derive(Debug, Deserialize)]
pub struct CreditsData {
    pub total_credits: f64,
    pub total_usage: f64,
}

impl fmt::Display for CreditsResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let remaining = self.data.total_credits - self.data.total_usage;
        write!(
            f,
            "Credits: ${:.4} purchased, ${:.4} used, ${:.4} remaining",
            self.data.total_credits, self.data.total_usage, remaining
        )
    }
}

// ---------------------------------------------------------------------------
// Display for ChatResponse — the core formatter
// ---------------------------------------------------------------------------

impl fmt::Display for ChatResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut first = true;
        for choice in &self.choices {
            if !first {
                writeln!(f)?;
            }
            first = false;

            // Reasoning block before content (when non-empty).
            if let Some(reasoning) = &choice.message.reasoning
                && !reasoning.is_empty()
            {
                write!(f, "[reasoning]\n{reasoning}\n[/reasoning]\n")?;
            }

            if choice.message.role != "assistant" {
                write!(f, "[{}] ", choice.message.role)?;
            }
            if let Some(content) = &choice.message.content {
                write!(f, "{content}")?;
            }
            if let Some(tool_calls) = &choice.message.tool_calls {
                let formatted = serde_json::to_string_pretty(tool_calls)
                    .unwrap_or_else(|e| format!("<failed to format tool calls: {e}>"));
                write!(f, "\nTool calls: {formatted}")?;
            }
            if let Some(err) = &choice.error {
                write!(f, "\n[provider error: {}]", err.render())?;
            }
            if let Some(reason) = &choice.finish_reason {
                write!(f, "\n[finish_reason: {reason}]")?;
            }
        }

        // Sources from url_citation annotations (dedupe by url, preserve order).
        let mut sources: Vec<(String, String)> = Vec::new();
        for choice in &self.choices {
            if let Some(annotations) = &choice.message.annotations {
                for ann in annotations {
                    let ann_type = ann.get("type").and_then(Value::as_str).unwrap_or("");
                    if ann_type != "url_citation" {
                        continue;
                    }
                    let cite = match ann.get("url_citation") {
                        Some(c) => c,
                        None => continue,
                    };
                    let url = cite.get("url").and_then(Value::as_str).unwrap_or("");
                    if url.is_empty() {
                        continue;
                    }
                    if sources.iter().any(|(_, u)| u == url) {
                        continue;
                    }
                    let title = cite
                        .get("title")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    sources.push((title, url.to_string()));
                }
            }
        }
        if !sources.is_empty() {
            write!(f, "\n\nSources:")?;
            for (title, url) in &sources {
                if title.is_empty() {
                    write!(f, "\n- {url}")?;
                } else {
                    write!(f, "\n- {title} — {url}")?;
                }
            }
        }

        // Model + provider routing info.
        if let Some(model) = &self.model {
            write!(f, "\n[model: {model}")?;
            if let Some(provider) = &self.provider {
                write!(f, " via {provider}")?;
            }
            write!(f, "]")?;
        }

        // Token usage with optional cached/reasoning detail.
        if let Some(usage) = &self.usage {
            write!(
                f,
                "\n[tokens: {} prompt + {} completion = {} total",
                usage.prompt_tokens, usage.completion_tokens, usage.total_tokens
            )?;
            if let Some(details) = &usage.prompt_tokens_details
                && let Some(cached) = details.cached_tokens
                && cached > 0
            {
                write!(f, "; {cached} cached")?;
            }
            if let Some(details) = &usage.completion_tokens_details
                && let Some(reasoning) = details.reasoning_tokens
                && reasoning > 0
            {
                write!(f, "; {reasoning} reasoning")?;
            }
            write!(f, "]")?;

            // Actual OpenRouter-reported USD cost.
            if let Some(cost) = usage.cost {
                write!(f, "\n[cost: ${cost:.6}]")?;
            }
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(prompt: u32, completion: u32, total: u32) -> Usage {
        Usage {
            prompt_tokens: prompt,
            completion_tokens: completion,
            total_tokens: total,
            cost: None,
            prompt_tokens_details: None,
            completion_tokens_details: None,
        }
    }

    fn choice(content: &str, finish: Option<&str>) -> ChatChoice {
        ChatChoice {
            message: ChatResponseMessage {
                role: "assistant".into(),
                content: Some(content.into()),
                reasoning: None,
                tool_calls: None,
                annotations: None,
            },
            finish_reason: finish.map(Into::into),
            error: None,
        }
    }

    #[test]
    fn display_basic() {
        let resp = ChatResponse {
            choices: vec![choice("Hello!", Some("stop"))],
            model: None,
            provider: None,
            usage: Some(usage(10, 5, 15)),
            error: None,
        };
        let output = resp.to_string();
        assert!(output.contains("Hello!"));
        assert!(output.contains("[finish_reason: stop]"));
        assert!(output.contains("[tokens: 10 prompt + 5 completion = 15 total]"));
    }

    #[test]
    fn display_cost_and_token_details() {
        let mut u = usage(10, 5, 15);
        u.cost = Some(0.00123);
        u.prompt_tokens_details = Some(PromptTokensDetails {
            cached_tokens: Some(640),
        });
        u.completion_tokens_details = Some(CompletionTokensDetails {
            reasoning_tokens: Some(210),
        });
        let resp = ChatResponse {
            choices: vec![choice("ok", Some("stop"))],
            model: None,
            provider: None,
            usage: Some(u),
            error: None,
        };
        let output = resp.to_string();
        assert!(output.contains("[cost: $0.001230]"));
        assert!(output.contains("; 640 cached"));
        assert!(output.contains("; 210 reasoning"));
    }

    #[test]
    fn display_model_line() {
        let resp = ChatResponse {
            choices: vec![choice("hi", None)],
            model: Some("moonshotai/kimi-k3".into()),
            provider: Some("Moonshot AI".into()),
            usage: None,
            error: None,
        };
        let output = resp.to_string();
        assert!(output.contains("[model: moonshotai/kimi-k3 via Moonshot AI]"));
    }

    #[test]
    fn display_reasoning_block() {
        let mut c = choice("answer", None);
        c.message.reasoning = Some("thought".into());
        let resp = ChatResponse {
            choices: vec![c],
            model: None,
            provider: None,
            usage: None,
            error: None,
        };
        let output = resp.to_string();
        assert!(output.contains("[reasoning]"));
        assert!(output.contains("thought"));
        assert!(output.find("[reasoning]").unwrap() < output.find("answer").unwrap());

        // Empty reasoning is skipped.
        let mut c2 = choice("answer", None);
        c2.message.reasoning = Some("".into());
        let resp2 = ChatResponse {
            choices: vec![c2],
            model: None,
            provider: None,
            usage: None,
            error: None,
        };
        assert!(!resp2.to_string().contains("[reasoning]"));
    }

    #[test]
    fn display_annotations_sources() {
        let mut c = choice("based on search", None);
        c.message.annotations = Some(vec![
            json!({
                "type": "url_citation",
                "url_citation": {
                    "url": "https://example.com/a",
                    "title": "Example A"
                }
            }),
            json!({
                "type": "url_citation",
                "url_citation": {
                    "url": "https://example.com/a",
                    "title": "Example A dup"
                }
            }),
            json!({
                "type": "url_citation",
                "url_citation": {
                    "url": "https://example.com/b",
                    "title": "Example B"
                }
            }),
        ]);
        let resp = ChatResponse {
            choices: vec![c],
            model: None,
            provider: None,
            usage: None,
            error: None,
        };
        let output = resp.to_string();
        assert!(output.contains("Sources:"));
        assert!(output.contains("Example A — https://example.com/a"));
        assert!(output.contains("Example B — https://example.com/b"));
        // Deduped: only one occurrence of the A url.
        assert_eq!(output.matches("https://example.com/a").count(), 1);
    }

    #[test]
    fn display_choice_error() {
        let mut c = choice("", None);
        c.error = Some(ApiErrorBody {
            code: Some(json!(502)),
            message: Some("provider down".into()),
            metadata: None,
        });
        let resp = ChatResponse {
            choices: vec![c],
            model: None,
            provider: None,
            usage: None,
            error: None,
        };
        let output = resp.to_string();
        assert!(output.contains("[provider error:"));
        assert!(output.contains("provider down"));
    }

    #[test]
    fn deserialize_error_only_body() {
        let resp: ChatResponse =
            serde_json::from_str(r#"{"error":{"code":402,"message":"Insufficient credits"}}"#)
                .unwrap();
        assert!(resp.choices.is_empty());
        assert!(resp.error.is_some());
        let err = resp.error.unwrap();
        assert_eq!(err.message.as_deref(), Some("Insufficient credits"));
    }

    #[test]
    fn display_credits() {
        let resp = CreditsResponse {
            data: CreditsData {
                total_credits: 25.0,
                total_usage: 3.2154,
            },
        };
        assert_eq!(
            resp.to_string(),
            "Credits: $25.0000 purchased, $3.2154 used, $21.7846 remaining"
        );
    }

    #[test]
    fn display_empty_choices() {
        let resp = ChatResponse {
            choices: vec![],
            model: None,
            provider: None,
            usage: None,
            error: None,
        };
        assert_eq!(resp.to_string(), "");
    }
}
