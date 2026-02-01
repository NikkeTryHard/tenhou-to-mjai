//! Browser automation for Majsoul token capture via Chrome DevTools Protocol.

use anyhow::{Context, Result};
use base64::Engine;
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::network::{
    EnableParams as NetworkEnable, EventWebSocketFrameReceived,
};
use futures::StreamExt;
use std::path::PathBuf;
use std::time::Duration;
use tokio::time::timeout;
use tracing::{debug, info, warn};

/// Path to cached token file
fn token_cache_path() -> Result<PathBuf> {
    let config_dir = dirs::config_dir()
        .context("Could not find config directory")?
        .join("tenhou-scraper");
    std::fs::create_dir_all(&config_dir)?;
    Ok(config_dir.join("majsoul-token.json"))
}

/// Cached token data
#[derive(serde::Serialize, serde::Deserialize, Debug)]
pub struct CachedToken {
    pub access_token: String,
    pub uid: u64,
    pub captured_at: i64,
}

impl CachedToken {
    /// Load cached token from disk
    pub fn load() -> Result<Option<Self>> {
        let path = token_cache_path()?;
        if !path.exists() {
            return Ok(None);
        }
        let data = std::fs::read_to_string(&path)?;
        let token: CachedToken = serde_json::from_str(&data)?;
        Ok(Some(token))
    }

    /// Save token to disk
    pub fn save(&self) -> Result<()> {
        let path = token_cache_path()?;
        let data = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, data)?;
        info!("Token saved to {:?}", path);
        Ok(())
    }
}

/// Launch Chrome with optimized flags for fast startup
async fn launch_browser(
) -> Result<(
    Browser,
    impl futures::Stream<Item = Result<(), chromiumoxide::error::CdpError>>,
)> {
    // Check if Chrome is available
    if std::process::Command::new("google-chrome")
        .arg("--version")
        .output()
        .is_err()
        && std::process::Command::new("chromium")
            .arg("--version")
            .output()
            .is_err()
        && std::process::Command::new("chromium-browser")
            .arg("--version")
            .output()
            .is_err()
    {
        anyhow::bail!(
            "Chrome/Chromium not found. Please install Chrome or Chromium.\n\
             Ubuntu: sudo apt install chromium-browser\n\
             Arch: sudo pacman -S chromium"
        );
    }

    let config = BrowserConfig::builder()
        .no_sandbox()
        .with_head() // Show the browser window for interactive login
        .arg("--disable-gpu")
        .arg("--disable-dev-shm-usage")
        .arg("--disable-extensions")
        .arg("--disable-background-networking")
        .arg("--disable-default-apps")
        .arg("--no-first-run")
        .arg("--disable-features=Translate,OptimizationHints,MediaRouter")
        .window_size(1280, 800)
        .build()
        .map_err(|e| anyhow::anyhow!("Failed to build browser config: {}", e))?;

    let (browser, handler) = Browser::launch(config)
        .await
        .context("Failed to launch Chrome. Is Chrome/Chromium installed?")?;

    Ok((browser, handler))
}

/// Capture access token by intercepting browser WebSocket traffic
pub async fn capture_token_interactive() -> Result<CachedToken> {
    info!("Launching Chrome for Majsoul authentication...");
    info!("Please login to Majsoul in the browser window that opens.");

    let (browser, mut handler) = launch_browser().await?;

    // Drive the browser connection in background
    let handler_task = tokio::spawn(async move {
        while let Some(result) = handler.next().await {
            if result.is_err() {
                break;
            }
        }
    });

    // Navigate to Majsoul
    let page = browser
        .new_page("https://game.maj-soul.com/1/")
        .await
        .context("Failed to open Majsoul page")?;

    // Enable network domain for WebSocket interception
    page.execute(NetworkEnable::default())
        .await
        .context("Failed to enable network domain")?;

    // Listen for WebSocket frames
    let mut ws_listener = page
        .event_listener::<EventWebSocketFrameReceived>()
        .await
        .context("Failed to create WebSocket listener")?;

    info!("Waiting for login... (timeout: 5 minutes)");
    info!("Login in the browser window, then wait for token capture.");

    let mut frame_count = 0u64;

    let token = timeout(Duration::from_secs(300), async {
        while let Some(event) = ws_listener.next().await {
            let frame = &event.response;

            // Binary frame (opcode 2)
            if frame.opcode == 2.0 {
                if let Ok(data) =
                    base64::engine::general_purpose::STANDARD.decode(&frame.payload_data)
                {
                    // Response packet starts with 0x03
                    if data.len() > 3 && data[0] == 0x03 {
                        if let Some(token) = try_extract_oauth2_token(&data[3..]) {
                            return Ok(token);
                        }
                    }
                }
            }

            frame_count += 1;
            if frame_count % 50 == 0 {
                debug!("Processed {} WebSocket frames...", frame_count);
            }
        }
        anyhow::bail!("WebSocket stream ended without capturing token")
    })
    .await
    .context("Timeout waiting for login")??;

    // Cleanup
    drop(page);
    drop(browser);
    handler_task.abort();

    // Save to cache
    token.save()?;
    info!("Token captured and cached successfully!");

    Ok(token)
}

