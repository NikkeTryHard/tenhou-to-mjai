use anyhow::{Context, Result};
use tracing::info;

use super::types::{GameRecord, PlayerSearchResult};

const DATA_BASE: &str = "https://5-data.amae-koromo.com/api/v2/pl4";

pub struct AmaeKoromoClient {
    client: reqwest::Client,
    delay_ms: u64,
}

impl AmaeKoromoClient {
    pub fn new(delay_ms: u64) -> Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36")
            .timeout(std::time::Duration::from_secs(30))
            .build()?;
        Ok(Self { client, delay_ms })
    }

    pub async fn search_player(&self, nickname: &str) -> Result<Vec<PlayerSearchResult>> {
        let url = format!("{}/search_player/{}?limit=20", DATA_BASE, nickname);
        info!("Searching for player: {}", nickname);

        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .context("Failed to search player")?;

        if !resp.status().is_success() {
            anyhow::bail!("HTTP {}", resp.status());
        }

        let results: Vec<PlayerSearchResult> = resp.json().await?;
        Ok(results)
    }

    pub async fn get_player_records(
        &self,
        player_id: i64,
        start_ms: i64,
        end_ms: i64,
        mode: i32,
    ) -> Result<Vec<GameRecord>> {
        let url = format!(
            "{}/player_records/{}/{}/{}?mode={}",
            DATA_BASE, player_id, start_ms, end_ms, mode
        );

        info!("Fetching records for player {} (mode {})", player_id, mode);

        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .context("Failed to fetch player records")?;

        if !resp.status().is_success() {
            anyhow::bail!("HTTP {}", resp.status());
        }

        let records: Vec<GameRecord> = resp.json().await?;

        if self.delay_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(self.delay_ms)).await;
        }

        Ok(records)
    }

    /// Fetch recent records from a specific room (no player ID required)
    pub async fn get_room_records(
        &self,
        start_ms: i64,
        end_ms: i64,
        mode: i32,
        limit: u32,
    ) -> Result<Vec<GameRecord>> {
        let url = format!(
            "{}/games/{}/{}?mode={}&limit={}",
            DATA_BASE, start_ms, end_ms, mode, limit
        );

        info!("Fetching room records (mode {}, limit {})", mode, limit);

        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .context("Failed to fetch room records")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("HTTP {}: {}", status, body);
        }

        let records: Vec<GameRecord> = resp.json().await?;

        if self.delay_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(self.delay_ms)).await;
        }

        Ok(records)
    }
}
