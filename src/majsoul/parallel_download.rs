use anyhow::Result;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{info, warn};

use super::gateway::{FetchOutcome, classify_outcome, discover_and_connect};
use super::rpc::MajsoulRpc;
use crate::db::Database;

/// Distributes work across multiple workers.
/// Round-robin chunking exercised by unit tests only; the production bulk
/// path streams UUIDs directly, so the dead-code lint is suppressed here.
#[allow(dead_code)]
pub struct WorkDistributor;

#[allow(dead_code)]
impl WorkDistributor {
    /// Divide UUIDs evenly across workers by moving ownership.
    ///
    /// If there are fewer UUIDs than workers, some workers will get empty chunks.
    /// Uses round-robin distribution for even load balancing.
    pub fn chunk_work(uuids: Vec<String>, num_workers: usize) -> Vec<Vec<String>> {
        if num_workers == 0 {
            return vec![];
        }

        let mut chunks: Vec<Vec<String>> = (0..num_workers).map(|_| Vec::new()).collect();

        for (i, uuid) in uuids.into_iter().enumerate() {
            chunks[i % num_workers].push(uuid);
        }

        chunks
    }
}

/// Downloader that uses native login (username/password).
pub struct ParallelDownloader {
    delay_ms: u64,
    restart_every: usize,
    version: String,
}

impl ParallelDownloader {
    /// Create a new downloader.
    ///
    /// # Arguments
    /// * `delay_ms` - Delay between requests (in milliseconds)
    /// * `restart_every` - Restart RPC connection every N records (0 = never restart)
    pub fn new(delay_ms: u64, restart_every: usize) -> Self {
        Self {
            delay_ms,
            restart_every,
            version: String::new(),
        }
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

    /// Download logs using native login (username/password).
    ///
    /// Returns (`success_count`, `failed_count`).
    // Length is gateway/login/retry boilerplate around one per-uuid loop (D2
    // error policy); splitting would churn the verified retry behavior.
    #[allow(clippy::too_many_lines)]
    pub async fn download_with_credentials(
        &self,
        db: Arc<Mutex<Database>>,
        username: &str,
        password: &str,
        server: &str,
        limit: Option<usize>,
    ) -> Result<(usize, usize)> {
        // Get UUIDs to download
        let uuids = {
            let db_guard = db.lock().await;
            db_guard.get_majsoul_undownloaded_with_full_uuid(limit)?
        };

        if uuids.is_empty() {
            info!("No pending downloads with full_uuid");
            return Ok((0, 0));
        }

        info!("Downloading {} game records", uuids.len());

        // Discover gateway
        let client = crate::util::http_client()?;

        // Discover, connect, and login with retry.
        let (rpc, version) = connect_with_retry(&client, server, username, password).await?;
        let mut current_version =
            if self.version.is_empty() { version } else { self.version.clone() };
        info!("Connected to {} server (version {})", server, current_version);

        // Set up progress bar
        let pb = crate::util::progress_bar(uuids.len() as u64)?;

        let mut success = 0;
        let mut failed = 0;
        let mut processed_since_restart = 0;
        let mut current_rpc = rpc;
        let mut version_retries = 0u32;

        for uuid in uuids {
            // Check if we need to restart connection
            if self.restart_every > 0 && processed_since_restart >= self.restart_every {
                info!("Restarting connection after {} records", processed_since_restart);
                let _ = current_rpc.close().await;
                let (new_rpc, new_version) =
                    connect_with_retry(&client, server, username, password).await?;
                current_rpc = new_rpc;
                current_version =
                    if self.version.is_empty() { new_version } else { self.version.clone() };
                processed_since_restart = 0;
            }

            // Use Database::normalize_uuid for consistent key
            let short_uuid = Database::normalize_uuid(&uuid).to_string();
            // version.json "X.Y.w" -> login/fetch "web-X.Y" (see gateway.rs).
            let client_version = format!("web-{}", current_version.replace(".w", ""));

            match current_rpc.fetch_game_record(&uuid, &client_version).await {
                Ok(data) => {
                    let db_guard = db.lock().await;
                    if let Err(e) = db_guard.mark_majsoul_downloaded(&short_uuid, &data) {
                        warn!("Failed to save {}: {}", uuid, e);
                        failed += 1;
                    } else {
                        success += 1;
                    }
                }
                Err(e) => {
                    warn!("Failed to fetch {}: {}", uuid, e);
                    match classify_outcome(&e) {
                        FetchOutcome::Abort(err) => {
                            pb.finish_with_message("Rate limited");
                            let _ = current_rpc.close().await;
                            return Err(err);
                        }
                        FetchOutcome::Done => {
                            if version_retries >= 3 {
                                pb.finish_with_message("Version mismatch");
                                let _ = current_rpc.close().await;
                                anyhow::bail!("Version mismatch persists after 3 gateway rediscoveries");
                            }
                            version_retries += 1;
                            warn!("Version mismatch, re-discovering gateway (retry {})", version_retries);
                            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                            let _ = current_rpc.close().await;
                            let (new_rpc, new_version) =
                                connect_with_retry(&client, server, username, password).await?;
                            current_rpc = new_rpc;
                            current_version = if self.version.is_empty() {
                                new_version
                            } else {
                                self.version.clone()
                            };
                            info!("Re-discovered gateway version {}", current_version);
                            processed_since_restart = 0;
                            let retry_version =
                                format!("web-{}", current_version.replace(".w", ""));
                            match current_rpc.fetch_game_record(&uuid, &retry_version).await {
                                Ok(data) => {
                                    let db_guard = db.lock().await;
                                    if let Err(e) = db_guard.mark_majsoul_downloaded(&short_uuid, &data) {
                                        warn!("Failed to save {}: {}", uuid, e);
                                        failed += 1;
                                    } else {
                                        success += 1;
                                    }
                                }
                                Err(e) => {
                                    warn!("Failed to fetch {} after retry: {}", uuid, e);
                                    let db_guard = db.lock().await;
                                    if let Err(db_err) = db_guard.mark_majsoul_download_error(&short_uuid) {
                                        warn!("Failed to mark error for {}: {}", uuid, db_err);
                                    }
                                    failed += 1;
                                }
                            }
                        }
                        FetchOutcome::MarkAndContinue => {
                            let db_guard = db.lock().await;
                            if let Err(db_err) = db_guard.mark_majsoul_download_error(&short_uuid) {
                                warn!("Failed to mark error for {}: {}", uuid, db_err);
                            }
                            failed += 1;
                        }
                    }
                }
            }

            processed_since_restart += 1;
            pb.inc(1);

            if self.delay_ms > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(self.delay_ms)).await;
            }
        }

