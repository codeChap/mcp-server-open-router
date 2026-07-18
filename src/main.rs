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
