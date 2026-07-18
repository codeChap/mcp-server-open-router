use base64::Engine;
use moka::future::Cache;
use reqwest::Method;
use rmcp::{
    ErrorData as McpError, ServerHandler, handler::server::tool::ToolRouter,
    handler::server::wrapper::Parameters, model::*, tool, tool_handler, tool_router,
};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;
use tracing::debug;

use crate::api::{
    ChatMessage, ChatRequest, ChatResponse, CreditsResponse, ModelInfo, ModelsResponse,
    OpenRouterClient, ReasoningEffort,
};
use crate::params::{ChatParams, ListModelsParams, SearchParams, VisionParams};

const DEFAULT_MODEL: &str = "moonshotai/kimi-k3";
const DEFAULT_MAX_RESULTS: u32 = 5;
const MAX_SEARCH_RESULTS: u32 = 20;
const MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024;
const VALID_ROLES: &[&str] = &["system", "user", "assistant", "tool"];

/// The MCP server wrapping the OpenRouter API.
#[derive(Clone)]
pub struct OpenRouterServer {
    client: Arc<OpenRouterClient>,
    default_model: String,
    default_max_tokens: Option<u32>,
    models_cache: Cache<(), Arc<Vec<ModelInfo>>>,
    tool_router: ToolRouter<Self>,
}

// ---------------------------------------------------------------------------
// Shared helpers — pure associated fns so tests need no client
// ---------------------------------------------------------------------------

impl OpenRouterServer {
    /// Validate temperature is within the allowed range and is a finite number.
    fn validate_temperature(temp: Option<f32>) -> Result<(), McpError> {
        if let Some(t) = temp
            && (!t.is_finite() || !(0.0..=2.0).contains(&t))
        {
            return Err(McpError::invalid_params(
                format!("temperature must be a finite number between 0.0 and 2.0, got {t}"),
                None,
            ));
        }
        Ok(())
    }

    /// Build the messages vec from optional system prompt, optional history, and current prompt.
    fn build_messages(
        system_prompt: Option<&str>,
        history_json: Option<&str>,
        prompt: &str,
    ) -> Result<Vec<ChatMessage>, String> {
        let mut messages = Vec::new();

        if let Some(sys) = system_prompt {
            messages.push(ChatMessage::system(sys));
        }

        if let Some(json) = history_json {
            let parsed: Vec<ChatMessage> =
                serde_json::from_str(json).map_err(|e| format!("Invalid messages JSON: {e}"))?;
            for msg in &parsed {
                if !VALID_ROLES.contains(&msg.role.as_str()) {
                    return Err(format!(
                        "Invalid role '{}' in messages — must be one of: {}",
                        msg.role,
                        VALID_ROLES.join(", ")
                    ));
                }
            }
            messages.extend(parsed);
        }

        messages.push(ChatMessage::user(prompt));
        Ok(messages)
    }

    /// Build a ChatRequest with shared optional fields applied.
    /// Always includes `usage: {"include": true}` via `ChatRequest::new`.
    fn build_chat_request(
        model: &str,
        messages: Vec<ChatMessage>,
        temperature: Option<f32>,
        max_tokens: Option<u32>,
        response_schema: Option<&str>,
        reasoning: Option<Value>,
        plugins: Option<Vec<Value>>,
    ) -> Result<ChatRequest, String> {
        let mut req = ChatRequest::new(model, messages);
        req.temperature = temperature;
        req.max_tokens = max_tokens;
        req.reasoning = reasoning;
        req.plugins = plugins;

        if let Some(schema_str) = response_schema {
            let schema: Value = serde_json::from_str(schema_str)
                .map_err(|e| format!("Invalid response_schema JSON: {e}"))?;
            req.response_format = Some(json!({
                "type": "json_schema",
                "json_schema": {
                    "name": "structured_output",
                    "strict": true,
                    "schema": schema
                }
            }));
        }

        Ok(req)
    }

    /// Resolve max_tokens: per-call override, else server default, else None (omit).
    fn resolve_max_tokens(&self, requested: Option<u32>) -> Option<u32> {
        requested.or(self.default_max_tokens)
    }

