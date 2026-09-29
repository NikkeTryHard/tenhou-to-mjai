//! Browser automation for Majsoul phantom resolution via Chrome `DevTools` Protocol.

use anyhow::{Context, Result};
use chromiumoxide::browser::{Browser, BrowserConfig};
use futures::StreamExt;
use std::time::Duration;
use tokio::time::timeout;
use tracing::{info, warn};

/// Server URLs for different regions
pub fn server_url(server: &str) -> &'static str {
    match server {
        "cn" => "https://game.maj-soul.com/1/",
        // EN, JP, and unknown servers share the yo-star endpoint.
        _ => "https://mahjongsoul.game.yo-star.com/",
    }
}

/// Profile type for browser launch
pub enum BrowserProfile {
    /// Interactive headed browser for login
    Interactive,
    /// Headless scraper (separate profile)
    #[allow(dead_code)]
    Headless,
}

/// Launch Chrome with optimized flags for fast startup
/// Uses PERSISTENT profile so you stay logged in
async fn launch_browser_with_profile(
    profile: BrowserProfile,
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

    // Use separate profile directories for interactive vs headless
    let profile_name = match profile {
        BrowserProfile::Interactive => "majsoul-chrome-profile",
        BrowserProfile::Headless => "majsoul-headless-profile",
    };
    let user_data_dir = dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("tenhou-scraper")
        .join(profile_name);
    std::fs::create_dir_all(&user_data_dir).ok();

    let is_headless = matches!(profile, BrowserProfile::Headless);
    info!("Using {} profile: {:?}", if is_headless { "headless" } else { "interactive" }, user_data_dir);

    // Try to find chromium first, fall back to chrome
    let chrome_path = ["chromium", "chromium-browser", "google-chrome", "chrome"]
        .iter()
        .find_map(|name| {
            std::process::Command::new("which")
                .arg(name)
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        });

    let mut builder = BrowserConfig::builder()
        .no_sandbox()
        .disable_default_args()
        .arg("--disable-blink-features=AutomationControlled")
        .arg("--exclude-switches=enable-automation")
        .arg("--disable-dev-shm-usage")
        .arg("--no-first-run")
        .arg("--disable-features=Translate,OptimizationHints,MediaRouter")
        .arg(format!("--user-data-dir={}", user_data_dir.display()))
        .window_size(1280, 800);

    // Headed for interactive, headless for scraping
    if is_headless {
        builder = builder.arg("--headless=new");
    } else {
        builder = builder.with_head();
    }

    if let Some(path) = chrome_path {
        info!("Using browser: {}", path);
        builder = builder.chrome_executable(path);
    }

    let config = builder
        .build()
        .map_err(|e| anyhow::anyhow!("Failed to build browser config: {e}"))?;

    let (browser, handler) = Browser::launch(config)
        .await
        .context("Failed to launch Chrome. Is Chrome/Chromium installed?")?;

    Ok((browser, handler))
}

