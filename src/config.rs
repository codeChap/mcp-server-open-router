use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::path::PathBuf;

/// Default OpenRouter API base URL. Overridable via the `base_url` config field.
fn default_base_url() -> String {
    "https://openrouter.ai/api/v1".to_string()
}

/// Configuration loaded from the TOML config file.
#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    pub api_key: String,
    #[serde(default = "default_base_url")]
    pub base_url: String,
    #[serde(default)]
    pub default_model: Option<String>,
    /// Only sent when set — models have their own defaults otherwise.
    #[serde(default)]
    pub default_max_tokens: Option<u32>,
    /// Sent as the `X-Title` header for OpenRouter attribution.
    #[serde(default)]
    pub app_name: Option<String>,
    /// Sent as the `HTTP-Referer` header for OpenRouter attribution.
    #[serde(default)]
    pub site_url: Option<String>,
}

/// Returns the path to the config file, using `dirs::config_dir()` for cross-platform support.
pub fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
            PathBuf::from(home).join(".config")
        })
        .join("mcp-server-open-router")
        .join("config.toml")
}

/// Load and validate the config file.
pub fn load() -> Result<Config> {
    let path = config_path();
    let content = std::fs::read_to_string(&path).with_context(|| {
        format!(
            "Failed to read config file: {}\n\
             Create it with your OpenRouter API key (https://openrouter.ai/settings/keys).\n\
             Example:\n\n\
             api_key = \"sk-or-...\"",
            path.display()
        )
    })?;
    let config: Config =
        toml::from_str(&content).with_context(|| format!("Failed to parse {}", path.display()))?;

    if config.api_key.trim().is_empty() {
        bail!(
            "api_key in {} is empty — set it to your OpenRouter API key",
            path.display()
        );
    }

    Ok(config)
}