    /// Build the OpenRouter `reasoning` field from effort + show_reasoning.
    ///
    /// | effort | show_reasoning | result |
    /// | None   | false          | field omitted |
    /// | Some   | false          | `{"effort": e, "exclude": true}` |
    /// | Some   | true           | `{"effort": e}` |
    /// | None   | true           | `{"enabled": true}` |
    fn build_reasoning(effort: Option<ReasoningEffort>, show_reasoning: bool) -> Option<Value> {
        match (effort, show_reasoning) {
            (None, false) => None,
            (Some(e), false) => Some(json!({"effort": e, "exclude": true})),
            (Some(e), true) => Some(json!({"effort": e})),
            (None, true) => Some(json!({"enabled": true})),
        }
    }

    /// Build the web-search plugin array.
    fn web_plugin(max_results: u32) -> Vec<Value> {
        vec![json!({"id": "web", "max_results": max_results})]
    }

    /// Map a file extension to a MIME type for local image loading.
    fn mime_for_extension(ext: &str) -> Option<&'static str> {
        match ext.to_lowercase().as_str() {
            "png" => Some("image/png"),
            "jpg" | "jpeg" => Some("image/jpeg"),
            "webp" => Some("image/webp"),
            "gif" => Some("image/gif"),
            _ => None,
        }
    }

    /// Resolve an image input to a URL OpenRouter can fetch.
    ///
    /// Passes through `http://`, `https://`, and `data:` URLs. Local paths are
    /// read, size-checked, and converted to a base64 data URL.
    fn resolve_image(input: &str) -> Result<String, String> {
        if input.starts_with("http://")
            || input.starts_with("https://")
            || input.starts_with("data:")
        {
            return Ok(input.to_string());
        }

        // Treat as local path.
        let path = std::path::Path::new(input);
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        let mime = Self::mime_for_extension(ext).ok_or_else(|| {
            format!(
                "Unsupported image extension '{ext}' — supported: png, jpg, jpeg, webp, gif"
            )
        })?;

        let bytes = std::fs::read(path)
            .map_err(|e| format!("Failed to read image file '{input}': {e}"))?;
        if bytes.len() > MAX_IMAGE_BYTES {
            return Err(format!(
                "Image file '{input}' is {} bytes (max {} bytes / 20 MB)",
                bytes.len(),
                MAX_IMAGE_BYTES
            ));
        }

        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        Ok(format!("data:{mime};base64,{b64}"))
    }

    /// POST /chat/completions and format the result (or surface errors as tool errors).
    async fn do_chat(&self, req: &ChatRequest) -> Result<CallToolResult, McpError> {
        match self
            .client
            .request::<_, ChatResponse>(Method::POST, "/chat/completions", Some(req))
            .await
        {
            Ok(resp) => {
                // HTTP-200-with-error quirk: body is `{"error":...}` with no choices.
                if let Some(err) = &resp.error {
                    return Ok(CallToolResult::error(vec![Content::text(err.render())]));
                }
                Ok(CallToolResult::success(vec![Content::text(resp.to_string())]))
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
        }
    }

    /// Filter models by case-insensitive substring over id and name.
    fn filter_models<'a>(models: &'a [ModelInfo], filter: Option<&str>) -> Vec<&'a ModelInfo> {
        match filter {
            None => models.iter().collect(),
            Some(f) => {
                let f_lower = f.to_lowercase();
                models
                    .iter()
                    .filter(|m| {
                        m.id.to_lowercase().contains(&f_lower)
                            || m.name
                                .as_ref()
                                .map(|n| n.to_lowercase().contains(&f_lower))
                                .unwrap_or(false)
                    })
                    .collect()
            }
        }
    }

    /// Format one model line with context length and $/M pricing.
    fn format_model_line(m: &ModelInfo) -> String {
        let name_part = m
            .name
            .as_ref()
            .map(|n| format!(" ({n})"))
            .unwrap_or_default();
        let ctx = m
            .context_length
            .map(|n| n.to_string())
            .unwrap_or_else(|| "n/a".into());
        let price_m = |s: &Option<String>| -> String {
            s.as_ref()
                .and_then(|p| p.parse::<f64>().ok())
                .map(|v| format!("${:.2}/M", v * 1_000_000.0))
                .unwrap_or_else(|| "n/a".into())
        };
        let pin = m
            .pricing
            .as_ref()
            .map(|p| price_m(&p.prompt))
            .unwrap_or_else(|| "n/a".into());
        let pout = m
            .pricing
            .as_ref()
            .map(|p| price_m(&p.completion))
            .unwrap_or_else(|| "n/a".into());
        format!("- {}{name_part} | ctx {ctx} | {pin} in, {pout} out", m.id)
    }
}