/// Try to extract access_token from oauth2Auth response
fn try_extract_oauth2_token(data: &[u8]) -> Option<CachedToken> {
    // Decode wrapper: field 1 = name, field 2 = payload
    let (name, payload) = decode_wrapper(data).ok()?;

    if !name.contains("oauth2Auth") {
        return None;
    }

    debug!("Found oauth2Auth response, extracting token...");

    // Parse access_token from payload (field 2)
    let access_token = extract_string_field(&payload, 2)?;

    // We don't have UID here, will get it from login response
    // For now, use 0 and update later
    Some(CachedToken {
        access_token,
        uid: 0,
        captured_at: chrono::Utc::now().timestamp(),
    })
}

/// Simple protobuf wrapper decoder (matches rpc.rs wrapper module)
fn decode_wrapper(buf: &[u8]) -> Result<(String, Vec<u8>)> {
    let mut pos = 0;
    let mut name = String::new();
    let mut data = Vec::new();

    while pos < buf.len() {
        if pos >= buf.len() {
            break;
        }
        let tag = buf[pos];
        pos += 1;
        let field_num = tag >> 3;
        let wire_type = tag & 0x07;

        if wire_type != 2 {
            if wire_type == 0 {
                while pos < buf.len() && buf[pos] & 0x80 != 0 {
                    pos += 1;
                }
                pos += 1;
            }
            continue;
        }

        let (len, bytes_read) = decode_varint(&buf[pos..])?;
        pos += bytes_read;
        let end = pos + len as usize;
        if end > buf.len() {
            anyhow::bail!("Buffer overflow");
        }

        match field_num {
            1 => name = String::from_utf8_lossy(&buf[pos..end]).to_string(),
            2 => data = buf[pos..end].to_vec(),
            _ => {}
        }
        pos = end;
    }
    Ok((name, data))
}

fn decode_varint(buf: &[u8]) -> Result<(u64, usize)> {
    let mut value: u64 = 0;
    let mut shift = 0;
    let mut pos = 0;
    loop {
        if pos >= buf.len() {
            anyhow::bail!("Unexpected end in varint");
        }
        let byte = buf[pos];
        pos += 1;
        value |= ((byte & 0x7f) as u64) << shift;
        if byte & 0x80 == 0 {
            break;
        }
        shift += 7;
    }
    Ok((value, pos))
}

fn extract_string_field(data: &[u8], target_field: u8) -> Option<String> {
    let mut pos = 0;
    while pos < data.len() {
        let tag = data[pos];
        pos += 1;
        let field_num = tag >> 3;
        let wire_type = tag & 0x07;

        if wire_type == 2 {
            let (len, bytes_read) = decode_varint(&data[pos..]).ok()?;
            pos += bytes_read;
            let end = pos + len as usize;
            if end > data.len() {
                return None;
            }
            if field_num == target_field {
                return Some(String::from_utf8_lossy(&data[pos..end]).to_string());
            }
            pos = end;
        } else if wire_type == 0 {
            while pos < data.len() && data[pos] & 0x80 != 0 {
                pos += 1;
            }
            pos += 1;
        } else {
            return None;
        }
    }
    None
}
