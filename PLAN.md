# PLAN.md — mcp-server-open-router

Build spec for a new Rust MCP server that chats with **any model on OpenRouter** through one API key.
Written 2026-07-18. This document is self-contained: an agent with no prior context can build the
server from it. The sibling projects it references live next to this folder and should be consulted
for any idiom not fully quoted here.

- **Template for structure:** `../mcp-server-fable` (main/config/api/params/server split, rmcp macros, footer, inline tests, docs style)
- **Template for the OpenAI-compatible request layer:** `../mcp-server-grok-chat/src/api.rs`
- **Template for citations/Sources rendering:** `../mcp-server-claude-chat/src/api.rs` (~lines 187–271)

---

## 1. Goal

A general-purpose chat MCP server (stdio, JSON-RPC 2.0) for OpenRouter (`https://openrouter.ai/api/v1`).
Default model **`moonshotai/kimi-k3`**. NOT planning-only like fable — this is a chat server in the
spirit of grok-chat/claude-chat, with OpenRouter's specifics (actual-cost accounting, model routing,
web plugin, unified reasoning) handled properly.

**Five tools:** `chat`, `chat_with_vision`, `chat_with_search`, `list_models`, `credits`.

Out of scope for v1 (record in the project CLAUDE.md): embeddings, streaming, fallback `models`
array, tool/function-calling passthrough, `GET /key`.

## 2. Ground truth — verified live against OpenRouter on 2026-07-18

- `GET /api/v1/models` returns `{"data":[...]}` — **344 models**. Each entry has `id`, `name`,
  `context_length`, `architecture{input_modalities,output_modalities}`,
  `pricing{prompt,completion,...}` (**strings, USD per token** — multiply by 1e6 for $/M),
  `top_provider`, `supported_parameters`.
- **`moonshotai/kimi-k3`**: context 1,048,576; pricing prompt `0.000003` ($3/M), completion
  `0.000015` ($15/M), input_cache_read `0.0000003`; input_modalities `["text","image"]`
  (**vision-capable — the default model can take images**); supported_parameters include
  `reasoning`, `reasoning_effort`, `response_format`, `structured_outputs`, `tools`, `max_tokens`
  — but **NOT `temperature`** (OpenRouter silently drops params the routed model doesn't support,
  so sending temperature is always safe).
- `POST /api/v1/chat/completions` — OpenAI-compatible. Auth `Authorization: Bearer sk-or-...`.
  Optional attribution headers `HTTP-Referer` and `X-Title`.
- **Cost accounting:** include `"usage": {"include": true}` in the request body → the response
  `usage` object gains `cost` (actual USD charged), `cost_details`,
  `prompt_tokens_details.cached_tokens`, `completion_tokens_details.reasoning_tokens`.
  This server always sends it — the cost footer shows REAL cost, no pricing constants.
- **Error-in-200 quirk:** OpenRouter can return HTTP 200 whose body is a top-level
  `{"error":{"code":...,"message":...,"metadata":...}}` (no `choices` at all), and choices can
  carry their own `error` object for provider failures. Both must be handled (see §5.6/§5.8).
- **Reasoning (unified across providers):** request field
  `"reasoning": {"effort": "low"|"medium"|"high", "max_tokens": N, "exclude": bool, "enabled": bool}`.
  The response message may carry `reasoning: Option<String>` (the reasoning text).
- **Web search:** `"plugins": [{"id": "web", "max_results": N}]` works with ANY model
  (Exa-backed, ~$4 per 1000 results, default 5). Cited pages arrive as `annotations` on the
  response message: entries of `{"type":"url_citation","url_citation":{"url":...,"title":...}}`.
- **Model routing:** the response echoes `model` (the model actually served) and `provider`
  (which upstream provider served it) — worth printing, since routing is real information.
- `GET /api/v1/credits` → `{"data":{"total_credits": f64, "total_usage": f64}}` (remaining =
  total_credits − total_usage).

## 3. Project conventions (must match the siblings)

- Folder `mcp-server-open-router` → **crate & binary name `open-router`** (fable: folder
  `mcp-server-fable` → crate `fable`). Binary lands at `target/release/open-router`.
- Rust **edition 2024**; `[profile.release] lto = true, strip = true`.
- Config at `~/.config/mcp-server-open-router/config.toml`, loaded fail-fast at startup.
- Tracing to **stderr** (stdout is reserved for JSON-RPC).
- Inline `#[cfg(test)]` unit tests in `api.rs` and `server.rs` (no `tests/` dir). Pure request
  builders are extracted as associated fns precisely so they're testable without a client.
- Own git repo (`git init` + initial commit); `config.toml` is gitignored.
- MCP errors: parameter problems → `McpError::invalid_params(msg, None)`; API failures →
  `CallToolResult::error(vec![Content::text(e.to_string())])` (a tool-level error, not a
  protocol error).

