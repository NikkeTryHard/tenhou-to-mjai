# Phase 1: Tenhou Scraper - Project Setup & Unified Pipeline

> **REQUIRED:** Use `execute-plan` to implement this plan batch by batch.

**Goal:** Build a robust Rust tool that scrapes Tenhou houou logs (2025-2026) and converts them to MJAI format in a single unified pipeline.

**Architecture:** Workspace with vendored crates (mjlog, tenhou-json, mjlog2json-core, convlog). CLI with subcommands for fetch (get log IDs), download (get XML), and convert (XML → MJAI). SQLite for state tracking. Rate-limited HTTP requests.

**Tech Stack:** Rust 1.92+, tokio, reqwest, rusqlite, flate2, clap, quick-xml (via mjlog)

---

## Batch 1: Vendor Crates & Workspace Setup

**Goal:** Set up workspace with all vendored crates compiling together.

### Task 1.1: Vendor mjlog crate

**Files:**
- Create: `crates/mjlog/` (copy from `mjlog2json/mjlog/`)
- Modify: `crates/mjlog/Cargo.toml`

**Step 1: Copy and modify**
```bash
cp -r mjlog2json/mjlog crates/
```

**Step 2: Update Cargo.toml**
```toml
[package]
name = "mjlog"
version = "0.1.3"
edition = "2021"

[dependencies]
num-derive = "0.4"
num-traits = "0.2"
percent-encoding = "2.3"
quick-xml = "0.37"
serde = { version = "1", features = ["derive"] }
thiserror = "2"
```

**Step 3: Verify compiles**
Run: `cd ~/dev/tenhou-to-mjai && cargo check -p mjlog`
Expected: Compiling mjlog... Finished

**Step 4: Commit**
```bash
git add crates/mjlog
git commit -m "chore: vendor mjlog crate for XML parsing"
```

### Task 1.2: Vendor tenhou-json crate

**Files:**
- Create: `crates/tenhou-json/` (copy from `mjlog2json/tenhou-json/`)
- Modify: `crates/tenhou-json/Cargo.toml`

**Step 1: Copy and modify**
```bash
cp -r mjlog2json/tenhou-json crates/
```

**Step 2: Update Cargo.toml**
```toml
[package]
name = "tenhou-json"
version = "0.1.2"
edition = "2021"

[dependencies]
num-derive = "0.4"
num-traits = "0.2"
serde = { version = "1", features = ["derive"] }
serde_json = { version = "1", features = ["preserve_order"] }
thiserror = "2"
```

**Step 3: Verify compiles**
Run: `cargo check -p tenhou-json`
Expected: Compiling tenhou-json... Finished

**Step 4: Commit**
```bash
git add crates/tenhou-json
git commit -m "chore: vendor tenhou-json crate for JSON models"
```

### Task 1.3: Vendor mjlog2json-core crate

**Files:**
- Create: `crates/mjlog2json-core/` (copy from `mjlog2json/mjlog2json-core/`)
- Modify: `crates/mjlog2json-core/Cargo.toml`

**Step 1: Copy and modify**
```bash
cp -r mjlog2json/mjlog2json-core crates/
```

**Step 2: Update Cargo.toml with path deps**
```toml
[package]
name = "mjlog2json-core"
version = "0.1.3"
edition = "2021"

[dependencies]
mjlog = { path = "../mjlog" }
tenhou-json = { path = "../tenhou-json" }
thiserror = "2"
```

**Step 3: Verify compiles**
Run: `cargo check -p mjlog2json-core`
Expected: Compiling mjlog2json-core... Finished

**Step 4: Commit**
```bash
git add crates/mjlog2json-core
git commit -m "chore: vendor mjlog2json-core crate for XML to JSON conversion"
```

### Task 1.4: Update workspace Cargo.toml

**Files:**
- Modify: `Cargo.toml`

**Step 1: Update root Cargo.toml**
```toml
[package]
name = "tenhou-scraper"
version = "0.1.0"
edition = "2021"

[dependencies]
tokio = { version = "1", features = ["full"] }
reqwest = { version = "0.12", features = ["gzip", "json"] }
rusqlite = { version = "0.32", features = ["bundled"] }
flate2 = "1"
clap = { version = "4", features = ["derive"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
anyhow = "1"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
indicatif = "0.17"
regex = "1"
chrono = "0.4"

# Vendored crates
mjlog = { path = "crates/mjlog" }
mjlog2json-core = { path = "crates/mjlog2json-core" }
convlog = { path = "crates/convlog" }

[workspace]
members = [
    ".",
    "crates/mjlog",
    "crates/tenhou-json",
    "crates/mjlog2json-core",
    "crates/convlog",
]
```

