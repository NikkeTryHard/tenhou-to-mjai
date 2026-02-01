use anyhow::Result;
use indicatif::{ProgressBar, ProgressStyle};
use tracing::{info, warn};

use super::gateway::discover_gateway;
use super::rpc::MajsoulRpc;
use crate::db::Database;

pub struct MajsoulDownloader {
    delay_ms: u64,
}

impl MajsoulDownloader {
    pub fn new(delay_ms: u64) -> Self {
        Self { delay_ms }
    }

    pub async fn download_logs(
        &self,
        db: &Database,
        access_token: &str,
        limit: Option<usize>,
    ) -> Result<(usize, usize)> {
        let client = reqwest::Client::builder()
            .user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36")
            .build()?;

        let uuids = db.get_majsoul_undownloaded(limit)?;
        if uuids.is_empty() {
            info!("No pending downloads");
            return Ok((0, 0));
        }

        // Discover gateway with retry
        let (endpoint, version) = {
            let mut attempts = 0;
            loop {
                match discover_gateway(&client).await {
                    Ok(result) => break result,
                    Err(e) if attempts < 3 => {
                        attempts += 1;
                        warn!("Gateway discovery failed (attempt {}): {}", attempts, e);
                        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    }
                    Err(e) => return Err(e),
                }
            }
        };

        // Connect with retry
        let rpc = {
            let mut attempts = 0;
            loop {
                match MajsoulRpc::connect(&endpoint).await {
                    Ok(rpc) => break rpc,
                    Err(e) if attempts < 3 => {
                        attempts += 1;
                        warn!("Connection failed (attempt {}): {}", attempts, e);
                        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    }
                    Err(e) => return Err(e),
                }
            }
        };

        rpc.login(access_token, &version).await?;

        info!("Downloading {} game records", uuids.len());

        let pb = ProgressBar::new(uuids.len() as u64);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({eta})")?
                .progress_chars("#>-"),
        );

        let mut success = 0;
        let mut failed = 0;

        for uuid in &uuids {
            match rpc.fetch_game_record(uuid).await {
                Ok(data) => {
                    if let Err(e) = db.mark_majsoul_downloaded(uuid, &data) {
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
                        pb.finish_with_message("Rate limited");
                        return Err(anyhow::anyhow!(
                            "Rate limited (error 151). Try again later."
                        ));
                    }
                    db.mark_majsoul_download_error(uuid)?;
                    failed += 1;
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