## 4. Cargo.toml

```toml
[package]
name = "open-router"
version = "0.1.0"
edition = "2024"
description = "MCP server for OpenRouter — chat with any model through one API key (default: moonshotai/kimi-k3)"

[dependencies]
rmcp = { version = "1.2", features = ["server", "transport-io"] }
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
schemars = "1"
reqwest = { version = "0.12", features = ["json"] }
anyhow = "1"
thiserror = "2"
toml = "0.8"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
dirs = "6"
moka = { version = "0.12", features = ["future"] }
base64 = "0.22"

[dev-dependencies]
mockito = "1"

[profile.release]
lto = true
strip = true
```

(`moka` = 5-minute models cache, as grok-chat uses; `base64` = local image file → data URL.
Everything else is fable's dependency set verbatim. rmcp 1.2 resolves to 1.8.0 in the siblings.)

`.gitignore` (fable's, verbatim):

```
/target
config.toml
*.key
.env
```

## 5. File-by-file specification

### 5.1 src/lib.rs

```rust
pub mod api;
pub mod config;
pub mod params;
pub mod server;
```

### 5.2 src/main.rs — copy fable's shape exactly

```rust
mod api;
mod config;
mod params;
mod server;

use anyhow::Result;
use rmcp::{ServiceExt, transport::stdio};
use tracing::info;
use tracing_subscriber::EnvFilter;

use api::OpenRouterClient;
use server::OpenRouterServer;

#[tokio::main]
async fn main() -> Result<()> {
    // Tracing writes to stderr so stdout stays clean for MCP JSON-RPC.
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    info!("loading config");
    let cfg = config::load()?;
    let client = OpenRouterClient::new(cfg.api_key, cfg.base_url, cfg.site_url, cfg.app_name);
    let server = OpenRouterServer::new(client, cfg.default_model, cfg.default_max_tokens);

    info!("starting MCP server via stdio");
    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
```

(Yes, modules are declared in both main.rs and lib.rs — that is the established sibling convention.)

### 5.3 src/config.rs — fable's pattern, adapted fields

```rust
fn default_base_url() -> String {
    "https://openrouter.ai/api/v1".to_string()
}

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    pub api_key: String,
    #[serde(default = "default_base_url")]
    pub base_url: String,
    #[serde(default)]
    pub default_model: Option<String>,
    #[serde(default)]
    pub default_max_tokens: Option<u32>,   // only sent when set — models have their own defaults
    #[serde(default)]
    pub app_name: Option<String>,          // -> X-Title header
    #[serde(default)]
    pub site_url: Option<String>,          // -> HTTP-Referer header
}
```

`config_path()` = `dirs::config_dir()` (with `$HOME/.config` fallback) + `"mcp-server-open-router"`
+ `"config.toml"` — copy fable's fn verbatim. `load()` copies fable's: `read_to_string` with a
`with_context` message that embeds a create-it example
(`Failed to read config file: {path}\nCreate it with your OpenRouter API key (https://openrouter.ai/settings/keys).\nExample:\n\napi_key = "sk-or-..."`),
`toml::from_str` with parse context, then `bail!` if `api_key.trim().is_empty()`.

### 5.4 src/api.rs — client

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort { Low, Medium, High }

#[derive(Error, Debug)]
pub enum ApiError {
    #[error("HTTP request failed: {0}")]
    Reqwest(#[from] reqwest::Error),
    #[error("OpenRouter API error ({status}): {body}")]
    Api { status: reqwest::StatusCode, body: String },
}

pub struct OpenRouterClient {
    api_key: String,
    base_url: String,
    site_url: Option<String>,
    app_name: Option<String>,
    http: Client,   // Client::builder().timeout(Duration::from_secs(600)).build().expect(...)
}
```

Timeout is **600s** (fable's, not grok's 300s — OpenRouter routes to reasoning models that can run
minutes). The generic request method is fable's verbatim, with OpenRouter headers:

```rust
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
```

### 5.5 src/api.rs — request types

```rust
#[derive(Serialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")] pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")] pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")] pub response_format: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")] pub reasoning: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")] pub plugins: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")] pub usage: Option<Value>,
}
```

`ChatRequest::new(model: &str, messages: Vec<ChatMessage>) -> Self` sets
`usage: Some(json!({"include": true}))` and every other optional to `None`. Cost accounting is the
point of this server — **every chat request opts in**.

`ChatMessage` is grok-chat's struct verbatim (`role: String`, `content: Option<Value>` with
skip_serializing_if — `Value::String` for text, JSON array for multimodal — plus
`tool_calls: Option<Vec<Value>>`, `tool_call_id: Option<String>`), with constructors:

```rust
pub fn system(text: &str) -> Self   // role "system", content Value::String
pub fn user(text: &str) -> Self     // role "user",   content Value::String
pub fn user_with_image(text: &str, image_url: &str, detail: &str) -> Self {
    // content: json!([
    //   { "type": "text", "text": text },
    //   { "type": "image_url", "image_url": { "url": image_url, "detail": detail } }
    // ])
}
```

### 5.6 src/api.rs — response types

```rust
#[derive(Debug, Deserialize)]
pub struct ChatResponse {
    #[serde(default)] pub choices: Vec<ChatChoice>,   // DEFAULT — error-only bodies have no choices!
    #[serde(default)] pub model: Option<String>,      // model actually served
    #[serde(default)] pub provider: Option<String>,   // provider that served it
    #[serde(default)] pub usage: Option<Usage>,
    #[serde(default)] pub error: Option<ApiErrorBody>, // the HTTP-200-with-error quirk
}

#[derive(Debug, Deserialize)]
pub struct ChatChoice {
    pub message: ChatResponseMessage,
    #[serde(default)] pub finish_reason: Option<String>,
    #[serde(default)] pub error: Option<ApiErrorBody>, // per-choice provider error
}

#[derive(Debug, Deserialize)]
pub struct ChatResponseMessage {
    pub role: String,
    #[serde(default)] pub content: Option<String>,
    #[serde(default)] pub reasoning: Option<String>,       // unified reasoning text
    #[serde(default)] pub tool_calls: Option<Vec<Value>>,
    #[serde(default)] pub annotations: Option<Vec<Value>>, // url_citation blocks (web plugin)
}

#[derive(Debug, Deserialize)]
pub struct ApiErrorBody {
    #[serde(default)] pub code: Option<Value>,     // number or string depending on source
    #[serde(default)] pub message: Option<String>,
    #[serde(default)] pub metadata: Option<Value>,
}
// impl ApiErrorBody::render() -> String:
//   "OpenRouter error ({code}): {message}" — code via to_string with surrounding quotes trimmed,
//   "unknown" fallbacks for both; append "\nmetadata: {pretty json}" when metadata is Some non-Null.

#[derive(Debug, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    #[serde(default)] pub cost: Option<f64>,   // ACTUAL USD (because usage.include was sent)
    #[serde(default)] pub prompt_tokens_details: Option<PromptTokensDetails>,
    #[serde(default)] pub completion_tokens_details: Option<CompletionTokensDetails>,
}
#[derive(Debug, Deserialize)] pub struct PromptTokensDetails {
    #[serde(default)] pub cached_tokens: Option<u32>,
}
#[derive(Debug, Deserialize)] pub struct CompletionTokensDetails {
    #[serde(default)] pub reasoning_tokens: Option<u32>,
}
```

Models catalog (unknown fields are skipped by serde by default; `Clone` needed for the Arc cache):

```rust
#[derive(Debug, Deserialize)] pub struct ModelsResponse { pub data: Vec<ModelInfo> }
#[derive(Debug, Clone, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    #[serde(default)] pub name: Option<String>,
    #[serde(default)] pub context_length: Option<u64>,
    #[serde(default)] pub pricing: Option<ModelPricing>,
}
#[derive(Debug, Clone, Deserialize)]
pub struct ModelPricing {
    #[serde(default)] pub prompt: Option<String>,     // USD per token, as a string
    #[serde(default)] pub completion: Option<String>,
}
```

Credits:

```rust
#[derive(Debug, Deserialize)] pub struct CreditsResponse { pub data: CreditsData }
#[derive(Debug, Deserialize)] pub struct CreditsData { pub total_credits: f64, pub total_usage: f64 }
// Display: "Credits: ${:.4} purchased, ${:.4} used, ${:.4} remaining"
```

### 5.7 src/api.rs — `impl fmt::Display for ChatResponse` (the core formatter)

Render in this exact order:

1. **Per choice** (blank-line separated when multiple, grok's `first` flag):
   - `message.reasoning` Some + non-empty → `[reasoning]\n{text}\n[/reasoning]\n`
   - role != "assistant" → `[{role}] ` prefix (grok parity)
   - `content` if Some
   - `tool_calls` if Some → `\nTool calls: {pretty json}` (defensive parity with grok)
   - choice `error` if Some → `\n[provider error: {err.render()}]`
   - `finish_reason` if Some → `\n[finish_reason: {reason}]`
2. **Sources** (claude-chat pattern): walk all choices' `annotations`; entries with
   `"type":"url_citation"` → read `annotation["url_citation"]["url"]` and `["title"]`; dedupe by
   url preserving order; if any: `\n\nSources:` then `\n- {title} — {url}` (just `\n- {url}` when
   title missing/empty).
3. `self.model` Some → `\n[model: {model}]`, with ` via {provider}` inside the bracket when
   provider is Some. Example: `[model: moonshotai/kimi-k3 via Moonshot AI]`.
4. Usage → `\n[tokens: {p} prompt + {c} completion = {t} total`
   + `; {n} cached` when `prompt_tokens_details.cached_tokens` > 0
   + `; {n} reasoning` when `completion_tokens_details.reasoning_tokens` > 0
   + `]`.
5. `usage.cost` Some → `\n[cost: ${:.6}]` (actual OpenRouter-reported USD — no `≈`, no rate
   constants; `$0.000000` for free models is correct and informative).

Top-level `resp.error` is NOT rendered by Display — `do_chat` in server.rs intercepts it first
(§5.9). Example full footer:

```
[finish_reason: stop]
[model: moonshotai/kimi-k3 via Moonshot AI]
[tokens: 812 prompt + 431 completion = 1243 total; 640 cached; 210 reasoning]
[cost: $0.008901]
```

### 5.8 src/params.rs

All structs derive `Debug, Deserialize, JsonSchema`; **every field** carries
`#[schemars(description = "...")]` (attribute form — that text becomes the MCP parameter doc).