**Step 2: Verify full workspace compiles**
Run: `cargo check --workspace`
Expected: Finished dev [unoptimized + debuginfo]

**Step 3: Commit**
```bash
git add Cargo.toml Cargo.lock
git commit -m "chore: configure workspace with all vendored crates"
```

---

## Batch 2: Database Module

**Goal:** Implement SQLite database for tracking log IDs and download state.

### Task 2.1: Create database schema module

**Files:**
- Create: `src/db.rs`
- Modify: `src/main.rs` (add mod)

**Step 1: Write db module**
Create `src/db.rs`:
```rust
use anyhow::Result;
use rusqlite::{Connection, params};
use std::path::Path;

pub struct Database {
    conn: Connection,
}

#[derive(Debug, Clone)]
pub struct LogEntry {
    pub id: String,
    pub date: String,
    pub num_players: i32,
    pub is_hanchan: bool,
    pub is_downloaded: bool,
    pub is_converted: bool,
    pub xml_data: Option<Vec<u8>>,
}

impl Database {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        let db = Self { conn };
        db.init_schema()?;
        Ok(db)
    }

    fn init_schema(&self) -> Result<()> {
        self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS logs (
                id TEXT PRIMARY KEY,
                date TEXT NOT NULL,
                num_players INTEGER NOT NULL,
                is_hanchan INTEGER NOT NULL,
                is_downloaded INTEGER NOT NULL DEFAULT 0,
                is_converted INTEGER NOT NULL DEFAULT 0,
                xml_data BLOB
            );

            CREATE TABLE IF NOT EXISTS fetch_state (
                date TEXT PRIMARY KEY,
                fetched_at TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_logs_downloaded ON logs(is_downloaded);
            CREATE INDEX IF NOT EXISTS idx_logs_converted ON logs(is_converted);
            "
        )?;
        Ok(())
    }

    pub fn insert_log_id(&self, entry: &LogEntry) -> Result<bool> {
        let result = self.conn.execute(
            "INSERT OR IGNORE INTO logs (id, date, num_players, is_hanchan, is_downloaded, is_converted)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                entry.id,
                entry.date,
                entry.num_players,
                entry.is_hanchan as i32,
                entry.is_downloaded as i32,
                entry.is_converted as i32,
            ],
        )?;
        Ok(result > 0)
    }

    pub fn get_undownloaded_ids(&self, limit: Option<usize>) -> Result<Vec<String>> {
        let mut stmt = match limit {
            Some(n) => self.conn.prepare(
                "SELECT id FROM logs WHERE is_downloaded = 0 ORDER BY id LIMIT ?1"
            )?,
            None => self.conn.prepare(
                "SELECT id FROM logs WHERE is_downloaded = 0 ORDER BY id"
            )?,
        };

        let rows = match limit {
            Some(n) => stmt.query_map([n], |row| row.get(0))?,
            None => stmt.query_map([], |row| row.get(0))?,
        };

        let mut ids = Vec::new();
        for id in rows {
            ids.push(id?);
        }
        Ok(ids)
    }

    pub fn mark_downloaded(&self, id: &str, xml_data: &[u8]) -> Result<()> {
        self.conn.execute(
            "UPDATE logs SET is_downloaded = 1, xml_data = ?1 WHERE id = ?2",
            params![xml_data, id],
        )?;
        Ok(())
    }

    pub fn mark_download_error(&self, id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE logs SET is_downloaded = -1 WHERE id = ?1",
            params![id],
        )?;
        Ok(())
    }

    pub fn get_unconverted_logs(&self, limit: Option<usize>) -> Result<Vec<(String, Vec<u8>)>> {
        let mut stmt = match limit {
            Some(n) => self.conn.prepare(
                "SELECT id, xml_data FROM logs
                 WHERE is_downloaded = 1 AND is_converted = 0 AND xml_data IS NOT NULL
                 ORDER BY id LIMIT ?1"
            )?,
            None => self.conn.prepare(
                "SELECT id, xml_data FROM logs
                 WHERE is_downloaded = 1 AND is_converted = 0 AND xml_data IS NOT NULL
                 ORDER BY id"
            )?,
        };

        let rows = match limit {
            Some(n) => stmt.query_map([n], |row| Ok((row.get(0)?, row.get(1)?)))?,
            None => stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?,
        };

        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    }

    pub fn mark_converted(&self, id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE logs SET is_converted = 1 WHERE id = ?1",
            params![id],
        )?;
        Ok(())
    }

    pub fn mark_date_fetched(&self, date: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO fetch_state (date, fetched_at) VALUES (?1, datetime('now'))",
            params![date],
        )?;
        Ok(())
    }

    pub fn is_date_fetched(&self, date: &str) -> Result<bool> {
        let count: i32 = self.conn.query_row(
            "SELECT COUNT(*) FROM fetch_state WHERE date = ?1",
            params![date],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    pub fn count_logs(&self) -> Result<(i64, i64, i64)> {
        let total: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM logs", [], |row| row.get(0)
        )?;
        let downloaded: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM logs WHERE is_downloaded = 1", [], |row| row.get(0)
        )?;
        let converted: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM logs WHERE is_converted = 1", [], |row| row.get(0)
        )?;
        Ok((total, downloaded, converted))
    }
}
```

