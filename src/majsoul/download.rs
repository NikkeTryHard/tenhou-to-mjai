use anyhow::Result;
use tracing::{info, warn};

use super::gateway::{FetchOutcome, classify_outcome, discover_and_connect};
use crate::db::Database;

pub struct MajsoulDownloader {
    delay_ms: u64,
    version: String,
}

impl MajsoulDownloader {
    pub fn new(delay_ms: u64) -> Self {
        Self { delay_ms, version: String::new() }
    }

    /// Override the gateway-discovered client version (setter chosen to
    /// minimize callsite edits; empty means "use discovered version").
    /// No production caller wires an override yet (that needs a main.rs
    /// callsite, D1); kept as the intended override seam.
    #[allow(dead_code)]
    pub fn with_version(mut self, version: impl Into<String>) -> Self {
        self.version = version.into();
        self
    }

    /// Download logs using native login (username/password)
    // Length is gateway/login/retry boilerplate around one per-uuid loop (D2
    // error policy); splitting would churn the verified retry behavior.
    #[allow(clippy::too_many_lines)]
    pub async fn download_logs(
        &self,
        db: &Database,
        username: &str,
        password: &str,
        limit: Option<usize>,
        server: &str,
    ) -> Result<(usize, usize)> {
        let client = crate::util::http_client()?;

        let uuids = db.get_majsoul_undownloaded(limit)?;
        if uuids.is_empty() {
            info!("No pending downloads");
            return Ok((0, 0));
        }

        // Discover gateway, connect, and login with retry (per-server Origin
        // handled inside the helper).
        let (rpc, version) = {
            let mut attempts = 0;
            loop {
                match discover_and_connect(&client, server, username, password).await {
                    Ok(result) => break result,
                    Err(e) if attempts < 3 => {
                        attempts += 1;
                        warn!("Gateway connect failed (attempt {}): {}", attempts, e);
                        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    }
                    Err(e) => return Err(e),
                }
            }
        };

        info!("Downloading {} game records", uuids.len());

        let pb = crate::util::progress_bar(uuids.len() as u64)?;

        let effective_version = if self.version.is_empty() {
            version.clone()
        } else {
            self.version.clone()
        };
        // version.json "X.Y.w" -> login/fetch "web-X.Y" (see gateway.rs).
        let client_version = format!("web-{}", effective_version.replace(".w", ""));
        let mut success = 0;
        let mut failed = 0;
        // Bounded version-mismatch retries shared across the batch.
        let mut version_retries = 0u32;

        for uuid in &uuids {
            match rpc.fetch_game_record(uuid, &client_version).await {
                Ok(data) => {
                    if let Err(e) = db.mark_majsoul_downloaded(uuid, &data) {
                        warn!("Failed to save {}: {}", uuid, e);
                        failed += 1;
                    } else {
                        success += 1;
                    }
                }
                Err(e) => {
                    warn!("Failed to fetch {}: {}", uuid, e);
                    match classify_outcome(&e) {
                        FetchOutcome::Done => {
                            if version_retries >= 3 {
                                pb.finish_with_message("Version mismatch");
                                anyhow::bail!("Version mismatch persists after 3 gateway rediscoveries");
                            }
                            version_retries += 1;
                            warn!("Version mismatch, rediscovering gateway (retry {})", version_retries);
                            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                            // Best-effort rediscovery; batch continues on next uuid.
                            failed += 1;
                            if let Err(db_err) = db.mark_majsoul_download_error(uuid) {
                                warn!("Failed to mark error for {}: {}", uuid, db_err);
                            }
                        }
                        FetchOutcome::Abort(err) => {
                            pb.finish_with_message("Rate limited");
                            return Err(err);
                        }
                        FetchOutcome::MarkAndContinue => {
                            db.mark_majsoul_download_error(uuid)?;
                            failed += 1;
                        }
                    }
                }
            }
            pb.inc(1);
            if self.delay_ms > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(self.delay_ms)).await;
            }
        }

        pb.finish_with_message("Done");
        rpc.close().await?;
        Ok((success, failed))
    }
}
