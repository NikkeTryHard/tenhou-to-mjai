use anyhow::{Context, Result};
use serde::Deserialize;
use tracing::info;

const MS_HOST: &str = "https://game.maj-soul.com";

#[derive(Debug, Deserialize)]
pub struct VersionInfo {
    pub version: String,
}

#[derive(Debug, Deserialize)]
struct RegionUrl {
    url: String,
}

#[derive(Debug, Deserialize)]
struct IpEntry {
    region_urls: Vec<RegionUrl>,
}

#[derive(Debug, Deserialize)]
struct Config {
    ip: Vec<IpEntry>,
}

#[derive(Debug, Deserialize)]
struct ServerList {
    servers: Vec<String>,
}

pub async fn discover_gateway(client: &reqwest::Client) -> Result<(String, String)> {
    // Step 1: Get version
    let version_url = format!("{}/1/version.json", MS_HOST);
    let version_info: VersionInfo = client
        .get(&version_url)
        .send()
        .await?
        .json()
        .await
        .context("Failed to parse version.json")?;

    let version = &version_info.version;
    let version_clean = version.replace(".w", "");
    info!("Majsoul version: {}", version);

    // Step 2: Get config
    let config_url = format!("{}/1/v{}/config.json", MS_HOST, version);
    let config: Config = client
        .get(&config_url)
        .send()
        .await?
        .json()
        .await
        .context("Failed to parse config.json")?;

    let region_url = config
        .ip
        .first()
        .and_then(|ip| ip.region_urls.get(1).or(ip.region_urls.first()))
        .map(|r| &r.url)
        .context("No region URL found in config")?;

    // Step 3: Get server list
    let servers_url = format!("{}?service=ws-gateway&protocol=ws&ssl=true", region_url);
    let server_list: ServerList = client
        .get(&servers_url)
        .send()
        .await?
        .json()
        .await
        .context("Failed to parse server list")?;

    let server = server_list
        .servers
        .first()
        .context("No servers available")?;

    let endpoint = format!("wss://{}/gateway", server);
    info!("Discovered gateway: {}", endpoint);

    Ok((endpoint, version_clean))
}