**Step 2: Verify compiles**
Run: `cargo check`
Expected: Finished

**Step 3: Commit**
```bash
git add src/db.rs
git commit -m "feat: add database module for log tracking"
```

---

## Batch 3: Fetch Command

**Goal:** Implement fetching log IDs from Tenhou daily HTML files.

### Task 3.1: Create fetch module

**Files:**
- Create: `src/fetch.rs`

**Step 1: Write fetch module**
Create `src/fetch.rs`:
```rust
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
                let url = format!("{}/{}/{}{}.html.gz", TENHOU_BASE_URL, year, log_type, date_str);

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
                            log_type, date_str, entries.len(), new_count
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
        decoder.read_to_string(&mut html)
            .context("Failed to decompress gzip")?;

        self.parse_log_ids(&html)
    }

    fn parse_log_ids(&self, html: &str) -> Result<Vec<LogEntry>> {
        // Pattern: log=2025010100gm-00a9-0000-d7141b66
        let log_id_re = Regex::new(r"log=(\d{10}gm-([0-9a-f]{4})-[0-9a-f]{4}-[0-9a-f]{8})")?;

        let mut entries = Vec::new();

        for cap in log_id_re.captures_iter(html) {
            let full_id = cap.get(1).unwrap().as_str();
            let game_type = cap.get(2).unwrap().as_str();

            // Parse game type code
            // 00a9 = 4-player hanchan with red
            // 0029 = 4-player hanchan without red
            // 00b9 = 3-player hanchan with red
            // 00e1 = 4-player tonpu fast
            let (num_players, is_hanchan) = match game_type {
                "00a9" | "0029" => (4, true),   // 4p hanchan
                "00b9" | "0039" => (3, true),   // 3p hanchan
                "00e1" | "0061" => (4, false),  // 4p tonpu
                "00f1" | "0071" => (3, false),  // 3p tonpu
                _ => continue, // skip unknown types
            };

            // Extract date from log ID (first 8 chars after skipping time)
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
```

**Step 2: Verify compiles**
Run: `cargo check`
Expected: Finished

**Step 3: Commit**
```bash
git add src/fetch.rs
git commit -m "feat: add fetch module for scraping log IDs"
```

---

## Batch 4: Download Command

**Goal:** Implement downloading XML logs from Tenhou.

### Task 4.1: Create download module

**Files:**
- Create: `src/download.rs`

**Step 1: Write download module**
Create `src/download.rs`:
```rust
use anyhow::{Context, Result};
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

    pub async fn download_logs(
        &self,
        db: &Database,
        limit: Option<usize>,
    ) -> Result<(usize, usize)> {
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
```

**Step 2: Verify compiles**
Run: `cargo check`
Expected: Finished

**Step 3: Commit**
```bash
git add src/download.rs
git commit -m "feat: add download module for fetching XML logs"
```

---

## Batch 5: Convert Command

**Goal:** Implement conversion from XML to MJAI format.

### Task 5.1: Create convert module

**Files:**
- Create: `src/convert.rs`