// ---------------------------------------------------------------------------
// Tool definitions
// ---------------------------------------------------------------------------

#[tool_router]
impl OpenRouterServer {
    pub fn new(
        client: OpenRouterClient,
        default_model: Option<String>,
        default_max_tokens: Option<u32>,
    ) -> Self {
        let models_cache = Cache::builder()
            .max_capacity(1)
            .time_to_live(Duration::from_secs(300))
            .build();

        Self {
            client: Arc::new(client),
            default_model: default_model.unwrap_or_else(|| DEFAULT_MODEL.to_string()),
            default_max_tokens,
            models_cache,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "Chat with any OpenRouter model (default: moonshotai/kimi-k3). Supports multi-turn history, a system prompt, structured output via JSON schema, and reasoning control. Every response ends with a token-usage and actual USD cost footer."
    )]
    async fn chat(
        &self,
        Parameters(p): Parameters<ChatParams>,
    ) -> Result<CallToolResult, McpError> {
        debug!(model = ?p.model, "chat tool called");
        Self::validate_temperature(p.temperature)?;

        let messages =
            Self::build_messages(p.system_prompt.as_deref(), p.messages.as_deref(), &p.prompt)
                .map_err(|e| McpError::invalid_params(e, None))?;

        let reasoning =
            Self::build_reasoning(p.reasoning_effort, p.show_reasoning.unwrap_or(false));

        let req = Self::build_chat_request(
            p.model.as_deref().unwrap_or(&self.default_model),
            messages,
            p.temperature,
            self.resolve_max_tokens(p.max_tokens),
            p.response_schema.as_deref(),
            reasoning,
            None,
        )
        .map_err(|e| McpError::invalid_params(e, None))?;

        self.do_chat(&req).await
    }

    #[tool(
        description = "Analyse an image with a vision-capable OpenRouter model (the default moonshotai/kimi-k3 accepts images). Provide the image as an http(s) URL, a data: URL, or a local file path."
    )]
    async fn chat_with_vision(
        &self,
        Parameters(p): Parameters<VisionParams>,
    ) -> Result<CallToolResult, McpError> {
        debug!(model = ?p.model, "chat_with_vision tool called");
        Self::validate_temperature(p.temperature)?;

        let url =
            Self::resolve_image(&p.image).map_err(|e| McpError::invalid_params(e, None))?;

        let messages = vec![ChatMessage::user_with_image(
            &p.prompt,
            &url,
            p.detail.unwrap_or_default().as_str(),
        )];

        let req = Self::build_chat_request(
            p.model.as_deref().unwrap_or(&self.default_model),
            messages,
            p.temperature,
            self.resolve_max_tokens(p.max_tokens),
            None,
            None,
            None,
        )
        .map_err(|e| McpError::invalid_params(e, None))?;

        self.do_chat(&req).await
    }

    #[tool(
        description = "Chat with live web-search grounding (OpenRouter's web plugin — works with any model). Returns the answer plus a Sources list of cited URLs. Each web result costs about $0.004."
    )]
    async fn chat_with_search(
        &self,
        Parameters(p): Parameters<SearchParams>,
    ) -> Result<CallToolResult, McpError> {
        debug!(model = ?p.model, "chat_with_search tool called");
        Self::validate_temperature(p.temperature)?;

        let n = p.max_results.unwrap_or(DEFAULT_MAX_RESULTS);
        if !(1..=MAX_SEARCH_RESULTS).contains(&n) {
            return Err(McpError::invalid_params(
                format!("max_results must be between 1 and {MAX_SEARCH_RESULTS}, got {n}"),
                None,
            ));
        }

        let messages = Self::build_messages(p.system_prompt.as_deref(), None, &p.prompt)
            .map_err(|e| McpError::invalid_params(e, None))?;

        let req = Self::build_chat_request(
            p.model.as_deref().unwrap_or(&self.default_model),
            messages,
            p.temperature,
            self.resolve_max_tokens(p.max_tokens),
            None,
            None,
            Some(Self::web_plugin(n)),
        )
        .map_err(|e| McpError::invalid_params(e, None))?;

        self.do_chat(&req).await
    }

    #[tool(
        description = "List OpenRouter models with context length and per-million-token pricing (cached 5 minutes). Optional case-insensitive substring filter over model id and name — use it; the full catalog is ~344 models."
    )]
    async fn list_models(
        &self,
        Parameters(p): Parameters<ListModelsParams>,
    ) -> Result<CallToolResult, McpError> {
        let models = if let Some(cached) = self.models_cache.get(&()).await {
            debug!("list_models: returning cached data");
            cached
        } else {
            debug!("list_models: fetching from API");
            match self
                .client
                .request::<(), ModelsResponse>(Method::GET, "/models", None)
                .await
            {
                Ok(resp) => {
                    let arc = Arc::new(resp.data);
                    self.models_cache.insert((), arc.clone()).await;
                    arc
                }
                Err(e) => {
                    return Ok(CallToolResult::error(vec![Content::text(e.to_string())]));
                }
            }
        };

        let filtered = Self::filter_models(&models, p.filter.as_deref());
        let total = models.len();
        let shown = filtered.len();

        if shown == 0 {
            let msg = match &p.filter {
                Some(f) => format!("No models match filter '{f}'"),
                None => "No models available".to_string(),
            };
            return Ok(CallToolResult::success(vec![Content::text(msg)]));
        }

        let header = match &p.filter {
            Some(f) => format!("{shown} of {total} models (filter: \"{f}\")"),
            None => format!("{total} models"),
        };
        let lines: Vec<String> = filtered.iter().map(|m| Self::format_model_line(m)).collect();
        let result = format!("{header}\n{}", lines.join("\n"));
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    #[tool(
        description = "Show OpenRouter account balance: total credits purchased, total used, and remaining."
    )]
    async fn credits(&self) -> Result<CallToolResult, McpError> {
        debug!("credits tool called");
        match self
            .client
            .request::<(), CreditsResponse>(Method::GET, "/credits", None)
            .await
        {
            Ok(resp) => Ok(CallToolResult::success(vec![Content::text(resp.to_string())])),
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
        }
    }
}

