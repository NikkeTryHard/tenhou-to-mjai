use anyhow::{Context, Result};
use serde::Deserialize;
use std::time::Duration;
use tracing::info;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Get base URL for server
pub fn server_base_url(server: &str) -> &'static str {
    match server {
        "cn" => "https://game.maj-soul.com",
        "en" | "jp" => "https://mahjongsoul.game.yo-star.com",
        _ => "https://mahjongsoul.game.yo-star.com",
    }
}

/// Get path prefix for server (CN uses /1/, EN doesn't)
fn server_path_prefix(server: &str) -> &'static str {
    match server {
        "cn" => "/1",
        _ => "",
    }
}

#[derive(Debug, Deserialize)]
pub struct VersionInfo {
    pub version: String,
}

#[derive(Debug, Deserialize)]
struct Gateway {
    url: String,
}

#[derive(Debug, Deserialize)]
struct IpEntry {
    #[serde(default)]
    gateways: Vec<Gateway>,
}

#[derive(Debug, Deserialize)]
struct Config {
    ip: Vec<IpEntry>,
}

pub async fn discover_gateway(client: &reqwest::Client, server: &str) -> Result<(String, String)> {
    let ms_host = server_base_url(server);
    let prefix = server_path_prefix(server);
    info!("Using {} server: {}", server, ms_host);
    // Step 1: Get version
    let version_url = format!("{}{}/version.json", ms_host, prefix);
    let version_info: VersionInfo = tokio::time::timeout(REQUEST_TIMEOUT, async {
        client
            .get(&version_url)
            .send()
            .await?
            .json()
            .await
            .context("Failed to parse version.json")
    })
    .await
    .context("Timeout fetching version.json")??;

    let version = &version_info.version;
    let version_clean = version.replace(".w", "");
    info!("Majsoul version: {}", version);

    // Step 2: Get config
    let config_url = format!("{}{}/v{}/config.json", ms_host, prefix, version);
    let config: Config = tokio::time::timeout(REQUEST_TIMEOUT, async {
        client
            .get(&config_url)
            .send()
            .await?
            .json()
            .await
            .context("Failed to parse config.json")
    })
    .await
    .context("Timeout fetching config.json")??;

    // New format: gateways contains direct server URLs
    let gateway = config
        .ip
        .first()
        .and_then(|ip| ip.gateways.first())
        .context("No gateway found in config")?;

    // Gateway URL is like "https://route-2.maj-soul.com"
    // Strip protocol and use directly as WebSocket server
    let server = gateway
        .url
        .replace("https://", "")
        .replace("http://", "");

    let endpoint = format!("wss://{}/gateway", server);
    info!("Discovered gateway: {}", endpoint);

    Ok((endpoint, version_clean))
}