/// Resolve phantom UUIDs (short UUIDs without `full_uuid`) via browser injection
/// Returns Vec<(`short_uuid`, `full_uuid`)> for successfully resolved games
// Length is page-evaluate/retry scaffolding per UUID; splitting would churn
// the verified injection flow.
#[allow(clippy::too_many_lines)]
pub async fn resolve_phantom_uuids(
    server: &str,
    short_uuids: &[String],
    delay_ms: u64,
) -> Result<Vec<(String, String)>> {
    if short_uuids.is_empty() {
        return Ok(Vec::new());
    }

    let url = server_url(server);
    info!("Launching browser to resolve {} phantom UUIDs", short_uuids.len());

    let (browser, mut handler) = launch_browser_with_profile(BrowserProfile::Interactive).await?;

    let handler_task = tokio::spawn(async move {
        while let Some(result) = handler.next().await {
            if result.is_err() {
                break;
            }
        }
    });

    let page = browser
        .new_page(url)
        .await
        .context("Failed to open Majsoul page")?;

    // Wait for lobby to be ready (user may need to login/dismiss dialogs)
    info!("Waiting for lobby... (login if needed, dismiss any popups)");
    let lobby_ready = timeout(Duration::from_secs(300), async {
        let mut attempt = 0;
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            attempt += 1;

            // Auto-close Yostar Account Binding dialog if present
            let _ = page.evaluate(r"
                (function() {
                    const bindingText = document.querySelector('span.ellipsis');
                    if (bindingText && bindingText.textContent.includes('Yostar Account Binding')) {
                        const closeBtn = document.querySelector('div.btn.close');
                        if (closeBtn) {
                            closeBtn.click();
                            return 'closed';
                        }
                    }
                    return 'not-found';
                })()
            ").await;

            // Check if lobby is ready using GameMgr.Inst.login_loading_end
            let check = page.evaluate(r"
                (function() {
                    try {
                        const lobbyReady = (typeof GameMgr !== 'undefined' && GameMgr.Inst && GameMgr.Inst.login_loading_end === true);
                        const inGame = (typeof view !== 'undefined' && view.DesktopMgr && view.DesktopMgr.Inst && view.DesktopMgr.Inst.active === true);

                        return JSON.stringify({
                            lobbyReady: lobbyReady,
                            inGame: inGame
                        });
                    } catch(e) {
                        return JSON.stringify({error: e.toString()});
                    }
                })()
            ").await;

            if let Ok(val) = check {
                if let Some(json_str) = val.value().and_then(|v| v.as_str()) {
                    if attempt % 5 == 0 {
                        info!("Lobby check #{}: {}", attempt, json_str);
                    }
                    if json_str.contains("\"lobbyReady\":true") || json_str.contains("\"inGame\":true") {
                        return Ok::<(), anyhow::Error>(());
                    }
                }
            }
        }
    })
    .await;

    if lobby_ready.is_err() {
        drop(page);
        drop(browser);
        handler_task.abort();
        anyhow::bail!("Timeout waiting for lobby");
    }

    info!("Lobby ready! Starting phantom UUID resolution...");

    let mut results = Vec::new();

    for (i, short_uuid) in short_uuids.iter().enumerate() {
        info!("[{}/{}] Resolving {}...", i + 1, short_uuids.len(), short_uuid);

        // Use app.NetAgent.sendReq2Lobby to fetch game record and extract full UUID from head.uuid
        let script = format!(
            r#"
            new Promise((resolve, reject) => {{
                const uuid = "{short_uuid}";
                const timeoutId = setTimeout(() => reject('Timeout after 30s'), 30000);

                if (typeof app === 'undefined' || !app.NetAgent || !app.NetAgent.sendReq2Lobby) {{
                    clearTimeout(timeoutId);
                    reject('app.NetAgent.sendReq2Lobby not available');
                    return;
                }}

                app.NetAgent.sendReq2Lobby('Lobby', 'fetchGameRecord', {{ game_uuid: uuid }}, (err, res) => {{
                    clearTimeout(timeoutId);
                    if (err) {{
                        reject(JSON.stringify(err));
                        return;
                    }}

                    // Extract full_uuid from res.head.uuid
                    if (res && res.head && res.head.uuid) {{
                        resolve(JSON.stringify({{
                            success: true,
                            full_uuid: res.head.uuid
                        }}));
                    }} else {{
                        reject('No head.uuid in response: ' + JSON.stringify(Object.keys(res || {{}})));
                    }}
                }});
            }})
            "#
        );

        let fetch_result = timeout(Duration::from_secs(30), page.evaluate(script.as_str())).await;

        match fetch_result {
            Ok(Ok(val)) => {
                if let Some(json_str) = val.value().and_then(|v| v.as_str()) {
                    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(json_str) {
                        if let Some(full_uuid) = parsed.get("full_uuid").and_then(|v| v.as_str()) {
                            info!("  Resolved: {} -> {}", short_uuid, full_uuid);
                            results.push((short_uuid.clone(), full_uuid.to_string()));
                        } else {
                            warn!("  Failed to extract full_uuid from response: {}", json_str);
                        }
                    } else {
                        warn!("  Failed to parse response as JSON: {}", json_str);
                    }
                } else {
                    warn!("  No value returned for {}", short_uuid);
                }
            }
            Ok(Err(e)) => {
                warn!("  Fetch error for {}: {}", short_uuid, e);
            }
            Err(_) => {
                warn!("  Timeout for {}", short_uuid);
            }
        }

        if delay_ms > 0 && i < short_uuids.len() - 1 {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        }
    }

    info!("Phantom UUID resolution complete. Resolved {}/{}", results.len(), short_uuids.len());
    drop(page);
    drop(browser);
    handler_task.abort();

    Ok(results)
}