// ---------------------------------------------------------------------------
// MCP ServerHandler
// ---------------------------------------------------------------------------

#[tool_handler]
impl ServerHandler for OpenRouterServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                "open-router",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "OpenRouter MCP server — chat with any model on OpenRouter through one API key. \
                 Default model: moonshotai/kimi-k3 (1M context, vision-capable). Tools: chat \
                 (multi-turn, structured output, reasoning control), chat_with_vision (image URL or \
                 local file path), chat_with_search (web-grounded answers with a Sources list), \
                 list_models (filterable catalog with pricing), credits (account balance). Every \
                 response ends with token-usage and actual USD cost footers reported by OpenRouter.",
            )
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::ChatParams;

    // -- build_messages -------------------------------------------------------

    #[test]
    fn build_messages_basic() {
        let msgs = OpenRouterServer::build_messages(None, None, "hello").unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, "user");
    }

    #[test]
    fn build_messages_system_history_prompt_order() {
        let history =
            r#"[{"role": "user", "content": "hi"}, {"role": "assistant", "content": "hey"}]"#;
        let msgs =
            OpenRouterServer::build_messages(Some("be helpful"), Some(history), "next").unwrap();
        assert_eq!(msgs.len(), 4);
        assert_eq!(msgs[0].role, "system");
        assert_eq!(msgs[1].role, "user");
        assert_eq!(msgs[2].role, "assistant");
        assert_eq!(msgs[3].role, "user");
    }

    #[test]
    fn build_messages_invalid_role() {
        let history = r#"[{"role": "hacker", "content": "hi"}]"#;
        let result = OpenRouterServer::build_messages(None, Some(history), "hello");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid role 'hacker'"));
    }

    #[test]
    fn build_messages_invalid_json() {
        let result = OpenRouterServer::build_messages(None, Some("not json"), "hello");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid messages JSON"));
    }

    // -- validate_temperature -------------------------------------------------

    #[test]
    fn validate_temperature_bounds() {
        assert!(OpenRouterServer::validate_temperature(None).is_ok());
        assert!(OpenRouterServer::validate_temperature(Some(0.0)).is_ok());
        assert!(OpenRouterServer::validate_temperature(Some(1.0)).is_ok());
        assert!(OpenRouterServer::validate_temperature(Some(2.0)).is_ok());
        assert!(OpenRouterServer::validate_temperature(Some(-0.1)).is_err());
        assert!(OpenRouterServer::validate_temperature(Some(2.1)).is_err());
        assert!(OpenRouterServer::validate_temperature(Some(f32::NAN)).is_err());
        assert!(OpenRouterServer::validate_temperature(Some(f32::INFINITY)).is_err());
    }

    // -- build_chat_request ---------------------------------------------------

    #[test]
    fn build_chat_request_always_includes_usage() {
        let msgs = vec![ChatMessage::user("hello")];
        let req =
            OpenRouterServer::build_chat_request("moonshotai/kimi-k3", msgs, None, None, None, None, None)
                .unwrap();
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v["usage"]["include"], true);
        assert_eq!(v["model"], "moonshotai/kimi-k3");
        assert!(v.get("temperature").is_none());
        assert!(v.get("max_tokens").is_none());
        assert!(v.get("response_format").is_none());
        assert!(v.get("reasoning").is_none());
        assert!(v.get("plugins").is_none());
    }

    #[test]
    fn build_chat_request_with_schema() {
        let msgs = vec![ChatMessage::user("hello")];
        let schema = r#"{"type": "object", "properties": {"name": {"type": "string"}}}"#;
        let req = OpenRouterServer::build_chat_request(
            "moonshotai/kimi-k3",
            msgs,
            None,
            None,
            Some(schema),
            None,
            None,
        )
        .unwrap();
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v["response_format"]["json_schema"]["strict"], true);
    }

    #[test]
    fn build_chat_request_invalid_schema() {
        let msgs = vec![ChatMessage::user("hello")];
        let result = OpenRouterServer::build_chat_request(
            "moonshotai/kimi-k3",
            msgs,
            None,
            None,
            Some("not json"),
            None,
            None,
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid response_schema JSON"));
    }

    // -- build_reasoning matrix -----------------------------------------------

    #[test]
    fn build_reasoning_matrix() {
        assert!(OpenRouterServer::build_reasoning(None, false).is_none());

        let hidden = OpenRouterServer::build_reasoning(Some(ReasoningEffort::Medium), false).unwrap();
        assert_eq!(hidden["effort"], "medium");
        assert_eq!(hidden["exclude"], true);

        let shown = OpenRouterServer::build_reasoning(Some(ReasoningEffort::High), true).unwrap();
        assert_eq!(shown["effort"], "high");
        assert!(shown.get("exclude").is_none());

        let enabled = OpenRouterServer::build_reasoning(None, true).unwrap();
        assert_eq!(enabled["enabled"], true);
    }

    // -- web_plugin -----------------------------------------------------------

    #[test]
    fn web_plugin_shape() {
        let plugins = OpenRouterServer::web_plugin(5);
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0]["id"], "web");
        assert_eq!(plugins[0]["max_results"], 5);
    }

    // -- mime_for_extension ---------------------------------------------------

    #[test]
    fn mime_for_extension_cases() {
        assert_eq!(
            OpenRouterServer::mime_for_extension("png"),
            Some("image/png")
        );
        assert_eq!(
            OpenRouterServer::mime_for_extension("JPG"),
            Some("image/jpeg")
        );
        assert_eq!(
            OpenRouterServer::mime_for_extension("jpeg"),
            Some("image/jpeg")
        );
        assert_eq!(
            OpenRouterServer::mime_for_extension("webp"),
            Some("image/webp")
        );
        assert_eq!(
            OpenRouterServer::mime_for_extension("gif"),
            Some("image/gif")
        );
        assert_eq!(OpenRouterServer::mime_for_extension("bmp"), None);
        assert_eq!(OpenRouterServer::mime_for_extension(""), None);
    }

    // -- resolve_image --------------------------------------------------------

    #[test]
    fn resolve_image_passthrough_and_local() {
        assert_eq!(
            OpenRouterServer::resolve_image("https://example.com/a.png").unwrap(),
            "https://example.com/a.png"
        );
        assert_eq!(
            OpenRouterServer::resolve_image("data:image/png;base64,abc").unwrap(),
            "data:image/png;base64,abc"
        );

        // Tiny local PNG.
        let path = std::env::temp_dir().join("or_test_tiny.png");
        std::fs::write(&path, b"\x89PNG\r\n\x1a\n").unwrap();
        let result = OpenRouterServer::resolve_image(path.to_str().unwrap()).unwrap();
        assert!(result.starts_with("data:image/png;base64,"));
        let _ = std::fs::remove_file(&path);

        // Missing file.
        let err = OpenRouterServer::resolve_image("/nonexistent/path/img.png").unwrap_err();
        assert!(err.contains("Failed to read image file"));
    }

    // -- format_model_line / filter_models ------------------------------------

    #[test]
    fn format_model_line_pricing_and_fallbacks() {
        let m = ModelInfo {
            id: "moonshotai/kimi-k3".into(),
            name: Some("Kimi K3".into()),
            context_length: Some(1_048_576),
            pricing: Some(crate::api::ModelPricing {
                prompt: Some("0.000003".into()),
                completion: Some("0.000015".into()),
            }),
        };
        let line = OpenRouterServer::format_model_line(&m);
        assert_eq!(
            line,
            "- moonshotai/kimi-k3 (Kimi K3) | ctx 1048576 | $3.00/M in, $15.00/M out"
        );

        let empty = ModelInfo {
            id: "x/empty".into(),
            name: None,
            context_length: None,
            pricing: None,
        };
        let line2 = OpenRouterServer::format_model_line(&empty);
        assert_eq!(line2, "- x/empty | ctx n/a | n/a in, n/a out");
    }

    #[test]
    fn filter_models_case_insensitive() {
        let models = vec![
            ModelInfo {
                id: "moonshotai/kimi-k3".into(),
                name: Some("Kimi K3".into()),
                context_length: None,
                pricing: None,
            },
            ModelInfo {
                id: "anthropic/claude-sonnet-4.5".into(),
                name: Some("Claude Sonnet 4.5".into()),
                context_length: None,
                pricing: None,
            },
        ];
        let kimi = OpenRouterServer::filter_models(&models, Some("KIMI"));
        assert_eq!(kimi.len(), 1);
        assert_eq!(kimi[0].id, "moonshotai/kimi-k3");

        let by_name = OpenRouterServer::filter_models(&models, Some("sonnet"));
        assert_eq!(by_name.len(), 1);

        let all = OpenRouterServer::filter_models(&models, None);
        assert_eq!(all.len(), 2);
    }

    // -- mockito integration --------------------------------------------------

    #[tokio::test]
    async fn chat_round_trip_formats_cost() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/chat/completions")
            .match_header("Authorization", "Bearer test-key")
            .match_body(mockito::Matcher::PartialJson(json!({
                "model": "moonshotai/kimi-k3",
                "usage": {"include": true}
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{
                    "choices": [{
                        "message": {"role": "assistant", "content": "Hello back!"},
                        "finish_reason": "stop"
                    }],
                    "usage": {
                        "prompt_tokens": 5,
                        "completion_tokens": 3,
                        "total_tokens": 8,
                        "cost": 0.000123
                    }
                }"#,
            )
            .create_async()
            .await;

        let client =
            OpenRouterClient::new("test-key".into(), server.url(), None, None);
        let srv = OpenRouterServer::new(client, None, None);
        let result = srv
            .chat(Parameters(ChatParams {
                prompt: "hi".into(),
                system_prompt: None,
                messages: None,
                model: None,
                temperature: None,
                max_tokens: None,
                reasoning_effort: None,
                show_reasoning: None,
                response_schema: None,
            }))
            .await
            .unwrap();

        mock.assert_async().await;

        let serialized = serde_json::to_value(&result).unwrap();
        let text = serialized["content"][0]["text"].as_str().unwrap_or("");
        assert!(text.contains("Hello back!"));
        assert!(text.contains("[tokens: 5 prompt + 3 completion = 8 total]"));
        assert!(text.contains("[cost: $0.000123]"));
    }

    #[tokio::test]
    async fn error_in_200_returns_tool_error() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"error":{"code":402,"message":"Insufficient credits"}}"#)
            .create_async()
            .await;

        let client =
            OpenRouterClient::new("test-key".into(), server.url(), None, None);
        let srv = OpenRouterServer::new(client, None, None);
        let result = srv
            .chat(Parameters(ChatParams {
                prompt: "hi".into(),
                system_prompt: None,
                messages: None,
                model: None,
                temperature: None,
                max_tokens: None,
                reasoning_effort: None,
                show_reasoning: None,
                response_schema: None,
            }))
            .await
            .unwrap();

        mock.assert_async().await;

        let serialized = serde_json::to_value(&result).unwrap();
        assert_eq!(serialized["isError"], true);
        let text = serialized["content"][0]["text"].as_str().unwrap_or("");
        assert!(text.contains("Insufficient credits"));
    }
}
