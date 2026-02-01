use anyhow::Result;
use flate2::write::GzEncoder;
use flate2::Compression;
use indicatif::{ProgressBar, ProgressStyle};
use std::io::Write;
use std::time::Duration;
use tracing::{info, warn};

use crate::db::Database;

const TENHOU_LOG_URL: &str = "https://tenhou.net/0/log/";

pub struct Downloader {
    client: reqwest::Client,
    delay_ms: u64,
}

impl Downloader {
    pub fn new(delay_ms: u64) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()?;
        Ok(Self { client, delay_ms })
    }

    pub async fn download_logs(&self, db: &Database, limit: Option<usize>) -> Result<(usize, usize)> {
        let ids = db.get_undownloaded_ids(limit)?;

        if ids.is_empty() {
            info!("No logs to download");
            return Ok((0, 0));
        }

        info!("Downloading {} logs", ids.len());

        let pb = ProgressBar::new(ids.len() as u64);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({eta})")?
                .progress_chars("#>-"),
        );

        let mut success = 0;
        let mut failed = 0;

        for id in ids {
            match self.download_single(&id).await {
                Ok(xml_data) => {
                    // Compress before storing
                    let compressed = compress_gzip(&xml_data)?;
                    db.mark_downloaded(&id, &compressed)?;
                    success += 1;
                }
                Err(e) => {
                    warn!("Failed to download {}: {}", id, e);
                    db.mark_download_error(&id)?;
                    failed += 1;
                }
            }
            pb.inc(1);
            tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
        }

        pb.finish_with_message("Done");
        Ok((success, failed))
    }

    async fn download_single(&self, log_id: &str) -> Result<Vec<u8>> {
        let url = format!("{}?{}", TENHOU_LOG_URL, log_id);
        let response = self.client.get(&url).send().await?;

        if !response.status().is_success() {
            anyhow::bail!("HTTP {}", response.status());
        }

        let text = response.text().await?;

        // Validate it's actually an mjlog XML
        if !text.contains("mjloggm") {
            anyhow::bail!("Invalid response - not mjlog XML");
        }

        Ok(text.into_bytes())
    }
}

fn compress_gzip(data: &[u8]) -> Result<Vec<u8>> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(data)?;
    Ok(encoder.finish()?)
}
