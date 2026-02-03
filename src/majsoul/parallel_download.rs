use anyhow::Result;
use indicatif::{ProgressBar, ProgressStyle};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{info, warn};

use super::gateway::discover_gateway;
use super::rpc::MajsoulRpc;
use crate::db::Database;

/// Gateway info discovered once and shared across workers.
#[derive(Clone)]
struct GatewayInfo {
    endpoint: String,
    version: String,
    route_id: String,
}

/// Distributes work across multiple workers.
pub struct WorkDistributor;

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
        }
    }

    /// Download logs using native login (username/password).
    ///
    /// Returns (success_count, failed_count).
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
        let client = reqwest::Client::builder()
            .user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36")
            .build()?;

        let (endpoint, version, route_id) = discover_gateway_with_retry(&client, server).await?;
        let gateway = GatewayInfo { endpoint, version, route_id };
        info!("Discovered gateway: {} (version {})", gateway.endpoint, gateway.version);

        // Connect and login
        let rpc = connect_and_login_native(&gateway, username, password).await?;

        // Set up progress bar
        let pb = ProgressBar::new(uuids.len() as u64);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({eta})")?
                .progress_chars("#>-"),
        );

        let mut success = 0;
        let mut failed = 0;
        let mut processed_since_restart = 0;
        let mut current_rpc = rpc;
        let mut current_gateway = gateway;

        for uuid in uuids {
            // Check if we need to restart connection
            if self.restart_every > 0 && processed_since_restart >= self.restart_every {
                info!("Restarting connection after {} records", processed_since_restart);
                let _ = current_rpc.close().await;
                current_rpc = connect_and_login_native(&current_gateway, username, password).await?;
                processed_since_restart = 0;
            }

            // Use Database::normalize_uuid for consistent key
            let short_uuid = Database::normalize_uuid(&uuid).to_string();

            match current_rpc.fetch_game_record(&uuid).await {
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
                    let err_str = e.to_string();
                    warn!("Failed to fetch {}: {}", uuid, err_str);

                    if err_str.contains("151") {
                        // Error 151 = version mismatch - re-discover gateway
                        warn!("Version mismatch (error 151), re-discovering gateway");
                        tokio::time::sleep(std::time::Duration::from_secs(60)).await;

                        // Re-discover gateway to get fresh version
                        let (new_endpoint, new_version, new_route_id) =
                            discover_gateway_with_retry(&client, server).await?;
                        current_gateway = GatewayInfo {
                            endpoint: new_endpoint,
                            version: new_version,
                            route_id: new_route_id,
                        };
                        info!("Re-discovered gateway version {}", current_gateway.version);

                        // Reconnect with new gateway info
                        let _ = current_rpc.close().await;
                        current_rpc = connect_and_login_native(&current_gateway, username, password).await?;
                        processed_since_restart = 0;

                        // Retry this UUID
                        match current_rpc.fetch_game_record(&uuid).await {
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
                    } else {
                        let db_guard = db.lock().await;
                        if let Err(db_err) = db_guard.mark_majsoul_download_error(&short_uuid) {
                            warn!("Failed to mark error for {}: {}", uuid, db_err);
                        }
                        failed += 1;
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

/// Discover gateway with retry logic.
async fn discover_gateway_with_retry(
    client: &reqwest::Client,
    server: &str,
) -> Result<(String, String, String)> {
    let mut attempts = 0;
    loop {
        match discover_gateway(client, server).await {
            Ok(result) => return Ok(result),
            Err(e) if attempts < 3 => {
                attempts += 1;
                warn!("Gateway discovery failed (attempt {}): {}", attempts, e);
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
            Err(e) => return Err(e),
        }
    }
}

/// Connect to gateway and login with native credentials.
async fn connect_and_login_native(
    gateway: &GatewayInfo,
    username: &str,
    password: &str,
) -> Result<MajsoulRpc> {
    let mut attempts = 0;
    loop {
        match MajsoulRpc::connect(&gateway.endpoint).await {
            Ok(rpc) => {
                rpc.login_native(username, password, &gateway.version, &gateway.route_id).await?;
                return Ok(rpc);
            }
            Err(e) if attempts < 3 => {
                attempts += 1;
                warn!("Connection failed (attempt {}): {}", attempts, e);
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
        let uuids: Vec<String> = (0..10).map(|i| format!("uuid-{}", i)).collect();
        let chunks = WorkDistributor::chunk_work(uuids, 3);

        assert_eq!(chunks.len(), 3);

        // Round-robin distribution: 0,3,6,9 | 1,4,7 | 2,5,8
        assert_eq!(chunks[0].len(), 4); // 0, 3, 6, 9
        assert_eq!(chunks[1].len(), 3); // 1, 4, 7
        assert_eq!(chunks[2].len(), 3); // 2, 5, 8

        // Verify total count
        let total: usize = chunks.iter().map(|c| c.len()).sum();
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
        let uuids: Vec<String> = (0..2).map(|i| format!("uuid-{}", i)).collect();
        let chunks = WorkDistributor::chunk_work(uuids, 5);

        assert_eq!(chunks.len(), 5);

        // Only first 2 workers get work
        assert_eq!(chunks[0].len(), 1);
        assert_eq!(chunks[1].len(), 1);
        assert_eq!(chunks[2].len(), 0);
        assert_eq!(chunks[3].len(), 0);
        assert_eq!(chunks[4].len(), 0);

        // Verify total count
        let total: usize = chunks.iter().map(|c| c.len()).sum();
        assert_eq!(total, 2);
    }

    #[test]
    fn test_work_distributor_empty() {
        let uuids: Vec<String> = vec![];
        let chunks = WorkDistributor::chunk_work(uuids, 3);

        assert_eq!(chunks.len(), 3);
        assert!(chunks.iter().all(|c| c.is_empty()));
    }

    #[test]
    fn test_work_distributor_zero_workers() {
        let uuids: Vec<String> = vec!["uuid-0".to_string()];
        let chunks = WorkDistributor::chunk_work(uuids, 0);

        assert_eq!(chunks.len(), 0);
    }
}
