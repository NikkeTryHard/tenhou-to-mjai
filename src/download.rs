use anyhow::Result;
use flate2::write::GzEncoder;
use flate2::Compression;
use futures::{stream, StreamExt};
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

        let pb = crate::util::progress_bar(ids.len() as u64)?;

        let mut success = 0;
        let mut failed = 0;

        if concurrent > 1 {
            // Parallel: persist each item as its download completes, so memory
            // stays bounded to `concurrent` in-flight payloads and a crash or
            // dropped DB connection only loses the in-flight items, not everything.
            let mut pending = stream::iter(ids)
                .map(|id| {
                    let client = self.client.clone();
                    let pb = pb.clone();
                    async move {
                        let result = download_single(&client, &id).await;
                        pb.inc(1);
                        (id, result)
                    }
                })
                .buffer_unordered(concurrent);

            while let Some((id, result)) = pending.next().await {
                if record_outcome(db, &id, result)? {
                    success += 1;
                } else {
                    failed += 1;
                }
            }
        } else {
            // Sequential: respect delay, persist immediately per item.
            for id in ids {
                let result = download_single(&self.client, &id).await;
                pb.inc(1);
                if record_outcome(db, &id, result)? {
                    success += 1;
                } else {
                    failed += 1;
                }
                tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
            }
        }

        pb.finish_with_message("Done");

        Ok((success, failed))
    }
}

async fn download_single(client: &reqwest::Client, log_id: &str) -> Result<Vec<u8>> {
    let url = format!("{TENHOU_LOG_URL}?{log_id}");
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

// Store gzipped: raw mjlog XML balloons the DB; convert/export decompress on read.
fn compress_gzip(data: &[u8]) -> Result<Vec<u8>> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(data)?;
    Ok(encoder.finish()?)
}

fn record_outcome(db: &Database, id: &str, data: Result<Vec<u8>>) -> Result<bool> {
    match data {
        Ok(xml_data) => match compress_gzip(&xml_data) {
            Ok(compressed) => {
                db.mark_downloaded(id, &compressed)?;
                Ok(true)
            }
            Err(e) => {
                warn!("Failed to compress {}: {}", id, e);
                db.mark_download_error(id)?;
                Ok(false)
            }
        },
        Err(e) => {
            warn!("Failed to download {}: {}", id, e);
            db.mark_download_error(id)?;
            Ok(false)
        }
    }
}
