use anyhow::Result;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{info, warn};

use super::gateway::discover_gateway;
use super::rpc::MajsoulRpc;
use super::token_pool::{AccountToken, TokenPool};
use crate::db::Database;

/// Gateway info discovered once and shared across workers.
#[derive(Clone)]
struct GatewayInfo {
    endpoint: String,
    version: String,
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

/// Parallel downloader that uses multiple tokens from a pool.
pub struct ParallelDownloader {
    delay_ms: u64,
    restart_every: usize,
}

impl ParallelDownloader {
    /// Create a new parallel downloader.
    ///
    /// # Arguments
    /// * `delay_ms` - Delay between requests per worker (in milliseconds)
    /// * `restart_every` - Restart RPC connection every N records (0 = never restart)
    pub fn new(delay_ms: u64, restart_every: usize) -> Self {
        Self {
            delay_ms,
            restart_every,
        }
    }

    /// Download logs using a pool of tokens in parallel.
    ///
    /// Each worker gets its own token and processes a chunk of UUIDs.
    /// Gateway is discovered once and shared across all workers.
    /// Returns (success_count, failed_count).
    pub async fn download_with_pool(
        &self,
        db: Arc<Mutex<Database>>,
        pool: &TokenPool,
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

        let num_workers = pool.len();
        if num_workers == 0 {
            anyhow::bail!("Token pool is empty");
        }

        info!(
            "Downloading {} game records with {} workers",
            uuids.len(),
            num_workers
        );

        // Discover gateway once before spawning workers (fix #1)
        let client = reqwest::Client::builder()
            .user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36")
            .build()?;

        // Use first token's server for gateway discovery
        let first_token = pool.next();
        let (endpoint, version) = discover_gateway_with_retry(&client, &first_token.server).await?;
        let gateway = GatewayInfo { endpoint, version };
        info!("Discovered gateway: {} (version {})", gateway.endpoint, gateway.version);

        // Distribute work (fix #4: takes Vec by value)
        let chunks = WorkDistributor::chunk_work(uuids, num_workers);

        // Set up progress bars
        let multi_progress = MultiProgress::new();
        let style = ProgressStyle::default_bar()
            .template("{prefix:.bold.dim} [{bar:30.cyan/blue}] {pos}/{len} ({eta})")?
            .progress_chars("##-");

        // Spawn workers
        let mut handles = Vec::new();

        for (worker_id, chunk) in chunks.into_iter().enumerate() {
            if chunk.is_empty() {
                continue;
            }

            let token = pool.next();
            let db_clone = Arc::clone(&db);
            let delay_ms = self.delay_ms;
            let restart_every = self.restart_every;
            let gateway_clone = gateway.clone();

            let pb = multi_progress.add(ProgressBar::new(chunk.len() as u64));
            pb.set_style(style.clone());
            pb.set_prefix(format!("Worker {}", worker_id));

            let handle = tokio::spawn(async move {
                Self::worker_loop(worker_id, token, chunk, db_clone, delay_ms, restart_every, pb, gateway_clone)
                    .await
            });

            handles.push(handle);
        }

        // Wait for all workers to complete
        let mut total_success = 0;
        let mut total_failed = 0;

        for handle in handles {
            match handle.await {
                Ok(Ok((success, failed))) => {
                    total_success += success;
                    total_failed += failed;
                }
                Ok(Err(e)) => {
                    warn!("Worker failed: {}", e);
                    total_failed += 1;
                }
                Err(e) => {
                    warn!("Worker panicked: {}", e);
                    total_failed += 1;
                }
            }
        }

        Ok((total_success, total_failed))
    }

