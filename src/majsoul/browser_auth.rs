//! Authenticated-token capture for the modern English Unity Mahjong Soul client.
//!
//! Read-only CDP capture: connects to the caller's existing game browser,
//! polls English game tabs for `localStorage["access_token"]`, and saves the
//! token for the network downloader. Never navigates, closes, or otherwise
//! mutates tabs; never calls login RPCs (a second login would displace the
//! browser session). Token presence only means capture readiness, not proof
//! of an authenticated account.

use anyhow::{Context, Result};
use chromiumoxide::browser::Browser;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::io::Write as _;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

/// Saved Unity-client auth, consumed by the network download module.
///
/// No `Debug` impl on purpose: the token must never be formattable into logs.
#[derive(Clone, Serialize, Deserialize)]
pub struct SavedAuth {
    pub access_token: String,
    pub captured_at: String,
    pub account_id: Option<u64>,
}


/// Interval between auto-detect polls of the existing game tabs.
const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Disambiguates temp files when captures share one output path in a process.
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Aborts the CDP event-pump task when the capture scope exits (all paths).
struct HandlerGuard(tokio::task::AbortHandle);

impl Drop for HandlerGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Capture `access_token` from the existing game browser via CDP and save it.
///
/// Connects to `cdp_url` (e.g. `http://127.0.0.1:9222`), polls the English
/// game tabs' `localStorage["access_token"]` for up to `timeout_secs`, then
/// falls back to a manual Enter prompt followed by one final sweep. The
/// browser is only read from: no tabs are opened, navigated, or closed, and
/// no login RPC is issued to validate the token. Saves a `0600` atomic file,
/// creating the parent directory with `0700` only when it did not exist.
///
/// # Errors
///
/// Returns an error when the CDP endpoint is unreachable, the tab list cannot
/// be read, `timeout_secs` is zero, no non-empty token is found, or the output
/// cannot be written securely. Token validity is checked by the downloader.
pub async fn capture(cdp_url: &str, output: &Path, timeout_secs: u64, origin: &str, manual: bool) -> Result<()> {
    let origin = reqwest::Url::parse(origin).context("invalid game origin")?;
    anyhow::ensure!(origin.scheme() == "https" && origin.host_str().is_some(), "game origin must be HTTPS");
    if cdp_url.trim().is_empty() {
        anyhow::bail!("CDP URL must not be empty");
    }
    if timeout_secs == 0 {
        anyhow::bail!("timeout_secs must be at least 1");
    }

    let (browser, mut handler) = Browser::connect(cdp_url.to_owned())
        .await
        .context("CDP connect (is the game browser running with remote debugging enabled?)")?;
    // Pump CDP events; the guard aborts the task on every exit path below.
    // The browser belongs to the user: never call `browser.close()` here.
    let handler_task = tokio::spawn(async move { while handler.next().await.is_some() {} });
    let _handler_guard = HandlerGuard(handler_task.abort_handle());

    tracing::info!("Waiting for a usable Mahjong Soul token in the existing browser");

    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        if let Some(auth) = find_usable_token(&browser, &origin).await? {
            save_secure(output, &auth)?;
            tracing::info!(
                path = %output.display(),
                token_len = auth.access_token.len(),
                has_account = auth.account_id.is_some(),
                "saved Mahjong Soul auth"
            );
            return Ok(());
        }
        if Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    anyhow::ensure!(manual, "no token captured within {timeout_secs}s; use --manual to enable an Enter fallback");
    // Manual fallback. No concurrent stdin task exists during auto-detect, so
    // this single awaited `spawn_blocking` cannot leave a lingering reader.
    println!("No usable Mahjong Soul token found within {timeout_secs}s.");
    println!("In the existing browser, open {origin} and log in,");
    println!("then press Enter here for one final check (Ctrl-C to abort)...");
    std::io::stdout().flush()?;
    tokio::task::spawn_blocking(|| -> std::io::Result<()> {
        let mut line = String::new();
        read_confirmation(&mut line)
    }).await.context("manual-confirm reader task failed")??;

    let Some(auth) = find_usable_token(&browser, &origin).await? else {
        anyhow::bail!(
            "no usable access_token in any English game tab; log in at \
             https://mahjongsoul.game.yo-star.com/ in the CDP browser and retry"
        );
    };
    save_secure(output, &auth)?;
    tracing::info!(
        path = %output.display(),
        token_len = auth.access_token.len(),
        has_account = auth.account_id.is_some(),
        "saved Mahjong Soul auth"
    );
    Ok(())
}

