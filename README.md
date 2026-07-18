# mcp-server-open-router

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](#license)
[![Rust edition 2024](https://img.shields.io/badge/Rust-edition%202024-dea584.svg?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![MCP](https://img.shields.io/badge/MCP-server-6f42c1.svg)](https://modelcontextprotocol.io/)
[![OpenRouter](https://img.shields.io/badge/OpenRouter-API-blueviolet.svg)](https://openrouter.ai/)

An MCP (Model Context Protocol) server for **OpenRouter** — one API key, 300+ models from every major lab. Built in Rust, it exposes chat, vision, web-search, model listing, and credit-balance tools over stdio so **any** MCP client can use them.

Default model: **`moonshotai/kimi-k3`** — 1M context, $3/$15 per M input/output, vision-capable. Communicates via stdio using JSON-RPC 2.0. Structurally it mirrors `mcp-server-fable` with the OpenAI-compatible request layer of `mcp-server-grok-chat`.

## OpenRouter specifics

- **Actual-cost accounting** — every chat request sends `usage:{"include":true}`; the response footer shows OpenRouter's reported USD (`[cost: $0.008901]`), never an estimate from pricing constants.
- **Error-in-200 handling** — OpenRouter can return HTTP 200 with a top-level `{"error":...}` body (no `choices`). The server treats that as a tool error, not a panic or empty success. Per-choice provider errors render inline.
- **Unified reasoning** — request field `reasoning` with effort / exclude / enabled; optional `[reasoning]…[/reasoning]` block when `show_reasoning=true`.
- **Web plugin + Sources** — `chat_with_search` uses `plugins:[{"id":"web"}]`; cited pages become a `Sources:` list from `url_citation` annotations.
- **Unsupported params are silently dropped** — e.g. kimi-k3 does not support `temperature`; sending it is always safe.
- **Attribution headers** — optional `app_name` → `X-Title`, `site_url` → `HTTP-Referer`.

## Tools

| Tool | Description |
|------|-------------|
| `chat` | Chat with any OpenRouter model. Multi-turn history, system prompt, structured output via JSON schema, reasoning control. Token + actual USD cost footer. |
| `chat_with_vision` | Analyse an image (http(s) URL, data URL, or local file path). Default model accepts images. |
| `chat_with_search` | Web-grounded chat via OpenRouter's web plugin. Returns answer + `Sources:` list. |
| `list_models` | Filterable catalog with context length and $/M pricing (cached 5 minutes). |
| `credits` | Account balance: purchased, used, remaining. |

### chat

| Name | Type | Required | Description |
|------|------|----------|-------------|
| `prompt` | string | yes | The user message / prompt to send |
| `system_prompt` | string | no | Optional system prompt to set context/behaviour |
| `messages` | string | no | Full conversation history as a JSON array of `{role, content}` objects. When provided, `prompt` is appended as the final user message. |
| `model` | string | no | Model ID (default: configured default / `moonshotai/kimi-k3`). Call `list_models` to browse. |
| `temperature` | number | no | Sampling temperature (0.0–2.0). Models that don't support it silently ignore it. |
| `max_tokens` | integer | no | Maximum tokens to generate |
| `reasoning_effort` | string | no | `low` / `medium` / `high` for reasoning-capable models. Hidden unless `show_reasoning=true`. |
| `show_reasoning` | boolean | no | Include reasoning text as a `[reasoning]` block (default false) |
| `response_schema` | string | no | Optional JSON schema string to enforce structured output |

### chat_with_vision

| Name | Type | Required | Description |
|------|------|----------|-------------|
| `prompt` | string | yes | Text prompt describing what to analyse in the image |
| `image` | string | yes | http(s) URL, data URL, or local file path (png/jpg/jpeg/webp/gif, max 20 MB) |
| `detail` | string | no | `low` / `high` / `auto` (default `auto`) |
| `model` | string | no | Must be vision-capable; default kimi-k3 accepts images |
| `temperature` | number | no | Sampling temperature (0.0–2.0) |
| `max_tokens` | integer | no | Maximum tokens to generate |

### chat_with_search

| Name | Type | Required | Description |
|------|------|----------|-------------|
| `prompt` | string | yes | The user message / prompt |
| `system_prompt` | string | no | Optional system prompt |
| `model` | string | no | Model ID (default: configured default) |
| `max_results` | integer | no | Max web results, 1–20 (default 5). ~$0.004 each |
| `temperature` | number | no | Sampling temperature (0.0–2.0) |
| `max_tokens` | integer | no | Maximum tokens to generate |

### list_models

| Name | Type | Required | Description |
|------|------|----------|-------------|
| `filter` | string | no | Case-insensitive substring over model id and name. Omit to list all (~344 models). |

### credits

No parameters.

## Prerequisites

- Rust (edition 2024)
- An OpenRouter API key from [openrouter.ai/settings/keys](https://openrouter.ai/settings/keys) with credits

The server expects a config file at `~/.config/mcp-server-open-router/config.toml` containing at minimum your `api_key`. See `config.toml.example`.

```toml
api_key = "sk-or-..."

# Optional overrides:
# base_url = "https://openrouter.ai/api/v1"
# default_model = "moonshotai/kimi-k3"
# default_max_tokens = 8192
# app_name = "mcp-server-open-router"
# site_url = "https://example.com"
```

The server fails fast at startup if the config is missing or `api_key` is empty.

## Build

```bash
cargo build --release   # produces target/release/open-router
cargo build             # debug build
cargo run               # run in dev mode
RUST_LOG=debug cargo run
cargo test              # unit tests (Display formatter, builders, mockito round-trips)
```

## Installation & MCP Configuration

### 1. Build the server

```bash
cargo build --release
# The binary will be at: target/release/open-router
```

Use the **full absolute path** to `target/release/open-router` in all configuration below.

### 2. AI-assisted installation (recommended modern method)

Copy the block below and paste it directly to your AI coding assistant (Claude Code, Cursor, Grok, etc.). The AI will handle cloning (if needed), building, path resolution, and registration for you.

```
Add the mcp-server-open-router MCP server for me.

Repository: https://github.com/<your-username>/mcp-server-open-router   (update this URL if you have a fork)

Steps to perform:
1. If the repo isn't cloned locally yet, clone it and cd into it.
2. Build the release binary:
     cargo build --release
3. Determine the absolute path to the built binary (target/release/open-router).
4. Set up the config directory and file:
     mkdir -p ~/.config/mcp-server-open-router
     cp config.toml.example ~/.config/mcp-server-open-router/config.toml
   Then edit the config and add your OpenRouter API key (api_key = "sk-or-...").

5. Register it as an MCP server named "open-router".

   For Claude Code, run:
     claude mcp add open-router -- <ABSOLUTE_PATH_TO>/target/release/open-router

   For Claude Desktop or other MCP clients, add this under the "mcpServers" key (use the real absolute path):
{
  "open-router": {
    "command": "<ABSOLUTE_PATH_TO>/target/release/open-router"
  }
}

After setup, test that the `chat` and `credits` tools are available and working.
```

### 3. Manual configuration

Claude Desktop or any MCP client (`~/.config/Claude/claude_desktop_config.json` or equivalent):

```json
{
  "mcpServers": {
    "open-router": {
      "command": "/media/codechap/4TB/develop/mcps/mcp-server-open-router/target/release/open-router"
    }
  }
}
```

Claude Code (one-liner):

```bash
claude mcp add open-router -- /media/codechap/4TB/develop/mcps/mcp-server-open-router/target/release/open-router
```

Replace the path with your actual absolute path to the release binary.

## Usage

Once registered, an MCP client calls the tools by name.

### From an MCP client (e.g. Claude Code)

> Use the open-router **chat** tool. prompt: "Reply with exactly OK". max_tokens: 10

### Raw JSON-RPC over stdio

```json
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{
  "name":"chat",
  "arguments":{
    "prompt":"Reply with exactly OK",
    "max_tokens":10
  }}}
```

Every successful chat response ends with token-usage and actual-cost footers:

```
OK
[finish_reason: stop]
[model: moonshotai/kimi-k3 via Moonshot AI]
[tokens: 812 prompt + 431 completion = 1243 total; 640 cached; 210 reasoning]
[cost: $0.008901]
```

The cost line is OpenRouter-reported USD (because every request opts into `usage.include`), not an estimate from pricing constants.

## Project Structure

```
src/
  main.rs    - entry point, config loading, stdio transport setup
  server.rs  - MCP tools (chat, chat_with_vision, chat_with_search, list_models, credits) + helpers
  api.rs     - OpenRouter HTTP client, request/response types, Display formatter
  params.rs  - tool parameter types with serde + JSON Schema derives
  config.rs  - TOML config loading
```

## License

MIT