    /// Worker loop that processes a chunk of UUIDs.
    /// Uses pre-discovered gateway info but can re-discover on version mismatch (error 151).
    async fn worker_loop(
        worker_id: usize,
        token: AccountToken,
        uuids: Vec<String>,
        db: Arc<Mutex<Database>>,
        delay_ms: u64,
        restart_every: usize,
        pb: ProgressBar,
        mut gateway: GatewayInfo,
    ) -> Result<(usize, usize)> {
        let client = reqwest::Client::builder()
            .user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36")
            .build()?;

        let mut success = 0;
        let mut failed = 0;
        let mut processed_since_restart = 0;

        // Initial connection using pre-discovered gateway
        let mut rpc = connect_and_login(&gateway.endpoint, &token, &gateway.version).await?;

        for uuid in uuids {
            // Check if we need to restart connection
            if restart_every > 0 && processed_since_restart >= restart_every {
                info!("Worker {}: Restarting connection", worker_id);
                let _ = rpc.close().await;
                rpc = connect_and_login(&gateway.endpoint, &token, &gateway.version).await?;
                processed_since_restart = 0;
            }

            // Use Database::normalize_uuid instead of duplicating logic (fix #3)
            let short_uuid = Database::normalize_uuid(&uuid).to_string();

            match rpc.fetch_game_record(&uuid).await {
                Ok(data) => {
                    let db_guard = db.lock().await;
                    if let Err(e) = db_guard.mark_majsoul_downloaded(&short_uuid, &data) {
                        warn!("Worker {}: Failed to save {}: {}", worker_id, uuid, e);
                        failed += 1;
                    } else {
                        success += 1;
                    }
                }
                Err(e) => {
                    let err_str = e.to_string();
                    warn!("Worker {}: Failed to fetch {}: {}", worker_id, uuid, err_str);

                    if err_str.contains("151") {
                        // Error 151 = version mismatch - re-discover gateway for fresh version (fix #2)
                        warn!("Worker {}: Version mismatch (error 151), re-discovering gateway", worker_id);
                        tokio::time::sleep(std::time::Duration::from_secs(60)).await;

                        // Re-discover gateway to get fresh version
                        let (new_endpoint, new_version) = discover_gateway_with_retry(&client, &token.server).await?;
                        gateway = GatewayInfo {
                            endpoint: new_endpoint,
                            version: new_version,
                        };
                        info!("Worker {}: Re-discovered gateway version {}", worker_id, gateway.version);

                        // Reconnect with new gateway info
                        let _ = rpc.close().await;
                        rpc = connect_and_login(&gateway.endpoint, &token, &gateway.version).await?;
                        processed_since_restart = 0;

                        // Retry this UUID
                        match rpc.fetch_game_record(&uuid).await {
                            Ok(data) => {
                                let db_guard = db.lock().await;
                                if let Err(e) = db_guard.mark_majsoul_downloaded(&short_uuid, &data)
                                {
                                    warn!("Worker {}: Failed to save {}: {}", worker_id, uuid, e);
                                    failed += 1;
                                } else {
                                    success += 1;
                                }
                            }
                            Err(e) => {
                                warn!(
                                    "Worker {}: Failed to fetch {} after retry: {}",
                                    worker_id, uuid, e
                                );
                                let db_guard = db.lock().await;
                                // Log warning on DB error instead of silently ignoring (fix #5)
                                if let Err(db_err) = db_guard.mark_majsoul_download_error(&short_uuid) {
                                    warn!("Worker {}: Failed to mark error for {}: {}", worker_id, uuid, db_err);
                                }
                                failed += 1;
                            }
                        }
                    } else {
                        let db_guard = db.lock().await;
                        // Log warning on DB error instead of silently ignoring (fix #5)
                        if let Err(db_err) = db_guard.mark_majsoul_download_error(&short_uuid) {
                            warn!("Worker {}: Failed to mark error for {}: {}", worker_id, uuid, db_err);
                        }
                        failed += 1;
                    }
                }
            }

            processed_since_restart += 1;
            pb.inc(1);

            if delay_ms > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            }
        }

        pb.finish();
        let _ = rpc.close().await;

        Ok((success, failed))
    }
}

/// Discover gateway with retry logic.
async fn discover_gateway_with_retry(
    client: &reqwest::Client,
    server: &str,
) -> Result<(String, String)> {
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

/// Connect to gateway and login with token.
async fn connect_and_login(
    endpoint: &str,
    token: &AccountToken,
    version: &str,
) -> Result<MajsoulRpc> {
    let mut attempts = 0;
    loop {
        match MajsoulRpc::connect(endpoint).await {
            Ok(rpc) => {
                // Format token as "token-uid" for login
                let login_token = format!("{}-{}", token.token, token.uid);
                rpc.login(&login_token, version, &token.server).await?;
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