/// Scan existing English game tabs for the first usable token.
///
/// Per-tab read failures are skipped (tabs come and go); connection-level and
/// tab-list failures propagate to the caller.
async fn find_usable_token(browser: &Browser, origin: &reqwest::Url) -> Result<Option<SavedAuth>> {
    let pages = browser.pages().await.context("CDP list pages")?;
    for page in pages {
        let Ok(url) = page.url().await else {
            continue;
        };
        let url = url.unwrap_or_default();
        if !is_game_url(&url, origin) {
            continue;
        }
        let Ok(token) = read_single_key(&page, "access_token").await else {
            continue;
        };
        let Some(token) = token.map(|token| token.trim().to_string()).filter(|token| !token.is_empty())
        else {
            continue;
        };
        // Best effort only: a missing/unparsable id stays `None`.
        let account_id: Option<u64> = read_single_key(&page, "account_id")
            .await
            .unwrap_or(None)
            .and_then(|raw| raw.trim().parse::<u64>().ok());
        return Ok(Some(SavedAuth {
            access_token: token,
            captured_at: chrono::Utc::now()
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            account_id,
        }));
    }
    Ok(None)
}

/// Read exactly one named `localStorage` key; never enumerate storage.
async fn read_single_key(
    page: &chromiumoxide::page::Page,
    key: &str,
) -> Result<Option<String>> {
    let script = format!(
        "(function(){{try{{return window.localStorage.getItem('{key}');}}catch(e){{return null;}}}})()"
    );
    let value = page
        .evaluate(script.as_str())
        .await
        .with_context(|| format!("CDP evaluate localStorage key {key}"))?
        .into_value::<Option<String>>()
        .with_context(|| format!("decode localStorage key {key}"))?;
    Ok(value)
}

fn read_confirmation(line: &mut String) -> std::io::Result<()> {
    if std::io::stdin().read_line(line)? == 0 {
        return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "manual confirmation needs stdin"));
    }
    Ok(())
}
/// Whether the tab belongs to the English game client.
fn is_game_url(url: &str, origin: &reqwest::Url) -> bool {
    reqwest::Url::parse(url).is_ok_and(|url| url.origin() == origin.origin())
}


/// Write the auth file atomically with `0600`, creating a missing parent
/// directory with `0700`. All failures propagate; a failed write leaves no
/// temp file behind and never touches a pre-existing parent directory.
fn save_secure(output: &Path, auth: &SavedAuth) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(auth).context("encode auth JSON")?;
    let parent: &Path = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if !parent.exists() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create auth dir {dir}", dir = parent.display()))?;
        #[cfg(unix)]
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
            .with_context(|| format!("chmod 0700 {dir}", dir = parent.display()))?;
    }
    let stem = output.file_name().map_or_else(
        || String::from("auth.json"),
        |name| name.to_string_lossy().into_owned(),
    );
    let slot = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let tmp = parent.join(format!(".{stem}.tmp.{pid}.{slot}"));
    let written = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options
            .open(&tmp)
            .with_context(|| format!("create temp auth file {tmp}", tmp = tmp.display()))?;
        // Enforce even under odd umasks or a stale pre-existing temp file.
        #[cfg(unix)]
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("chmod 0600 {tmp}", tmp = tmp.display()))?;
        file.write_all(&bytes)
            .with_context(|| format!("write temp auth file {tmp}", tmp = tmp.display()))?;
        file.sync_all()
            .with_context(|| format!("sync temp auth file {tmp}", tmp = tmp.display()))?;
        drop(file);
        std::fs::rename(&tmp, output)
            .with_context(|| format!("rename {tmp} to {output}", tmp = tmp.display(), output = output.display()))?;
        Ok::<(), anyhow::Error>(())
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}