**Step 1: Write convert module**
Create `src/convert.rs`:
```rust
use anyhow::{Context, Result};
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use indicatif::{ProgressBar, ProgressStyle};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::Path;
use tracing::{info, warn};

use crate::db::Database;

pub struct Converter {
    output_dir: std::path::PathBuf,
}

impl Converter {
    pub fn new(output_dir: impl AsRef<Path>) -> Result<Self> {
        let output_dir = output_dir.as_ref().to_path_buf();
        fs::create_dir_all(&output_dir)?;
        Ok(Self { output_dir })
    }

    pub fn convert_logs(&self, db: &Database, limit: Option<usize>) -> Result<(usize, usize)> {
        let logs = db.get_unconverted_logs(limit)?;

        if logs.is_empty() {
            info!("No logs to convert");
            return Ok((0, 0));
        }

        info!("Converting {} logs", logs.len());

        let pb = ProgressBar::new(logs.len() as u64);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({eta})")?
                .progress_chars("#>-"),
        );

        let mut success = 0;
        let mut failed = 0;

        for (id, compressed_xml) in logs {
            match self.convert_single(&id, &compressed_xml) {
                Ok(_) => {
                    db.mark_converted(&id)?;
                    success += 1;
                }
                Err(e) => {
                    warn!("Failed to convert {}: {}", id, e);
                    failed += 1;
                }
            }
            pb.inc(1);
        }

        pb.finish_with_message("Done");
        Ok((success, failed))
    }

    fn convert_single(&self, id: &str, compressed_xml: &[u8]) -> Result<()> {
        // Decompress XML
        let mut decoder = GzDecoder::new(compressed_xml);
        let mut xml_str = String::new();
        decoder.read_to_string(&mut xml_str)
            .context("Failed to decompress XML")?;

        // Parse XML with mjlog
        let mjlogs = mjlog::parser::parse_mjlogs(&xml_str)
            .context("Failed to parse mjlog XML")?;

        if mjlogs.is_empty() {
            anyhow::bail!("No games found in mjlog");
        }

        // Convert to tenhou JSON (take first game)
        let tenhou_json = mjlog2json_core::conv::conv_to_tenhou_json(&mjlogs[0])
            .context("Failed to convert to tenhou JSON")?;

        // Serialize to JSON string
        let json_str = serde_json::to_string(&tenhou_json)
            .context("Failed to serialize tenhou JSON")?;

        // Parse with convlog
        let log = convlog::tenhou::Log::from_json_str(&json_str)
            .context("Failed to parse with convlog")?;

        // Convert to MJAI events
        let events = convlog::tenhou_to_mjai(&log)
            .context("Failed to convert to MJAI")?;

        // Write gzipped MJAI output
        let output_path = self.output_dir.join(format!("{}.mjson", id));
        let file = File::create(&output_path)?;
        let mut encoder = GzEncoder::new(file, Compression::default());

        for event in events {
            let line = serde_json::to_string(&event)?;
            writeln!(encoder, "{}", line)?;
        }

        encoder.finish()?;
        Ok(())
    }
}
```

**Step 2: Verify compiles**
Run: `cargo check`
Expected: Finished

**Step 3: Commit**
```bash
git add src/convert.rs
git commit -m "feat: add convert module for XML to MJAI conversion"
```

---

## Batch 6: CLI Integration

**Goal:** Wire up all modules into a unified CLI.

### Task 6.1: Update main.rs with CLI

**Files:**
- Modify: `src/main.rs`

