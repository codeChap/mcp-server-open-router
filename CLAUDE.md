# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What This Is

An MCP (Model Context Protocol) server that wraps **OpenRouter** (`https://openrouter.ai/api/v1`) — a general-purpose chat server, **not** planning-only like fable. One API key reaches 300+ models; default is `moonshotai/kimi-k3` (1M context, vision-capable). Five tools: `chat`, `chat_with_vision`, `chat_with_search`, `list_models`, `credits`.

Out of scope for v1: embeddings, streaming, fallback `models` array, tool/function-calling passthrough, `GET /key`.

## Build & Run

```bash
cargo build --release    # binary: target/release/open-router
cargo build              # debug build
cargo run                # dev mode
RUST_LOG=debug cargo run # debug logging (to stderr; stdout is reserved for JSON-RPC)
cargo test               # unit tests
```

## Configuration

Config file: `~/.config/mcp-server-open-router/config.toml`

```toml
api_key = "sk-or-..."        # required
# base_url = "https://openrouter.ai/api/v1"
# default_model = "moonshotai/kimi-k3"
# default_max_tokens = 8192
# app_name = "mcp-server-open-router"   # -> X-Title
# site_url = "https://example.com"      # -> HTTP-Referer
```

Loaded at startup in `src/config.rs`; the server fails immediately if the file is missing or `api_key` is empty.

## OpenRouter specifics

These are the deliberate behaviours that make this more than a generic OpenAI-compatible wrapper:

1. **Actual-cost accounting.** Every chat request sends `usage:{"include":true}`. Response `usage.cost` is real USD; the footer never estimates from rate constants.
2. **HTTP 200 with error body.** OpenRouter can return `{"error":{...}}` with no `choices`. `ChatResponse.choices` is `#[serde(default)]`; `do_chat` intercepts top-level errors as tool errors; per-choice errors render inline as `[provider error: …]`.
3. **Unified reasoning.** Request field `reasoning` built by `build_reasoning` (effort + show_reasoning matrix). Response may include `message.reasoning` text, rendered as a `[reasoning]` block when present and non-empty.
4. **Web search.** `plugins:[{"id":"web","max_results":N}]` works with any model. Annotations with `type:url_citation` become a deduped `Sources:` list.
5. **Unsupported params are dropped.** OpenRouter silently ignores params the routed model doesn't support (e.g. temperature on kimi-k3) — always safe to send.
6. **Attribution headers.** Optional `HTTP-Referer` / `X-Title` from config.
7. **600s HTTP timeout** — reasoning models can run minutes (same as fable, not grok's 300s).
8. **Out of scope:** streaming, embeddings, fallback `models` array, tool-calling passthrough, `GET /key`.

## Architecture

Five source files, no sub-crates:

- **`main.rs`** — entry point. Loads config, builds `OpenRouterClient`, builds `OpenRouterServer`, starts rmcp stdio transport.
- **`config.rs`** — TOML config from `~/.config/mcp-server-open-router/config.toml`. `api_key` (required), `base_url` (defaulted), optional `default_model` / `default_max_tokens` / `app_name` / `site_url`.
- **`api.rs`** — `ReasoningEffort` enum, `OpenRouterClient` (reqwest, generic `request<Req,Resp>()`, attribution headers), request/response types (`ChatRequest` always sets `usage.include`, `ChatMessage`, `ChatResponse` with defaulted `choices` + top-level `error`, `Usage` with `cost`, `ModelsResponse`/`ModelInfo`, `CreditsResponse`), and `Display` for `ChatResponse` (reasoning block, content, Sources, model/provider, tokens, cost) and `CreditsResponse`.
- **`server.rs`** — MCP tools via rmcp `#[tool]`/`#[tool_router]`/`#[tool_handler]`. Tools: `chat`, `chat_with_vision`, `chat_with_search`, `list_models`, `credits`. Helpers: `validate_temperature`, `build_messages`, `build_chat_request`, `resolve_max_tokens`, `build_reasoning`, `web_plugin`, `mime_for_extension`, `resolve_image`, `do_chat`, `filter_models`, `format_model_line`. Models cache is moka TTL 300s storing `Arc<Vec<ModelInfo>>` (data, not formatted text).
- **`params.rs`** — serde + `JsonSchema` parameter structs; `#[schemars(description)]` becomes the MCP tool parameter docs. `ImageDetail` and `ReasoningEffort` are real JSON-Schema enums.

## Key Constants (`server.rs` / `api.rs`)

- Default model: `moonshotai/kimi-k3` (`server.rs`)
- Default max search results: `5` (`server.rs`)
- Max search results: `20` (`server.rs`)
- Max image bytes: `20 * 1024 * 1024` (`server.rs`)
- Valid message roles: `system`, `user`, `assistant`, `tool` (`server.rs`)
- Models cache TTL: 300 seconds (`server.rs`)
- API base URL: `https://openrouter.ai/api/v1` (default in `config.rs`)
- HTTP timeout: 600 seconds (`api.rs`)

## Adding a New Tool

1. Add a parameter struct to `params.rs` (`Deserialize` + `JsonSchema`).
2. Add a `#[tool(description = "...")]` method inside the `#[tool_router] impl OpenRouterServer` block in `server.rs`; assemble the request via the shared helpers (`build_chat_request` / `do_chat` where applicable).
3. Add any new API types to `api.rs` if calling a new endpoint.

## Dependencies

`rmcp` v1.2 for MCP protocol handling, `reqwest` for HTTP, `moka` for the models cache, `base64` for local image → data URL. Rust edition 2024.