        pb.finish_with_message("Done");
        let _ = current_rpc.close().await;

        Ok((success, failed))
    }
}

/// Discover, connect, and login with retry.
///
/// Thin retry loop around the shared [`discover_and_connect`] single-attempt
/// helper. The version-mismatch path keeps its own rediscover -> reconnect ->
/// retry-same-uuid flow around this (the helper alone does not cover the
/// same-uuid retry).
async fn connect_with_retry(
    client: &reqwest::Client,
    server: &str,
    username: &str,
    password: &str,
) -> Result<(MajsoulRpc, String)> {
    let mut attempts = 0;
    loop {
        match discover_and_connect(client, server, username, password).await {
            Ok(result) => return Ok(result),
            Err(e) if attempts < 3 => {
                attempts += 1;
                warn!("Gateway connect failed (attempt {}): {}", attempts, e);
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
            Err(e) => return Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_work_distributor() {
        let uuids: Vec<String> = (0..10).map(|i| format!("uuid-{i}")).collect();
        let chunks = WorkDistributor::chunk_work(uuids, 3);

        assert_eq!(chunks.len(), 3);

        // Round-robin distribution: 0,3,6,9 | 1,4,7 | 2,5,8
        assert_eq!(chunks[0].len(), 4); // 0, 3, 6, 9
        assert_eq!(chunks[1].len(), 3); // 1, 4, 7
        assert_eq!(chunks[2].len(), 3); // 2, 5, 8

        // Verify total count
        let total: usize = chunks.iter().map(std::vec::Vec::len).sum();
        assert_eq!(total, 10);

        // Verify round-robin assignment
        assert_eq!(chunks[0][0], "uuid-0");
        assert_eq!(chunks[1][0], "uuid-1");
        assert_eq!(chunks[2][0], "uuid-2");
        assert_eq!(chunks[0][1], "uuid-3");
    }

    #[test]
    fn test_work_distributor_uneven() {
        // Test with fewer items than workers
        let uuids: Vec<String> = (0..2).map(|i| format!("uuid-{i}")).collect();
        let chunks = WorkDistributor::chunk_work(uuids, 5);

        assert_eq!(chunks.len(), 5);

        // Only first 2 workers get work
        assert_eq!(chunks[0].len(), 1);
        assert_eq!(chunks[1].len(), 1);
        assert_eq!(chunks[2].len(), 0);
        assert_eq!(chunks[3].len(), 0);
        assert_eq!(chunks[4].len(), 0);

        // Verify total count
        let total: usize = chunks.iter().map(std::vec::Vec::len).sum();
        assert_eq!(total, 2);
    }

    #[test]
    fn test_work_distributor_empty() {
        let uuids: Vec<String> = vec![];
        let chunks = WorkDistributor::chunk_work(uuids, 3);

        assert_eq!(chunks.len(), 3);
        assert!(chunks.iter().all(std::vec::Vec::is_empty));
    }

    #[test]
    fn test_work_distributor_zero_workers() {
        let uuids: Vec<String> = vec!["uuid-0".to_string()];
        let chunks = WorkDistributor::chunk_work(uuids, 0);

        assert_eq!(chunks.len(), 0);
    }
}