```rust
use schemars::JsonSchema;
use serde::Deserialize;
use crate::api::ReasoningEffort;

#[derive(Debug, Clone, Copy, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ImageDetail { Low, High, #[default] Auto }
impl ImageDetail { pub fn as_str(&self) -> &'static str { /* "low" | "high" | "auto" */ } }

pub struct ChatParams {
    pub prompt: String,
    // "The user message / prompt to send"
    pub system_prompt: Option<String>,
    // "Optional system prompt to set context/behaviour"
    pub messages: Option<String>,
    // "Full conversation history as a JSON array of {role, content} objects. When provided,
    //  'prompt' is appended as the final user message."
    pub model: Option<String>,
    // "Model ID, e.g. 'moonshotai/kimi-k3', 'anthropic/claude-sonnet-4.5', 'openai/gpt-5'.
    //  Defaults to the configured default (moonshotai/kimi-k3). Call list_models to browse."
    pub temperature: Option<f32>,
    // "Sampling temperature (0.0 - 2.0). Models that don't support it (e.g. kimi-k3) silently ignore it."
    pub max_tokens: Option<u32>,
    // "Maximum tokens to generate"
    pub reasoning_effort: Option<ReasoningEffort>,
    // "Reasoning effort (low/medium/high) for reasoning-capable models. Reasoning runs but is
    //  hidden from the output unless show_reasoning=true."
    pub show_reasoning: Option<bool>,
    // "Include the model's reasoning text in the output as a [reasoning] block (default false)."
    pub response_schema: Option<String>,
    // "Optional JSON schema string to enforce structured output."
}

pub struct VisionParams {
    pub prompt: String,        // "Text prompt describing what to analyse in the image"
    pub image: String,
    // "Image to analyse: an http(s) URL, a data: URL, or a local file path
    //  (png/jpg/jpeg/webp/gif, max 20 MB — converted to a base64 data URL)."
    pub detail: Option<ImageDetail>,  // "Image detail level: low, high, or auto (default auto)"
    pub model: Option<String>,
    // "Model ID; must be vision-capable. The default moonshotai/kimi-k3 accepts images."
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
}

pub struct SearchParams {
    pub prompt: String,
    pub system_prompt: Option<String>,
    pub model: Option<String>,
    pub max_results: Option<u32>,
    // "Max web results to ground on, 1-20 (default 5). Each result costs about $0.004."
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
}

pub struct ListModelsParams {
    pub filter: Option<String>,
    // "Case-insensitive substring matched against model id and name, e.g. 'kimi' or 'anthropic'.
    //  Omit to list all (~344 models)."
}
```

