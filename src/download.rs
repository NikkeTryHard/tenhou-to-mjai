use anyhow::Result;
use flate2::write::GzEncoder;
use flate2::Compression;
use futures::{stream, StreamExt};
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

    pub async fn download_logs(
        &self,
        db: &Database,
        limit: Option<usize>,
        concurrent: usize,
    ) -> Result<(usize, usize)> {
        let ids = db.get_undownloaded_ids(limit)?;

        if ids.is_empty() {
            info!("No logs to download");
            return Ok((0, 0));
        }

        info!("Downloading {} logs (concurrent: {})", ids.len(), concurrent);

        let pb = ProgressBar::new(ids.len() as u64);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({eta})")?
                .progress_chars("#>-"),
        );

        let results: Vec<_> = if concurrent > 1 {
            // Parallel: no delay, just blast
            stream::iter(ids)
                .map(|id| {
                    let client = self.client.clone();
                    let pb = pb.clone();
                    async move {
                        let result = download_single(&client, &id).await;
                        pb.inc(1);
                        (id, result)
                    }
                })
                .buffer_unordered(concurrent)
                .collect()
                .await
        } else {
            // Sequential: respect delay
            let mut results = Vec::new();
            for id in ids {
                let result = download_single(&self.client, &id).await;
                pb.inc(1);
                results.push((id, result));
                tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
            }
            results
        };

        pb.finish_with_message("Done");

        let mut success = 0;
        let mut failed = 0;

        for (id, result) in results {
            match result {
                Ok(xml_data) => {
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
        }

        Ok((success, failed))
    }
}

async fn download_single(client: &reqwest::Client, log_id: &str) -> Result<Vec<u8>> {
    let url = format!("{}?{}", TENHOU_LOG_URL, log_id);
    let response = client.get(&url).send().await?;

    if !response.status().is_success() {
        anyhow::bail!("HTTP {}", response.status());
    }

    let text = response.text().await?;

    if !text.contains("mjloggm") {
        anyhow::bail!("Invalid response - not mjlog XML");
    }

    Ok(text.into_bytes())
}

fn compress_gzip(data: &[u8]) -> Result<Vec<u8>> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(data)?;
    Ok(encoder.finish()?)
}