**Step 1: Write main.rs**
Replace `src/main.rs`:
```rust
mod db;
mod download;
mod fetch;
mod convert;

use anyhow::Result;
use chrono::NaiveDate;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use tracing::info;

#[derive(Parser)]
#[command(name = "tenhou-scraper")]
#[command(about = "Scrape Tenhou houou logs and convert to MJAI format")]
struct Cli {
    /// Database file path
    #[arg(short, long, default_value = "tenhou.db")]
    database: PathBuf,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Fetch log IDs from Tenhou
    Fetch {
        /// Start date (YYYYMMDD)
        #[arg(short, long)]
        start: String,

        /// End date (YYYYMMDD), defaults to today
        #[arg(short, long)]
        end: Option<String>,

        /// Log types to fetch (comma-separated: scc=houou)
        #[arg(short = 't', long, default_value = "scc")]
        log_types: String,

        /// Delay between requests in ms
        #[arg(long, default_value = "200")]
        delay_ms: u64,

        /// Skip already fetched dates
        #[arg(long, default_value = "true")]
        skip_fetched: bool,
    },

    /// Download XML log content
    Download {
        /// Maximum logs to download (default: all)
        #[arg(short, long)]
        limit: Option<usize>,

        /// Delay between requests in ms
        #[arg(long, default_value = "200")]
        delay_ms: u64,
    },

    /// Convert downloaded logs to MJAI format
    Convert {
        /// Output directory for MJAI files
        #[arg(short, long, default_value = "mjai")]
        output: PathBuf,

        /// Maximum logs to convert (default: all)
        #[arg(short, long)]
        limit: Option<usize>,
    },

    /// Show database statistics
    Stats,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("tenhou_scraper=info".parse()?),
        )
        .init();

    let cli = Cli::parse();
    let db = db::Database::open(&cli.database)?;

    match cli.command {
        Commands::Fetch {
            start,
            end,
            log_types,
            delay_ms,
            skip_fetched,
        } => {
            let start_date = NaiveDate::parse_from_str(&start, "%Y%m%d")?;
            let end_date = match end {
                Some(e) => NaiveDate::parse_from_str(&e, "%Y%m%d")?,
                None => chrono::Local::now().date_naive(),
            };

            let log_types: Vec<&str> = log_types.split(',').collect();

            let fetcher = fetch::Fetcher::new(delay_ms)?;
            let new_count = fetcher
                .fetch_date_range(&db, start_date, end_date, &log_types, skip_fetched)
                .await?;

            info!("Fetched {} new log IDs", new_count);
        }

        Commands::Download { limit, delay_ms } => {
            let downloader = download::Downloader::new(delay_ms)?;
            let (success, failed) = downloader.download_logs(&db, limit).await?;
            info!("Downloaded {} logs ({} failed)", success, failed);
        }

        Commands::Convert { output, limit } => {
            let converter = convert::Converter::new(&output)?;
            let (success, failed) = converter.convert_logs(&db, limit)?;
            info!("Converted {} logs ({} failed)", success, failed);
        }

        Commands::Stats => {
            let (total, downloaded, converted) = db.count_logs()?;
            println!("Database: {}", cli.database.display());
            println!("Total log IDs:    {}", total);
            println!("Downloaded:       {}", downloaded);
            println!("Converted:        {}", converted);
            println!("Pending download: {}", total - downloaded);
            println!("Pending convert:  {}", downloaded - converted);
        }
    }

    Ok(())
}
```

**Step 2: Verify compiles**
Run: `cargo build --release`
Expected: Finished release [optimized]

**Step 3: Test CLI help**
Run: `./target/release/tenhou-scraper --help`
Expected: Shows subcommands (fetch, download, convert, stats)

**Step 4: Commit**
```bash
git add src/main.rs
git commit -m "feat: wire up CLI with fetch, download, convert commands"
```

---

## Batch 7: Integration Test

**Goal:** Verify the full pipeline works end-to-end.

### Task 7.1: Test fetch command

**Step 1: Fetch one day of logs**
Run:
```bash
./target/release/tenhou-scraper -d test.db fetch --start 20250101 --end 20250101
```
Expected: "Fetched scc 20250101 - N total, N new"

**Step 2: Check stats**
Run:
```bash
./target/release/tenhou-scraper -d test.db stats
```
Expected: Shows total log IDs > 0

### Task 7.2: Test download command

**Step 1: Download a few logs**
Run:
```bash
./target/release/tenhou-scraper -d test.db download --limit 5
```
Expected: Progress bar, "Downloaded 5 logs"

### Task 7.3: Test convert command

**Step 1: Convert downloaded logs**
Run:
```bash
./target/release/tenhou-scraper -d test.db convert --output test_mjai --limit 5
```
Expected: Progress bar, "Converted 5 logs"

**Step 2: Verify output files**
Run:
```bash
ls test_mjai/*.mjson | head -3
gunzip -c test_mjai/*.mjson | head -5
```
Expected: Files exist, valid JSON lines with MJAI events

**Step 3: Commit test verification**
```bash
git add -A
git commit -m "test: verify full pipeline works end-to-end"
```

---

## Cleanup After Plan Execution

```bash
# Remove cloned reference repos (no longer needed)
rm -rf mjlog2json houou-logs test_output test.db test_mjai

# Clean up any temp files
cargo clean

git add -A
git commit -m "chore: cleanup reference repos after vendoring"
```

---

## Summary

| Batch | Tasks | Focus |
|-------|-------|-------|
| 1 | 4 | Vendor crates, workspace setup |
| 2 | 1 | Database module |
| 3 | 1 | Fetch command |
| 4 | 1 | Download command |
| 5 | 1 | Convert command |
| 6 | 1 | CLI integration |
| 7 | 3 | Integration testing |

**Total: 12 tasks across 7 batches**

After completion, run full scrape:
```bash
# Fetch all 2025-2026 logs
./target/release/tenhou-scraper fetch --start 20250101 --end 20260131

# Download all
./target/release/tenhou-scraper download

# Convert all
./target/release/tenhou-scraper convert --output mjai/
```