`credits` takes no params struct (tool method has no `Parameters` argument, like grok's list_models).

### 5.9 src/server.rs

Imports (mirror grok-chat): `moka::future::Cache`, `reqwest::Method`,
`rmcp::{ErrorData as McpError, ServerHandler, handler::server::tool::ToolRouter, handler::server::wrapper::Parameters, model::*, tool, tool_handler, tool_router}`,
`serde_json::{Value, json}`, `std::sync::Arc`, `std::time::Duration`, `tracing::debug`.

```rust
const DEFAULT_MODEL: &str = "moonshotai/kimi-k3";
const DEFAULT_MAX_RESULTS: u32 = 5;
const MAX_SEARCH_RESULTS: u32 = 20;
const MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024;
const VALID_ROLES: &[&str] = &["system", "user", "assistant", "tool"];

#[derive(Clone)]
pub struct OpenRouterServer {
    client: Arc<OpenRouterClient>,
    default_model: String,                        // config override or DEFAULT_MODEL
    default_max_tokens: Option<u32>,              // only sent when Some
    models_cache: Cache<(), Arc<Vec<ModelInfo>>>, // capacity 1, TTL 300s — caches DATA, not text
    tool_router: ToolRouter<Self>,
}
```

**Plain helper `impl OpenRouterServer` block** (separate from the `#[tool_router]` block — fable
convention; pure helpers are associated fns so tests need no client):

- `fn validate_temperature(temp: Option<f32>) -> Result<(), McpError>` — grok's verbatim: if Some,
  must be finite and in `0.0..=2.0`, else `McpError::invalid_params`.
- `fn build_messages(system_prompt: Option<&str>, history_json: Option<&str>, prompt: &str) -> Result<Vec<ChatMessage>, String>`
  — grok's verbatim: push `ChatMessage::system` first if given; parse history JSON
  (`"Invalid messages JSON: {e}"`); validate every role against VALID_ROLES
  (`"Invalid role '{role}' in messages — must be one of: system, user, assistant, tool"`);
  append `ChatMessage::user(prompt)` last.
- `fn build_chat_request(model: &str, messages: Vec<ChatMessage>, temperature: Option<f32>, max_tokens: Option<u32>, response_schema: Option<&str>, reasoning: Option<Value>, plugins: Option<Vec<Value>>) -> Result<ChatRequest, String>`
  — start from `ChatRequest::new` (which sets `usage: {"include": true}`), set the optionals;
  `response_schema` parses the string (`"Invalid response_schema JSON: {e}"`) and wraps grok-style:
  `{"type":"json_schema","json_schema":{"name":"structured_output","strict":true,"schema":<parsed>}}`.
- `fn resolve_max_tokens(&self, requested: Option<u32>) -> Option<u32>` = `requested.or(self.default_max_tokens)`.
- `fn build_reasoning(effort: Option<ReasoningEffort>, show_reasoning: bool) -> Option<Value>` —
  the 4-way matrix:
  | effort | show_reasoning | result |
  |---|---|---|
  | None | false | `None` (field omitted) |
  | Some(e) | false | `{"effort": e, "exclude": true}` (reasoning runs, hidden) |
  | Some(e) | true | `{"effort": e}` |
  | None | true | `{"enabled": true}` |
- `fn web_plugin(max_results: u32) -> Vec<Value>` = `vec![json!({"id": "web", "max_results": max_results})]`.
- `fn mime_for_extension(ext: &str) -> Option<&'static str>` — lowercase match png/jpg/jpeg/webp/gif
  → `image/png` / `image/jpeg` / `image/jpeg` / `image/webp` / `image/gif`; else None.
- `fn resolve_image(input: &str) -> Result<String, String>` — starts with `http://`, `https://`,
  or `data:` → pass through unchanged. Otherwise treat as local path: extension →
  `mime_for_extension` (Err listing supported extensions on None); `std::fs::read`
  (Err `"Failed to read image file '{path}': {e}"`); size ≤ MAX_IMAGE_BYTES (Err with actual
  size); return `format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(&bytes))`.
- `async fn do_chat(&self, req: &ChatRequest) -> Result<CallToolResult, McpError>` — POST
  `/chat/completions`; `Err(e)` → `CallToolResult::error(vec![Content::text(e.to_string())])`;
  `Ok(resp)`: **if `resp.error.is_some()` → `CallToolResult::error(vec![Content::text(err.render())])`**
  (the error-in-200 quirk), else `CallToolResult::success(vec![Content::text(resp.to_string())])`.
- `fn filter_models<'a>(models: &'a [ModelInfo], filter: Option<&str>) -> Vec<&'a ModelInfo>` —
  case-insensitive substring over `id` and `name`.
- `fn format_model_line(m: &ModelInfo) -> String` —
  `- {id} ({name}) | ctx {n} | ${p:.2}/M in, ${c:.2}/M out`; price strings via
  `s.parse::<f64>().ok().map(|v| v * 1_000_000.0)`; missing name → omit parens; missing ctx/price
  → `ctx n/a` / `n/a`. (kimi-k3 must render `$3.00/M in, $15.00/M out`.)

**`#[tool_router] impl OpenRouterServer` block:**

- `pub fn new(client: OpenRouterClient, default_model: Option<String>, default_max_tokens: Option<u32>) -> Self`
  — moka cache `Cache::builder().max_capacity(1).time_to_live(Duration::from_secs(300)).build()`,
  `default_model.unwrap_or_else(|| DEFAULT_MODEL.to_string())`, `tool_router: Self::tool_router()`.
- `chat` — `#[tool(description = "Chat with any OpenRouter model (default: moonshotai/kimi-k3). Supports multi-turn history, a system prompt, structured output via JSON schema, and reasoning control. Every response ends with a token-usage and actual USD cost footer.")]`
  `async fn chat(&self, Parameters(p): Parameters<ChatParams>) -> Result<CallToolResult, McpError>`:
  `debug!`; `Self::validate_temperature(p.temperature)?`;
  `build_messages(p.system_prompt.as_deref(), p.messages.as_deref(), &p.prompt)` (Err →
  invalid_params); `build_reasoning(p.reasoning_effort, p.show_reasoning.unwrap_or(false))`;
  `build_chat_request(p.model.as_deref().unwrap_or(&self.default_model), messages, p.temperature,
  self.resolve_max_tokens(p.max_tokens), p.response_schema.as_deref(), reasoning, None)` (Err →
  invalid_params); `self.do_chat(&req).await`.
- `chat_with_vision` — `#[tool(description = "Analyse an image with a vision-capable OpenRouter model (the default moonshotai/kimi-k3 accepts images). Provide the image as an http(s) URL, a data: URL, or a local file path.")]`
  validate temperature; `Self::resolve_image(&p.image)` (Err → invalid_params);
  `vec![ChatMessage::user_with_image(&p.prompt, &url, p.detail.unwrap_or_default().as_str())]`;
  build request (no schema/reasoning/plugins); `do_chat`.
- `chat_with_search` — `#[tool(description = "Chat with live web-search grounding (OpenRouter's web plugin — works with any model). Returns the answer plus a Sources list of cited URLs. Each web result costs about $0.004.")]`
  validate temperature; `max_results` default DEFAULT_MAX_RESULTS, must be `1..=MAX_SEARCH_RESULTS`
  else invalid_params; `build_messages(p.system_prompt.as_deref(), None, &p.prompt)`; build request
  with `plugins: Some(Self::web_plugin(n))`; `do_chat`. (Sources render via the shared Display.)
- `list_models` — `#[tool(description = "List OpenRouter models with context length and per-million-token pricing (cached 5 minutes). Optional case-insensitive substring filter over model id and name — use it; the full catalog is ~344 models.")]`
  takes `Parameters<ListModelsParams>`. Cache hit → use it; miss → GET `/models` as
  `ModelsResponse` (API error → `CallToolResult::error`), `insert((), Arc::new(resp.data)).await`.
  Apply `filter_models`; empty → success `"No models match filter '{f}'"`; else header
  `"{shown} of {total} models (filter: \"{f}\")"` (or `"{total} models"` unfiltered) + one
  `format_model_line` per model.
- `credits` — `#[tool(description = "Show OpenRouter account balance: total credits purchased, total used, and remaining.")]`
  no params; GET `/credits` as `CreditsResponse`; Ok → success(to_string), Err →
  `CallToolResult::error`.

**`#[tool_handler] impl ServerHandler`:**

```rust
fn get_info(&self) -> ServerInfo {
    ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
        .with_server_info(Implementation::new("open-router", env!("CARGO_PKG_VERSION")))
        .with_instructions(
            "OpenRouter MCP server — chat with any model on OpenRouter through one API key. \
             Default model: moonshotai/kimi-k3 (1M context, vision-capable). Tools: chat \
             (multi-turn, structured output, reasoning control), chat_with_vision (image URL or \
             local file path), chat_with_search (web-grounded answers with a Sources list), \
             list_models (filterable catalog with pricing), credits (account balance). Every \
             response ends with token-usage and actual USD cost footers reported by OpenRouter.",
        )
}
```

## 6. Unit tests (inline `#[cfg(test)]`, ~24 asserts across ~2 modules)

**api.rs tests** (small builder helpers like fable's `resp(...)`/`usage(...)`):

1. `display_basic` — one assistant choice "Hello!", finish stop, usage(10,5,15) → contains
   `Hello!`, `[finish_reason: stop]`, `[tokens: 10 prompt + 5 completion = 15 total]`.
2. `display_cost_and_token_details` — cost 0.00123, cached 640, reasoning 210 → contains
   `[cost: $0.001230]`, `; 640 cached`, `; 210 reasoning`.
3. `display_model_line` — model+provider → `[model: moonshotai/kimi-k3 via Moonshot AI]`.
4. `display_reasoning_block` — reasoning Some("thought") renders `[reasoning]` before content;
   empty string is skipped.
5. `display_annotations_sources` — two url_citations with duplicate url → one `Sources:` section,
   deduped, `{title} — {url}` format.
6. `display_choice_error` — choice error {502, "provider down"} → `[provider error:` + message.
7. `deserialize_error_only_body` —
   `from_str::<ChatResponse>(r#"{"error":{"code":402,"message":"Insufficient credits"}}"#)`
   succeeds with empty choices and Some(error) (guards `#[serde(default)]`).
8. `display_credits` — {25.0, 3.2154} → `Credits: $25.0000 purchased, $3.2154 used, $21.7846 remaining`.
9. `display_empty_choices` — fully-empty response Displays as `""`.

**server.rs tests:**

10. `build_messages_*` — basic; system+2-history+prompt order; role "hacker" → Err
    `Invalid role 'hacker'`; bad JSON → Err `Invalid messages JSON`.
11. `validate_temperature_bounds` — Ok: None/0.0/1.0/2.0; Err: -0.1, 2.1, NAN, INFINITY.
12. `build_chat_request_always_includes_usage` — serialize to Value: `v["usage"]["include"]==true`,
    model correct, and absent optionals truly absent (`v.get("temperature").is_none()` etc.).
13. `build_chat_request_with_schema` / `_invalid_schema` —
    `v["response_format"]["json_schema"]["strict"]==true`; "not json" → Err.
14. `build_reasoning_matrix` — the 4 combos, exact JSON asserted.
15. `web_plugin_shape` — `[{"id":"web","max_results":5}]`.
16. `mime_for_extension_cases` — png/JPG/jpeg/webp/gif → Some; "bmp"/"" → None.
17. `resolve_image_passthrough_and_local` — https/data: unchanged; tiny file written to
    `std::env::temp_dir()` → starts with `data:image/png;base64,`; missing file → Err.
18. `format_model_line_pricing_and_fallbacks` — "0.000003"/"0.000015"/ctx 1048576/"Kimi K3" →
    `- moonshotai/kimi-k3 (Kimi K3) | ctx 1048576 | $3.00/M in, $15.00/M out`; empty model → `n/a`s.
19. `filter_models_case_insensitive` — "KIMI" matches id; name substring matches; None → all.
20. (mockito, `#[tokio::test]`) `chat_round_trip_formats_cost` — mock POST /chat/completions
    matching header `Authorization: Bearer test-key` and body `PartialJson({"model":"moonshotai/kimi-k3","usage":{"include":true}})`,
    returning 200 with one choice ("Hello back!", stop) and
    `usage{5,3,8,"cost":0.000123}`. Build `OpenRouterClient::new("test-key", server.url(), None, None)`
    → `OpenRouterServer::new(client, None, None)` → call `chat(Parameters(ChatParams{prompt:"hi", ..}))`;
    assert mock hit; serialized result contains `Hello back!`,
    `[tokens: 5 prompt + 3 completion = 8 total]`, `[cost: $0.000123]`.
21. (mockito) `error_in_200_returns_tool_error` — 200 body
    `{"error":{"code":402,"message":"Insufficient credits"}}` → result serializes with
    `"isError":true` and contains `Insufficient credits`.

## 7. Docs & meta files

### config.toml.example

```toml
# Copy to ~/.config/mcp-server-open-router/config.toml and fill in your key.

# Required: your OpenRouter API key (https://openrouter.ai/settings/keys)
api_key = "sk-or-..."

# Optional: override the API base URL.
# base_url = "https://openrouter.ai/api/v1"

# Optional: default model used when a tool call omits `model`.
# default_model = "moonshotai/kimi-k3"

# Optional: default max output tokens; only sent when set (models otherwise use their own defaults).
# default_max_tokens = 8192

# Optional: app attribution sent to OpenRouter on every request
# (shows on your OpenRouter activity page / leaderboards).
# app_name = "mcp-server-open-router"   # sent as X-Title
# site_url = "https://example.com"      # sent as HTTP-Referer
```

### README.md — follow ../mcp-server-fable/README.md's structure

Title; badge row; intro (OpenRouter = one key → 300+ models from every lab; default
moonshotai/kimi-k3 — 1M context, $3/$15 per M, vision-capable; stdio JSON-RPC 2.0; "structurally
mirrors mcp-server-fable with the OpenAI-compatible layer of mcp-server-grok-chat"); an
"OpenRouter specifics" section (actual-cost accounting via `usage:{"include":true}`; error-in-200
handling; unified reasoning + `[reasoning]` block; web plugin + Sources; unsupported params
silently dropped; attribution headers); Tools summary table (5 rows) + per-tool parameter tables
(Name/Type/Required/Description from §5.8); Prerequisites (Rust edition 2024; OpenRouter key with
credits); config block; Build (`cargo build --release` → `target/release/open-router`,
`cargo test`); Installation & MCP Configuration:

```bash
claude mcp add open-router -- /media/codechap/4TB/develop/mcps/mcp-server-open-router/target/release/open-router
```

```json
{
  "mcpServers": {
    "open-router": {
      "command": "/media/codechap/4TB/develop/mcps/mcp-server-open-router/target/release/open-router"
    }
  }
}
```

Usage section with a natural-language example, a raw JSON-RPC tools/call example, and the sample
footer block from §5.7; Project Structure tree; MIT license.

### CLAUDE.md (per-project) — follow ../mcp-server-fable/CLAUDE.md's headings

What This Is (general-purpose OpenRouter chat, NOT planning-only like fable; 5 tools) / Build & Run
/ Configuration / **OpenRouter specifics** (the analogue of fable's "How Fable differs"): (a) every
chat request sends `usage:{"include":true}` → `usage.cost` is actual USD, footer never estimates;
(b) HTTP 200 can carry a top-level `{"error":...}` — `choices` is `#[serde(default)]`, `do_chat`
intercepts top-level errors, per-choice errors render inline; (c) unified `reasoning` field +
build_reasoning matrix; (d) web search via `plugins:[{"id":"web"}]`, annotations → Sources; (e)
OpenRouter silently drops unsupported params (temperature on kimi-k3) — always safe to send; (f)
attribution headers from config; (g) 600s timeout (reasoning models); (h) out of scope: streaming,
embeddings, fallback `models` array, tool-calling passthrough / Architecture (5 files, naming every
helper) / Key Constants / Adding a New Tool (fable's 3 steps) / Dependencies.

### CHANGELOG.md

```markdown
# Changelog

## 0.1.0 - 2026-07-18

Initial release: chat, chat_with_vision, chat_with_search, list_models, credits
against the OpenRouter API. Actual-cost usage accounting, error-in-200 handling,
unified reasoning support, web-plugin search with Sources list.
```

## 8. Repo-level ../CLAUDE.md edits (5 precise edits; NOT a git repo at that level)

1. `**Rust servers:**` line: add `open-router` (alphabetical, between `mysql` and `post-x`).
2. Binary table: insert row `| open-router | \`open-router\` |` between mysql and post-x.
3. Architecture Patterns: new paragraph after the **fable** paragraph — OpenRouter wrapper, one key
   any model, default kimi-k3 (1M ctx, vision), the 5 tools, grok-chat request shape,
   `usage:{"include":true}` → actual-USD footer, error-in-200 handling (defaulted `choices`,
   top-level error → tool error, per-choice inline), unified reasoning, web plugin → Sources,
   attribution headers, 600s timeout, inline unit tests.
4. Key Configuration list: `- **open-router:** ~/.config/mcp-server-open-router/config.toml
   (api_key; optional base_url, default_model, default_max_tokens, app_name, site_url)` (alphabetical).
5. Testing section: extend to note fable and open-router have inline `cargo test` suites.

## 9. Build order & verification

1. In `/media/codechap/4TB/develop/mcps/mcp-server-open-router`: `git init`; write `.gitignore`,
   `Cargo.toml`, `config.toml.example`, `CHANGELOG.md`.
2. Write `src/config.rs` → `src/api.rs` → `src/params.rs` → `src/server.rs` → `src/lib.rs` →
   `src/main.rs`.
3. `cargo build` (expect first-pass fixes around rmcp macro imports / serde attrs) → `cargo test`
   (all green) → `cargo build --release` → verify `target/release/open-router` exists.
4. Write README.md and project CLAUDE.md; apply the 5 repo-level CLAUDE.md edits.
5. Config: `mkdir -p ~/.config/mcp-server-open-router && cp config.toml.example
   ~/.config/mcp-server-open-router/config.toml`, then **ask the user for their OpenRouter API key**
   (https://openrouter.ai/settings/keys) — never invent or placeholder-commit one. Running the
   binary without a valid config exiting with the create-it message is itself a valid negative test.
6. Stdio smoke test (no MCP client needed):

   ```bash
   printf '%s\n' \
     '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"smoke","version":"0"}}}' \
     '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
     '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' \
     | ./target/release/open-router
   ```

   Expect the initialize result (with instructions string) and tools/list naming all 5 tools.
   With a real key: append `{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"credits","arguments":{}}}`
   (zero model spend) → `Credits: $...`; then a near-free live chat
   `{"name":"chat","arguments":{"prompt":"Reply with exactly OK","max_tokens":10}}` → verify the
   `[cost: $...]` footer carries an actual amount and `[model: ...]` shows kimi-k3.
7. Register: `claude mcp add open-router -- /media/codechap/4TB/develop/mcps/mcp-server-open-router/target/release/open-router`
   (restart the Claude Code session to pick it up).
8. Initial commit (project repo only): `git add -A && git commit -m "Initial implementation:
   OpenRouter chat MCP server"` with the session's required commit trailers. `config.toml` never
   enters the repo.

## 10. Acceptance criteria

- `cargo test` passes; `cargo build --release` produces `target/release/open-router`.
- `tools/list` shows exactly: chat, chat_with_vision, chat_with_search, list_models, credits — every
  parameter carries a description; `reasoning_effort` and `detail` appear as real JSON-Schema enums.
- A live default `chat` is served by moonshotai/kimi-k3 and the output ends with
  `[finish_reason: ...]`, `[model: ...]`, `[tokens: ...]`, `[cost: $...]` where cost is
  OpenRouter-reported, not computed.
- A mocked HTTP-200 `{"error":...}` body surfaces as a tool error (isError) — never a panic or an
  empty success.
- `list_models` with `filter: "kimi"` returns a short list with ctx + $/M pricing; a repeat call
  within 5 minutes does not re-hit `/models`.
- `chat_with_vision` works with both an https image URL and a local `.png` path.
- `chat_with_search` output ends with a `Sources:` list of cited URLs.
