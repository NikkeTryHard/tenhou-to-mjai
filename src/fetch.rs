use anyhow::{Context, Result};
use chrono::NaiveDate;
use flate2::read::GzDecoder;
use regex::Regex;
use std::io::Read;
use tracing::{info, warn};

use crate::db::{Database, LogEntry};

const TENHOU_BASE_URL: &str = "https://tenhou.net/sc/raw/dat";

pub struct Fetcher {
    client: reqwest::Client,
    delay_ms: u64,
}

impl Fetcher {
    pub fn new(delay_ms: u64) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()?;
        Ok(Self { client, delay_ms })
    }

    pub async fn fetch_date_range(
        &self,
        db: &Database,
        start: NaiveDate,
        end: NaiveDate,
        log_types: &[&str],
        skip_fetched: bool,
    ) -> Result<usize> {
        let mut total_new = 0;
        let mut current = start;

        while current <= end {
            let date_str = current.format("%Y%m%d").to_string();
            let year = current.format("%Y").to_string();

            if skip_fetched && db.is_date_fetched(&date_str)? {
                info!("Skipping already fetched date: {}", date_str);
                current = current.succ_opt().unwrap();
                continue;
            }

            for log_type in log_types {
                let url = format!(
                    "{}/{}/{}{}.html.gz",
                    TENHOU_BASE_URL, year, log_type, date_str
                );

                match self.fetch_log_ids_from_url(&url).await {
                    Ok(entries) => {
                        let mut new_count = 0;
                        for entry in &entries {
                            if db.insert_log_id(entry)? {
                                new_count += 1;
                            }
                        }
                        info!(
                            "Fetched {} {} - {} total, {} new",
                            log_type,
                            date_str,
                            entries.len(),
                            new_count
                        );
                        total_new += new_count;
                    }
                    Err(e) => {
                        warn!("Failed to fetch {} {}: {}", log_type, date_str, e);
                    }
                }

                tokio::time::sleep(std::time::Duration::from_millis(self.delay_ms)).await;
            }

            db.mark_date_fetched(&date_str)?;
            current = current.succ_opt().unwrap();
        }

        Ok(total_new)
    }

    async fn fetch_log_ids_from_url(&self, url: &str) -> Result<Vec<LogEntry>> {
        let response = self.client.get(url).send().await?;

        if !response.status().is_success() {
            anyhow::bail!("HTTP {}", response.status());
        }

        let bytes = response.bytes().await?;

        let mut decoder = GzDecoder::new(&bytes[..]);
        let mut html = String::new();
        decoder
            .read_to_string(&mut html)
            .context("Failed to decompress gzip")?;

        self.parse_log_ids(&html)
    }

    fn parse_log_ids(&self, html: &str) -> Result<Vec<LogEntry>> {
        // Pattern: log=2025010100gm-00a9-0000-d7141b66
        let log_id_re = Regex::new(r"log=(\d{10}gm-([0-9a-f]{4})-[0-9a-f]{4}-[0-9a-f]{8})")?;

        let mut entries = Vec::new();

        for cap in log_id_re.captures_iter(html) {
            let full_id = cap.get(1).unwrap().as_str();
            let game_type_str = cap.get(2).unwrap().as_str();

            // Parse game type using bitmask (same as houou-logs)
            // Bit 4 (0x010) = 3-player if set, 4-player if unset
            // Bit 3 (0x008) = Hanchan if set, Tonpu if unset
            let Ok(game_type) = u16::from_str_radix(game_type_str, 16) else {
                continue;
            };

            let is_3p = (game_type & 0x010) != 0;
            let is_hanchan = (game_type & 0x008) != 0;

            // Only capture 4-player games
            if is_3p {
                continue;
            }

            let num_players = 4;

            // Extract date from log ID (first 8 chars)
            let date = &full_id[0..8];

            entries.push(LogEntry {
                id: full_id.to_string(),
                date: date.to_string(),
                num_players,
                is_hanchan,
                is_downloaded: false,
                is_converted: false,
                xml_data: None,
            });
        }

        Ok(entries)
    }
}
