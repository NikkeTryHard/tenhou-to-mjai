mod convert;
mod db;
mod download;
mod export;
mod fetch;
mod majsoul;
mod package;
mod util;

use anyhow::Result;
use chrono::NaiveDate;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use tracing::info;

fn parse_nonzero_usize(s: &str) -> Result<usize, String> {
    let v: usize = s.parse().map_err(|e| format!("invalid number: {e}"))?;
    if v == 0 {
        Err("must be at least 1".to_string())
    } else {
        Ok(v)
    }
}

#[derive(clap::ValueEnum, Clone, Debug)]
enum Room {
    Throne,
    Jade,
    Gold,
    Silver,
    Bronze,
    All,
}

impl Room {
    fn room_type_u32(&self) -> u32 {
        match self {
            Room::Throne => 5,
            Room::Jade => 4,
            Room::Gold => 3,
            Room::Silver => 2,
            Room::Bronze => 1,
            Room::All => 0,
        }
    }

    fn mode_id(&self) -> Option<i32> {
        match self {
            Room::Throne => Some(16),
            Room::Jade => Some(12),
            Room::Gold => Some(9),
            _ => None,
        }
    }

    fn as_str(&self) -> &'static str {
        match self {
            Room::Throne => "throne",
            Room::Jade => "jade",
            Room::Gold => "gold",
            Room::Silver => "silver",
            Room::Bronze => "bronze",
            Room::All => "all",
        }
    }
}
impl std::fmt::Display for Room {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(clap::ValueEnum, Clone, Debug)]
enum Server {
    En,
    Jp,
    Cn,
}

impl Server {
    fn as_str(&self) -> &'static str {
        match self {
            Server::En => "en",
            Server::Jp => "jp",
            Server::Cn => "cn",
        }
    }

    fn origin(&self) -> &'static str {
        match self {
            Server::Cn => "https://game.maj-soul.com",
            Server::En | Server::Jp => "https://mahjongsoul.game.yo-star.com",
        }
    }
}
impl std::fmt::Display for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

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

fn open_db(cli: &Cli) -> Result<db::Database> {
    db::Database::open(&cli.database)
}

/// Parse `YYYYMMDD` start/end dates shared by the fetch arms.
/// `end=None` defaults to today (`default_today`) or yesterday
/// (`FetchDays`: today's day-file is still accumulating and marking it
/// fetched would lock in a partial day).
fn parse_date_range(start: &str, end: Option<String>, default_today: bool) -> Result<(NaiveDate, NaiveDate)> {
    let start_date = NaiveDate::parse_from_str(start, "%Y%m%d")?;
    let end_date = match end {
        Some(e) => NaiveDate::parse_from_str(&e, "%Y%m%d")?,
        None if default_today => chrono::Local::now().date_naive(),
        None => chrono::Local::now().date_naive() - chrono::Duration::days(1),
    };
    if start_date > end_date {
        anyhow::bail!("start ({start_date}) is after end ({end_date})");
    }
    Ok((start_date, end_date))
}

/// GET with bounded retries for the `ScrapeAll` arm (module level so no items follow statements).
async fn fetch_with_retry(
    client: &reqwest::Client,
    url: &str,
    max_retries: u32,
) -> Result<Vec<majsoul::GameRecord>> {
    let mut attempt = 0;
    loop {
        attempt += 1;
        match client.get(url).send().await {
            Ok(resp) => {
                if resp.status().is_success() {
                    match resp.json::<Vec<majsoul::GameRecord>>().await {
                        Ok(records) => return Ok(records),
                        Err(e) => {
                            if attempt >= max_retries {
                                anyhow::bail!("JSON parse failed after {attempt} attempts: {e}");
                            }
                            tracing::warn!("Parse error (attempt {}): {}", attempt, e);
                        }
                    }
                } else if resp.status() == 429 {
                    tracing::warn!("Rate limited (429), waiting 30s...");
                    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                } else if resp.status().is_server_error() {
                    if attempt >= max_retries {
                        anyhow::bail!("Server error {} after {} attempts", resp.status(), attempt);
                    }
                    tracing::warn!("Server error {} (attempt {})", resp.status(), attempt);
                } else {
                    anyhow::bail!("HTTP {}", resp.status());
                }
            }
            Err(e) => {
                if attempt >= max_retries {
                    anyhow::bail!("Network error after {attempt} attempts: {e}");
                }
                tracing::warn!("Network error (attempt {}): {}", attempt, e);
            }
        }
        // 429 waits 30s (rate-limit window), transient errors back off 1/2/4/8/16s capped, failed scrape-all rounds pause 5s — stay under ~4 rps or risk a ban.
        let backoff = std::time::Duration::from_secs(1 << (attempt - 1).min(4));
        tokio::time::sleep(backoff).await;
    }
}

#[derive(Subcommand, Clone)]
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
        /// Number of concurrent date fetches (default: 1) (Tenhou allows max 1 session; values > 1 risk a ban)
        #[arg(short, long, default_value_t = 1, value_parser = parse_nonzero_usize)]
        concurrent: usize,
        /// Skip already fetched dates (pass --skip-fetched=false to re-fetch)
        #[arg(long, action = clap::ArgAction::Set, default_value_t = true, value_parser = clap::value_parser!(bool), value_name = "BOOL")]
        skip_fetched: bool,
        /// Import log IDs from a directory of *.html.gz files (no network)
        #[arg(long)]
        import_dir: Option<PathBuf>,
    },
    /// Download XML log content
    Download {
        /// Maximum logs to download (default: all)
        #[arg(short, long, value_parser = parse_nonzero_usize)]
        limit: Option<usize>,
        /// Delay between requests in ms
        #[arg(long, default_value = "200")]
        delay_ms: u64,
        /// Number of concurrent downloads (default: 1) (Tenhou allows max 1 session; values > 1 risk a ban)
        #[arg(short, long, default_value_t = 1, value_parser = parse_nonzero_usize)]
        concurrent: usize,
        /// Retry previously failed downloads (reset error flags and attempts)
        #[arg(long, action = clap::ArgAction::SetTrue)]
        retry_errors: bool,
    },
    /// Convert downloaded logs to MJAI format
    Convert {
        /// Output directory for MJAI files
        #[arg(short, long, default_value = "mjai")]
        output: PathBuf,
        /// Maximum logs to convert (page size, default: 500 per page)
        #[arg(short, long, value_parser = parse_nonzero_usize)]
        limit: Option<usize>,
        /// Filter by player count (e.g., 4 for 4-player games)
        #[arg(short, long, value_parser = clap::value_parser!(i32).range(3..=4))]
        players: Option<i32>,
        /// Only convert hanchan (full games)
        #[arg(long)]
        hanchan: bool,
        /// Retry previously failed conversions (reset error flags and attempts)
        #[arg(long, action = clap::ArgAction::SetTrue)]
        retry_errors: bool,
    },

    /// Show database statistics
    Stats,

    /// Export XML logs from database to files
    Export {
        /// Output directory for XML files
        #[arg(short, long, default_value = "xml")]
        output: PathBuf,
        /// Maximum logs to export (default: all)
        #[arg(short, long, value_parser = parse_nonzero_usize)]
        limit: Option<usize>,
    },
    /// Package MJAI files into a zip archive
    Package {
        /// Input directory containing .mjson.gz files
        #[arg(short, long)]
        input: PathBuf,
        /// Output zip file path
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Mahjong Soul (Majsoul) operations
    #[command(subcommand)]
    Majsoul(MajsoulCommands),
}
#[derive(Subcommand, Clone)]
enum MajsoulCommands {
    /// Search for a player by nickname
    Search {
        /// Player nickname to search
        nickname: String,
    },
    /// Fetch game UUIDs for a player
    Fetch {
        /// Player ID from amae-koromo
        #[arg(long)]
        player_id: i64,
        /// Room mode (9-26, 16=Throne, 12=Jade, 9=Gold)
        #[arg(long, default_value_t = 16, value_parser = clap::value_parser!(i32).range(9..=26))]
        mode: i32,
        /// Start date (YYYYMMDD)
        #[arg(long)]
        start: String,
        /// End date (YYYYMMDD)
        #[arg(long)]
        end: Option<String>,
        /// Delay between requests in ms
        #[arg(long, default_value = "300")]
        delay_ms: u64,
    },

    /// Show Majsoul stats
    Stats,

    /// Fetch public game UUIDs from ranked rooms (Throne, Jade, Gold)
    /// Note: This command requires authentication. Use --username and --password (or `MAJSOUL_PASSWORD` env).
    FetchPublic {
        /// Room type
        #[arg(long, value_enum, default_value_t = Room::Throne)]
        room: Room,
        /// Number of games to fetch (1-1000)
        #[arg(short, long, default_value_t = 100, value_parser = clap::value_parser!(u32).range(1..=1000))]
        count: u32,
        /// Server region
        #[arg(long, value_enum, default_value_t = Server::En)]
        server: Server,
        /// Username for native login (required)
        #[arg(long)]
        username: String,
        /// Password for native login (required, or `MAJSOUL_PASSWORD` env)
        #[arg(long, env = "MAJSOUL_PASSWORD", hide_env_values = true)]
        password: String,
    },
    /// Download game records using native login (username/password)
    Download {
        /// Maximum records to download
        #[arg(short, long, value_parser = parse_nonzero_usize)]
        limit: Option<usize>,
        /// Delay between requests in ms
        #[arg(long, default_value = "1500")]
        delay_ms: u64,
        /// Server region
        #[arg(long, value_enum, default_value_t = Server::En)]
        server: Server,
        /// Username for native login (required)
        #[arg(long)]
        username: String,
        /// Password for native login (required, or `MAJSOUL_PASSWORD` env)
        #[arg(long, env = "MAJSOUL_PASSWORD", hide_env_values = true)]
        password: String,
        /// Retry previously failed downloads (reset error flags and attempts)
        #[arg(long, action = clap::ArgAction::SetTrue)]
        retry_errors: bool,
    },
    /// Convert downloaded Majsoul logs to MJAI format
    Convert {
        /// Output directory for MJAI files
        #[arg(short, long, default_value = "mjai-majsoul")]
        output: PathBuf,
        /// Maximum logs to convert (page size, default: 500 per page)
        #[arg(short, long, value_parser = parse_nonzero_usize)]
        limit: Option<usize>,
        /// Filter by player count (e.g., 4 for 4-player games)
        #[arg(short, long, value_parser = clap::value_parser!(i32).range(3..=4))]
        players: Option<i32>,
        /// Only convert hanchan (full games)
        #[arg(long)]
        hanchan: bool,
        /// Retry previously failed conversions (reset error flags and attempts)
        #[arg(long, action = clap::ArgAction::SetTrue)]
        retry_errors: bool,
    },
    /// Fetch game UUIDs from ranked rooms (no player ID needed)
    FetchRoom {
        /// Room type
        #[arg(long, value_enum, default_value_t = Room::Throne)]
        room: Room,
        /// Start date (YYYYMMDD)
        #[arg(long)]
        start: String,
        /// End date (YYYYMMDD), defaults to today
        #[arg(long)]
        end: Option<String>,
        /// Delay between API requests in ms
        #[arg(long, default_value = "1000")]
        delay_ms: u64,
        /// Skip dates already fetched (pass --skip-fetched=false to re-fetch)
        #[arg(long, action = clap::ArgAction::Set, default_value_t = true, value_parser = clap::value_parser!(bool), value_name = "BOOL")]
        skip_fetched: bool,
    },

    /// Resolve short UUIDs to full paipu URLs via Amae-Koromo
    ResolvePaipu {
        /// Maximum UUIDs to resolve (default: all unresolved)
        #[arg(short, long, value_parser = parse_nonzero_usize)]
        limit: Option<usize>,
        /// Delay between API requests in ms
        #[arg(long, default_value = "300")]
        delay_ms: u64,
    },
    /// Export resolved paipu URLs to file
    ExportPaipu {
        /// Output file path
        #[arg(short, long, default_value = "paipu_urls.txt")]
        output: PathBuf,
    },
    /// Fetch full UUIDs by querying player records (parallel fetch, sequential write)
    FetchFullUuids {
        /// Number of concurrent API requests
        #[arg(short, long, default_value_t = 10, value_parser = parse_nonzero_usize)]
        concurrent: usize,
        /// Maximum players to process
        #[arg(short, long, value_parser = parse_nonzero_usize)]
        limit: Option<usize>,
        /// Delay between batches in ms
        #[arg(long, default_value = "100")]
        delay_ms: u64,
    },
    /// Recover orphaned games by re-fetching players with pagination
    RecoverOrphans {
        /// Number of concurrent API requests
        #[arg(short, long, default_value_t = 5, value_parser = parse_nonzero_usize)]
        concurrent: usize,
        /// Maximum players to process
        #[arg(short, long, value_parser = parse_nonzero_usize)]
        limit: Option<usize>,
        /// Delay between batches in ms
        #[arg(long, default_value = "200")]
        delay_ms: u64,
    },
    /// Resolve short UUIDs to full UUIDs via Majsoul RPC
    /// Note: This command requires authentication. Use --username and --password (or `MAJSOUL_PASSWORD` env).
    ResolveUuids {
        /// Maximum UUIDs to resolve
        #[arg(short, long, value_parser = parse_nonzero_usize)]
        limit: Option<usize>,
        /// Concurrent RPC requests (default: 4)
        #[arg(short, long, default_value_t = 4, value_parser = parse_nonzero_usize)]
        concurrent: usize,
        /// Delay between request batches in ms
        #[arg(long, default_value = "200")]
        delay_ms: u64,
        /// Server region
        #[arg(long, value_enum, default_value_t = Server::En)]
        server: Server,
        /// Username for native login (required)
        #[arg(long)]
        username: String,
        /// Password for native login (required, or `MAJSOUL_PASSWORD` env)
        #[arg(long, env = "MAJSOUL_PASSWORD", hide_env_values = true)]
        password: String,
    },
    /// Exhaustive scrape: fetch ALL Throne games (runs until no new games found)
    ScrapeAll {
        /// Requests per second (1-20, stay under 5 to be safe)
        #[arg(long, default_value_t = 4, value_parser = clap::value_parser!(u32).range(1..=20))]
        rps: u32,
        /// Start date for date fetcher (YYYYMMDD)
        #[arg(long, default_value = "20190801")]
        start: String,
    },
    /// Reset fetch status for players who hit the 200-game cap
    ResetCappedPlayers,
    /// Bulk download with native login (username/password)
    BulkDownload {
        /// Maximum records to download
        #[arg(short, long, value_parser = parse_nonzero_usize)]
        limit: Option<usize>,
        /// Delay between requests in ms
        #[arg(long, default_value = "2000")]
        delay_ms: u64,
        /// Restart RPC connection every N records (prevents memory leaks)
        #[arg(long, default_value = "10000")]
        restart_every: usize,

        /// Server region
        #[arg(long, value_enum, default_value_t = Server::En)]
        server: Server,
        /// Username for native login (required)
        #[arg(long)]
        username: String,
        /// Password for native login (required, or `MAJSOUL_PASSWORD` env)
        #[arg(long, env = "MAJSOUL_PASSWORD", hide_env_values = true)]
        password: String,
    },
    /// Resolve phantom UUIDs via browser injection
    ResolvePhantoms {
        /// Maximum UUIDs to resolve
        #[arg(short, long, value_parser = parse_nonzero_usize)]
        limit: Option<usize>,
        /// Delay between requests in ms
        #[arg(long, default_value = "2000")]
        delay_ms: u64,
        /// Server region
        #[arg(long, value_enum, default_value_t = Server::En)]
        server: Server,
    },
    /// Download games and convert to Tenhou JSON format
    DownloadJson {
        /// Output directory for JSON files
        #[arg(short, long, default_value = "tenhou-json")]
        output: PathBuf,
        /// Maximum records to download
        #[arg(short, long, value_parser = parse_nonzero_usize)]
        limit: Option<usize>,
        /// Username for native login (required)
        #[arg(long)]
        username: String,
        /// Password for native login (required, or `MAJSOUL_PASSWORD` env)
        #[arg(long, env = "MAJSOUL_PASSWORD", hide_env_values = true)]
        password: String,
        /// Delay between requests in ms
        #[arg(long, default_value = "2000")]
        delay_ms: u64,
        /// Server region
        #[arg(long, value_enum, default_value_t = Server::En)]
        server: Server,
    },
    /// Multi-account raw protobuf download (saves .pb files, no conversion)
    RawDownload {
        /// File with account emails (one per line)
        #[arg(short, long, default_value = "accounts.txt")]
        accounts: PathBuf,
        /// Shared password for all accounts (or `MAJSOUL_PASSWORD` env)
        #[arg(long, env = "MAJSOUL_PASSWORD", hide_env_values = true)]
        password: String,
        /// File with all UUIDs to download (one per line)
        #[arg(long, default_value = "todo.txt")]
        todo: PathBuf,
        /// Append-only log of completed UUIDs
        #[arg(long, default_value = "completed.log")]
        completed: PathBuf,
        /// Output directory for .pb files
        #[arg(short, long, default_value = "jade-raw")]
        output: PathBuf,
        /// Server region (defaults to cn: raw downloads target the CN Jade room)
        // Raw-download accounts and Jade-room history live on the CN gateway; other commands default to EN where the login accounts were created.
        #[arg(long, value_enum, default_value_t = Server::Cn)]
        server: Server,
        /// Maximum games to download
        #[arg(short, long, value_parser = parse_nonzero_usize)]
        limit: Option<usize>,
        /// Delay between requests per worker in ms
        #[arg(long, default_value = "300")]
        delay_ms: u64,
    },
    /// Convert raw .pb files to MJAI format (no database needed)
    ConvertRaw {
        /// Input directory containing .pb files
        #[arg(short, long, default_value = "jade-raw")]
        input: PathBuf,
        /// Output directory for .mjai.json files
        #[arg(short, long, default_value = "jade-mjai")]
        output: PathBuf,
        /// Delete .pb files after successful conversion
        #[arg(long, action = clap::ArgAction::SetTrue)]
        delete: bool,
    },
    /// Phase 1: Fetch all player IDs by day (fast, parallel-safe)
    FetchDays {
        /// Start date (YYYYMMDD)
        #[arg(long)]
        start: String,
        /// End date (YYYYMMDD), defaults to yesterday
        #[arg(long)]
        end: Option<String>,
        /// Delay between API requests in ms
        #[arg(long, default_value = "100")]
        delay_ms: u64,
    },
    /// Phase 2: Scrape full game history for unscraped players (slow, resumable)
    ScrapePlayers {
        /// Maximum players to scrape
        #[arg(short, long, value_parser = parse_nonzero_usize)]
        limit: Option<usize>,
        /// Number of concurrent player fetches
        #[arg(short, long, default_value_t = 5, value_parser = parse_nonzero_usize)]
        concurrent: usize,
        /// Delay between API requests in ms
        #[arg(long, default_value = "200")]
        delay_ms: u64,
    },
}

// Allow: CLI dispatch is a verified 1157-line match tree; splitting it would churn every arm.
#[allow(clippy::too_many_lines)]
#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "tenhou_scraper=info".into()),
        )
        .init();
    let cli = Cli::parse();
    match cli.command.clone() {
        Commands::Fetch {
            start,
            end,
            log_types,
            delay_ms,
            concurrent,
            skip_fetched,
            import_dir,
        } => {
            if let Some(dir) = import_dir {
                let db = open_db(&cli)?;
                let (total_new, files_ok, files_failed) =
                    fetch::Fetcher::import_html_gz_dir(&db, &dir)?;
                info!("Imported {} new log IDs from {} files ({} failed)", total_new, files_ok, files_failed);
                return Ok(());
            }
            let (start_date, end_date) = parse_date_range(&start, end, true)?;
            let log_types: Vec<String> = log_types.split(',').map(str::trim).filter(|s| !s.is_empty()).map(std::string::ToString::to_string).collect();
            if log_types.is_empty() {
                anyhow::bail!("--log-types must name at least one type");
            }
            let log_refs: Vec<&str> = log_types.iter().map(std::string::String::as_str).collect();
            let db = open_db(&cli)?;
            let fetcher = fetch::Fetcher::new(delay_ms)?;
            let new_count = fetcher
                .fetch_date_range(&db, start_date, end_date, &log_refs, skip_fetched, concurrent)
                .await?;
            info!("Fetched {} new log IDs", new_count);
        }
        Commands::Download { limit, delay_ms, concurrent, retry_errors } => {
            let db = open_db(&cli)?;
            if retry_errors {
                let n = db.reset_download_errors()?;
                info!("Reset {} previously failed downloads for retry", n);
            }
            let downloader = download::Downloader::new(delay_ms)?;
            let (success, failed) = downloader.download_logs(&db, limit, concurrent).await?;
            info!("Downloaded {} logs ({} failed)", success, failed);
        }
        Commands::Convert { output, limit, players, hanchan, retry_errors } => {
            let db = open_db(&cli)?;
            if retry_errors {
                let n = db.reset_convert_errors()?;
                info!("Reset {} previously failed conversions for retry", n);
            }
            let converter = convert::Converter::new(&output)?;
            let (success, failed) = converter.convert_logs(&db, limit, players, hanchan)?;
            info!("Converted {} logs ({} failed)", success, failed);
        }
        Commands::Stats => {
            let db = open_db(&cli)?;
            let (total, downloaded, converted) = db.count_logs()?;
            println!("Database: {}", cli.database.display());
            println!("Total log IDs:    {total}");
            println!("Downloaded:       {downloaded}");
            println!("Converted:        {converted}");
            if downloaded > total {
                tracing::warn!("inconsistent stats: downloaded ({}) > total ({})", downloaded, total);
            }
            if converted > downloaded {
                tracing::warn!("inconsistent stats: converted ({}) > downloaded ({})", converted, downloaded);
            }
            println!("Pending download: {}", total.saturating_sub(downloaded));
            println!("Pending convert:  {}", downloaded.saturating_sub(converted));
        }

        Commands::Export { output, limit } => {
            let db = open_db(&cli)?;
            let (success, failed) = export::export_logs(&db, &output, limit)?;
            info!("Exported {} logs ({} failed)", success, failed);
        }
        Commands::Package { input, output } => {
            let count = package::package_directory(&input, &output)?;
            info!("Packaged {} files into {:?}", count, output);
        }
        Commands::Majsoul(cmd) => match cmd {
            MajsoulCommands::Search { nickname } => {
                let db = open_db(&cli)?;
                let client = majsoul::AmaeKoromoClient::new(300)?;
                let results = client.search_player(&nickname).await?;
                if results.is_empty() {
                    println!("No players found for '{nickname}'");
                } else {
                    for p in &results {
                        let level_id = p.level.as_ref().map(|l| l.id);
                        println!("{} (ID: {}, Level: {})", p.nickname, p.id, level_id.unwrap_or(0));
                        db.insert_majsoul_player(p.id, &p.nickname, level_id)?;
                    }
                }
            }
            MajsoulCommands::Fetch {
                player_id,
                mode,
                start,
                end,
                delay_ms,
            } => {
                let db = open_db(&cli)?;
                let (start_date, end_date) = parse_date_range(&start, end, true)?;
                let start_ms = start_date.and_hms_opt(0, 0, 0).ok_or_else(|| anyhow::anyhow!("invalid start date"))?.and_utc().timestamp_millis();
                let end_ms = Some(end_date.and_hms_opt(23, 59, 59).ok_or_else(|| anyhow::anyhow!("invalid end date"))?.and_utc().timestamp_millis());
                let client = majsoul::AmaeKoromoClient::new(delay_ms)?;
                let (records, api_calls) = client.get_player_records_paginated(player_id, mode, Some(start_ms), end_ms).await?;
                info!("Found {} records ({} API calls)", records.len(), api_calls);
                let mut new_count = 0;
                for r in &records {
                    if db.insert_majsoul_log_with_full_uuid(&r.uuid, player_id, r.start_time, Some(r.mode_id))? {
                        new_count += 1;
                    }
                }
                info!("Stored {} new UUIDs in database", new_count);
            }
            MajsoulCommands::Stats => {
                let db = open_db(&cli)?;
                println!("=== Majsoul Pipeline Stats ===\n");
                let (days, day_games, day_players) = db.count_day_fetch_progress()?;
                println!("Phase 1 (Day Fetch):");
                println!("  Days fetched:     {days}");
                println!("  Games seen:       {day_games}");
                println!("  Players seen:     {day_players}");
                let (total_players, scraped_players) = db.count_player_scrape_progress()?;
                if scraped_players > total_players {
                    tracing::warn!("inconsistent player stats: scraped ({}) > total ({})", scraped_players, total_players);
                }
                let remaining_players = total_players.saturating_sub(scraped_players);
                println!("\nPhase 2 (Player Scrape):");
                println!("  Total players:    {total_players}");
                println!("  Scraped:          {scraped_players}");
                println!("  Remaining:        {remaining_players}");
                let (total, downloaded, converted) = db.count_majsoul_logs()?;
                println!("\nGame Logs:");
                println!("  Total UUIDs:      {total}");
                println!("  Downloaded:       {downloaded}");
                println!("  Converted:        {converted}");
                if downloaded > total {
                    tracing::warn!("inconsistent majsoul stats: downloaded ({}) > total ({})", downloaded, total);
                }
                if converted > downloaded {
                    tracing::warn!("inconsistent majsoul stats: converted ({}) > downloaded ({})", converted, downloaded);
                }
                println!("  Pending download: {}", total.saturating_sub(downloaded));
                println!("  Pending convert:  {}", downloaded.saturating_sub(converted));
                println!("\nBy Room:");
                let by_mode = db.count_majsoul_logs_by_mode()?;
                for (mode_id, count) in by_mode {
                    let room_name = match mode_id {
                        16 => "Throne",
                        12 => "Jade",
                        9 => "Gold",
                        _ => "Other",
                    };
                    println!("  {room_name} (mode {mode_id}): {count}");
                }
            }
            MajsoulCommands::FetchPublic { room, count, server, username, password } => {
                use crate::majsoul::gateway::discover_gateway;
                use crate::majsoul::rpc::MajsoulRpc;
                let db = open_db(&cli)?;
                let room_type: u32 = room.room_type_u32();
                let server_str = server.as_str();
                info!("Fetching {} public {} room games from {} server...", count, room.as_str(), server_str);
                let client = crate::util::http_client()?;
                let (endpoint, version, route_id) = discover_gateway(&client, server_str).await?;
                let rpc = MajsoulRpc::connect(&endpoint, server.origin()).await?;
                rpc.login_native(&username, &password, &version, &route_id).await?;
                let response = rpc.fetch_game_record_list(0, count, room_type).await?;
                info!("GameRecordList: {} bytes", response.len());
                let live_response = rpc.fetch_game_live_list(0).await?;
                info!("GameLiveList: {} bytes", live_response.len());
                let listed = crate::majsoul::proto::decode_game_record_list(&response)?;
                let live_listed = crate::majsoul::proto::decode_game_record_list(&live_response)?;
                let mut stored = 0usize;
                for (uuid, player_id, start_time, mode) in listed.iter().chain(live_listed.iter()) {
                    if db.insert_majsoul_log_with_full_uuid(uuid, *player_id, *start_time, Some(*mode))? {
                        stored += 1;
                    }
                }
                info!("FetchPublic: decoded {} + {} records, stored {} new rows", listed.len(), live_listed.len(), stored);
            }
            MajsoulCommands::Download {
                limit,
                delay_ms,
                server,
                username,
                password,
                retry_errors,
            } => {
                let db = open_db(&cli)?;
                if retry_errors {
                    let n = db.reset_majsoul_download_errors()?;
                    info!("Reset {} previously failed majsoul downloads for retry", n);
                }
                let downloader = majsoul::MajsoulDownloader::new(delay_ms);
                let (success, failed) = downloader.download_logs(&db, &username, &password, limit, server.as_str()).await?;
                info!("Downloaded {} records ({} failed)", success, failed);
            }
            MajsoulCommands::Convert { output, limit, players, hanchan, retry_errors } => {
                let db = open_db(&cli)?;
                if retry_errors {
                    let n = db.reset_majsoul_convert_errors()?;
                    info!("Reset {} previously failed majsoul conversions for retry", n);
                }
                let converter = majsoul::MajsoulConverter::new(&output)?;
                let (success, failed) = converter.convert_logs(&db, limit, players, hanchan)?;
                info!("Converted {} Majsoul logs ({} failed)", success, failed);
            }
            MajsoulCommands::FetchRoom {
                room,
                start,
                end,
                delay_ms,
                skip_fetched,
            } => {
                let db = open_db(&cli)?;
                let mode_id: i32 = match room.mode_id() {
                    Some(m) => m,
                    None => anyhow::bail!("room '{}' is not supported by fetch-room (use throne, jade, or gold)", room.as_str()),
                };
                let (start_date, end_date) = parse_date_range(&start, end, true)?;
                info!(
                    "Fetching {} room games from {} to {}",
                    room.as_str(),
                    start_date.format("%Y-%m-%d"),
                    end_date.format("%Y-%m-%d")
                );
                let client = majsoul::AmaeKoromoClient::new(delay_ms)?;
                let (total_new, _) = client
                    .fetch_room_range(&db, mode_id, start_date, end_date, skip_fetched)
                    .await?;
                info!("Total new UUIDs stored: {}", total_new);
            }
            MajsoulCommands::ResolvePaipu { limit, delay_ms } => {
                let db = open_db(&cli)?;
                let unresolved = db.get_majsoul_unresolved_paipu(limit)?;
                if unresolved.is_empty() {
                    info!("No unresolved paipu URLs");
                    return Ok(());
                }
                info!("Resolving {} UUIDs to paipu URLs...", unresolved.len());
                let client = reqwest::Client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .build()?;
                let mut resolved = 0;
                let mut failed = 0;
                for (uuid, player_id, mode_id) in &unresolved {
                    let url = format!(
                        "https://5-data.amae-koromo.com/api/v2/pl4/view_game/1/{mode_id}/{uuid}/{player_id}"
                    );
                    match client.get(&url).send().await {
                        Ok(resp) => {
                            let status = resp.status();
                            let headers = resp.headers().clone();
                            if resolved + failed < 3 {
                                info!("UUID {} -> status {}, headers: {:?}", uuid, status, headers.get("location"));
                            }
                            if let Some(location) = headers.get("location") {
                                if let Ok(loc_str) = location.to_str() {
                                    db.set_majsoul_paipu_url(uuid, loc_str)?;
                                    resolved += 1;
                                    if resolved % 100 == 0 {
                                        info!("Resolved {}/{}", resolved, unresolved.len());
                                    }
                                } else {
                                    failed += 1;
                                }
                            } else {
                                failed += 1;
                            }
                        }
                        Err(e) => {
                            tracing::warn!("Failed to resolve {}: {}", uuid, e);
                            failed += 1;
                        }
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                }
                info!("Resolved {} paipu URLs ({} failed)", resolved, failed);
            }
            MajsoulCommands::ExportPaipu { output } => {
                let db = open_db(&cli)?;
                let urls = db.get_majsoul_resolved_paipu()?;
                if urls.is_empty() {
                    info!("No resolved paipu URLs to export. Run `majsoul resolve-paipu` first.");
                    return Ok(());
                }
                std::fs::write(&output, format!("{}\n", urls.join("\n")))?;
                info!("Exported {} paipu URLs to {:?}", urls.len(), output);
            }
            MajsoulCommands::FetchFullUuids { concurrent, limit, delay_ms } => {
                let db = open_db(&cli)?;
                let player_count: i64 = db.conn_query_row("SELECT COUNT(*) FROM throne_players")?;

                if player_count == 0 {
                    info!("Populating throne_players from existing logs...");
                    db.populate_throne_players()?;
                }

                let (total_players, fetched_players) = db.count_throne_players()?;
                if fetched_players > total_players {
                    tracing::warn!("inconsistent throne stats: fetched ({}) > total ({})", fetched_players, total_players);
                }
                info!("Throne players: {} total, {} fetched, {} remaining",
                    total_players, fetched_players, total_players.saturating_sub(fetched_players));

                let players = db.get_unfetched_throne_players(limit)?;
                if players.is_empty() {
                    info!("No unfetched players remaining");
                    return Ok(());
                }

                info!("Fetching records for {} players ({} concurrent)...", players.len(), concurrent);

                let client = crate::util::http_client()?;

                let mut total_records = 0;
                let mut total_new = 0;
                let mut processed = 0;

                // Process in batches
                for chunk in players.chunks(concurrent) {
                    let futures: Vec<_> = chunk.iter().map(|&player_id| {
                        let client = client.clone();
                        async move {
                            // Paginate through all records using descending mode
                            let mut all_records = Vec::new();
                            let mut end_ms: i64 = chrono::Utc::now().timestamp_millis();
                            let start_ms: i64 = 1_262_304_000_000; // 2010-01-01

                            loop {
                                // Descending mode: swap end/start, add descending=true, limit=500
                                let url = format!(
                                    "https://5-data.amae-koromo.com/api/v2/pl4/player_records/{player_id}/{end_ms}/{start_ms}?mode=16&limit=500&descending=true"
                                );

                                let resp = match client.get(&url).send().await {
                                    Ok(r) => r,
                                    Err(e) => return (player_id, Err(e)),
                                };

                                if !resp.status().is_success() {
                                    break;
                                }

                                let records: Vec<majsoul::GameRecord> = match resp.json().await {
                                    Ok(r) => r,
                                    Err(_) => break,
                                };

                                let batch_size = records.len();
                                if records.is_empty() {
                                    break;
                                }

                                // In descending mode, last record is oldest - use its endTime for next page
                                let oldest_end_time = records
                                    .last()
                                    .and_then(|r| r.end_time)
                                    .unwrap_or(0);

                                all_records.extend(records);

                                // If we got fewer than 500, we've reached the end
                                if batch_size < 500 {
                                    break;
                                }

                                // Set end_ms to oldest game's end_time (in ms) - 1 for next batch
                                end_ms = (oldest_end_time * 1000) - 1;
                            }

                            (player_id, Ok(all_records))
                        }
                    }).collect();

                    let results = futures::future::join_all(futures).await;

                    // Sequential DB writes
                    for (player_id, result) in results {
                        match result {
                            Ok(records) => {
                                let mut new_for_player = 0;
                                for r in &records {
                                    // Insert with full UUID directly (player_records returns full UUIDs)
                                    if db.insert_majsoul_log_with_full_uuid(
                                        &r.uuid,
                                        player_id,
                                        r.start_time,
                                        Some(r.mode_id),
                                    )? {
                                        new_for_player += 1;
                                    }
                                }
                                total_records += records.len();
                                total_new += new_for_player;
                                db.mark_throne_player_fetched(player_id)?;
                            }
                            Err(e) => {
                                tracing::warn!("Request failed for {}: {}", player_id, e);
                            }
                        }
                        processed += 1;
                    }

                    if processed % 50 == 0 {
                        info!("Progress: {}/{} players, {} records, {} new full UUIDs",
                            processed, players.len(), total_records, total_new);
                    }

                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                }

                info!("Done: {} players processed, {} records fetched, {} new full UUIDs",
                    processed, total_records, total_new);
            }
            MajsoulCommands::RecoverOrphans { concurrent, limit, delay_ms } => {
                use tracing::warn;
                let db = open_db(&cli)?;
                let players = db.get_unfetched_throne_players(limit)?;
                let orphan_count = db.count_orphaned_games()?;
                if players.is_empty() {
                    info!("No unfetched players. Running cross-player match only...");
                } else {
                    info!("=== ORPHAN RECOVERY (Pagination) ===");
                    info!("Orphaned games: {}", orphan_count);
                    info!("Unfetched players: {}", players.len());
                    info!("Concurrent requests: {}", concurrent);
                    info!("Fetching ALL games with pagination, then cross-matching...\n");
                    let client = majsoul::AmaeKoromoClient::new(delay_ms)?;
                    let mut total_records = 0usize;
                    let mut total_new = 0usize;
                    let mut total_api_calls = 0u32;
                    let mut processed = 0usize;
                    for chunk in players.chunks(concurrent) {
                        let futures: Vec<_> = chunk.iter().map(|&player_id| {
                            let client = &client;
                            async move {
                                let result = client.get_player_records_paginated(player_id, 16, None, None).await;
                                (player_id, result)
                            }
                        }).collect();
                        let results = futures::future::join_all(futures).await;
                        for (player_id, result) in results {
                            match result {
                                Ok((records, api_calls)) => {
                                    total_api_calls += api_calls;
                                    let mut new_for_player = 0;
                                    for r in &records {
                                        if db.insert_majsoul_log_with_full_uuid(
                                            &r.uuid, player_id, r.start_time, Some(r.mode_id)
                                        )? {
                                            new_for_player += 1;
                                        }
                                    }
                                    total_records += records.len();
                                    total_new += new_for_player;
                                    db.mark_throne_player_fetched(player_id)?;
                                    if api_calls > 1 {
                                        info!("Player {}: {} games ({} API calls, {} new)",
                                            player_id, records.len(), api_calls, new_for_player);
                                    }
                                }
                                Err(e) => {
                                    warn!("Failed to fetch player {}: {}", player_id, e);
                                }
                            }
                            processed += 1;
                        }
                        if processed.is_multiple_of(100) || processed == players.len() {
                            info!("Progress: {}/{} players | {} records | {} new full UUIDs | {} API calls",
                                processed, players.len(), total_records, total_new, total_api_calls);
                        }
                    }
                    info!("\nFetch complete. {} records, {} new full UUIDs", total_records, total_new);
                }
                info!("\n=== CROSS-PLAYER MATCHING ===");
                let before_orphans = db.count_orphaned_games()?;
                let (matched, ambiguous) = db.cross_match_orphan_uuids()?;
                let after_orphans = db.count_orphaned_games()?;
                info!("\n=== RECOVERY COMPLETE ===");
                info!("Cross-player matched: {} (ambiguous skipped: {})", matched, ambiguous);
                info!("Orphans before: {}", before_orphans);
                info!("Orphans after: {}", after_orphans);
                info!("Total recovered: {}", before_orphans.saturating_sub(after_orphans));
            }
            MajsoulCommands::ResolveUuids { limit, concurrent, delay_ms, server, username, password } => {
                use crate::majsoul::gateway::discover_gateway;
                use crate::majsoul::rpc::{MajsoulRpc, extract_full_uuid_from_record};
                use futures::stream::StreamExt;
                use std::sync::Arc;
                use std::sync::atomic::{AtomicUsize, Ordering};
                use tokio::sync::Mutex;
                let db_outer = open_db(&cli)?;
                let orphans = db_outer.get_orphan_short_uuids(limit, Some(16))?;
                if orphans.is_empty() {
                    info!("No orphan UUIDs to resolve!");
                    return Ok(());
                }
                info!("=== UUID RESOLUTION VIA RPC ===");
                info!("Orphans to resolve: {}", orphans.len());
                info!("Concurrent requests: {}", concurrent);
                info!("Server: {}", server.as_str());
                let client = crate::util::http_client()?;
                let (endpoint, version, route_id) = discover_gateway(&client, server.as_str()).await?;
                info!("Gateway: {}", endpoint);
                let rpc = MajsoulRpc::connect(&endpoint, server.origin()).await?;
                rpc.login_native(&username, &password, &version, &route_id).await?;
                info!("Logged in successfully\n");
                let rpc = Arc::new(rpc);
                let db = Arc::new(Mutex::new(db_outer));
                let resolved = Arc::new(AtomicUsize::new(0));
                let failed = Arc::new(AtomicUsize::new(0));
                let processed = Arc::new(AtomicUsize::new(0));
                let total = orphans.len();
                futures::stream::iter(orphans.into_iter().enumerate())
                    .map(|(i, short_uuid)| {
                        let rpc = Arc::clone(&rpc);
                        let db = Arc::clone(&db);
                        let resolved = Arc::clone(&resolved);
                        let failed = Arc::clone(&failed);
                        let processed = Arc::clone(&processed);
                        async move {
                            if delay_ms > 0 && i > 0 && i % concurrent == 0 {
                                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                            }
                            match rpc.fetch_game_record(&short_uuid, "").await {
                                Ok(data) => {
                                    match extract_full_uuid_from_record(&data) {
                                        Ok(full_uuid) => {
                                            let db_guard = db.lock().await;
                                            match db_guard.set_orphan_full_uuid(&short_uuid, &full_uuid) {
                                                Ok(true) => {
                                                    resolved.fetch_add(1, Ordering::Relaxed);
                                                }
                                                Ok(false) => {}
                                                Err(e) => {
                                                    tracing::warn!("DB error resolving {}: {}", short_uuid, e);
                                                    failed.fetch_add(1, Ordering::Relaxed);
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            tracing::warn!("Failed to parse {}: {}", short_uuid, e);
                                            failed.fetch_add(1, Ordering::Relaxed);
                                        }
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!("RPC failed for {}: {}", short_uuid, e);
                                    failed.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                            let current = processed.fetch_add(1, Ordering::Relaxed) + 1;
                            if current.is_multiple_of(100) || current == total {
                                let db_guard = db.lock().await;
                                match db_guard.count_orphaned_games() {
                                    Ok(remaining) => {
                                        info!(
                                            "Progress: {}/{} | Resolved: {} | Failed: {} | Remaining: {}",
                                            current, total,
                                            resolved.load(Ordering::Relaxed),
                                            failed.load(Ordering::Relaxed),
                                            remaining
                                        );
                                    }
                                    Err(e) => {
                                        tracing::warn!("DB error counting orphans: {}", e);
                                    }
                                }
                            }
                        }
                    })
                    .buffer_unordered(concurrent)
                    .collect::<Vec<()>>()
                    .await;
                let final_resolved = resolved.load(Ordering::Relaxed);
                let final_failed = failed.load(Ordering::Relaxed);
                let db_guard = db.lock().await;
                let remaining = db_guard.count_orphaned_games()?;
                info!("\n=== RESOLUTION COMPLETE ===");
                info!("Resolved: {}", final_resolved);
                info!("Failed: {}", final_failed);
                info!("Remaining orphans: {}", remaining);
            }
            MajsoulCommands::ScrapeAll { rps, start } => {
                use std::sync::Arc;
                use tokio::sync::Mutex;
                use tracing::warn;
                let db_outer = open_db(&cli)?;
                db_outer.enable_wal_mode()?;
                let start_date = NaiveDate::parse_from_str(&start, "%Y%m%d")?;
                let delay_ms = 1000 / u64::from(rps);
                info!("=== EXHAUSTIVE THRONE SCRAPER ===");
                info!("Start date: {}", start_date);
                info!("Rate: {} req/s ({}ms delay)", rps, delay_ms);
                info!("Running until no new games found...\n");
                let db = Arc::new(Mutex::new(db_outer));
                let client = Arc::new(crate::util::http_client()?);
                let api_client = Arc::new(majsoul::AmaeKoromoClient::new(delay_ms)?);
                let mut round = 0;
                loop {
                    round += 1;
                    let mut new_this_round = 0;
                    let mut failures: u32 = 0;
                    info!("=== Round {} ===", round);
                    let dates_to_fetch = {
                        let db_guard = db.lock().await;
                        let today = chrono::Local::now().date_naive();
                        let mut current_date = start_date;
                        let mut dates = Vec::new();
                        while current_date <= today {
                            let date_str = current_date.format("%Y-%m-%d").to_string();
                            if !db_guard.is_majsoul_room_fetched(&date_str, 16)? {
                                dates.push(current_date);
                            }
                            current_date += chrono::Duration::days(1);
                        }
                        dates
                    };

                    if dates_to_fetch.is_empty() {
                        info!("[Dates] All dates already fetched");
                    } else {
                        info!("[Dates] {} unfetched dates to process", dates_to_fetch.len());

                        for date in &dates_to_fetch {
                            let date_str = date.format("%Y-%m-%d").to_string();
                            let day_start_ms = date.and_hms_opt(0, 0, 0).ok_or_else(|| anyhow::anyhow!("invalid date"))?.and_utc().timestamp_millis();
                            let day_end_ms = date.and_hms_opt(23, 59, 59).ok_or_else(|| anyhow::anyhow!("invalid date"))?.and_utc().timestamp_millis();

                            // 6-hour chunks to avoid 500 cap
                            let chunk_ms: i64 = 6 * 60 * 60 * 1000;
                            let mut chunk_start = day_start_ms;
                            let mut day_new = 0;
                            let mut all_chunks_ok = true;

                            while chunk_start < day_end_ms {
                                let chunk_end = (chunk_start + chunk_ms).min(day_end_ms);
                                let url = format!(
                                    "https://5-data.amae-koromo.com/api/v2/pl4/games/{chunk_start}/{chunk_end}?mode=16&limit=500"
                                );

                                match fetch_with_retry(&client, &url, 3).await {
                                    Ok(records) => {
                                        // Warn if we hit the cap
                                        if records.len() >= 500 {
                                            warn!("{} chunk hit 500 cap - may be missing records!", date_str);
                                        }
                                        let db_guard = db.lock().await;
                                        for r in &records {
                                            let Some(player_id) = r.players.first().map(|p| p.account_id) else {
                                                warn!("skipping {}: empty players", r.uuid);
                                                continue;
                                            };
                                            if player_id == 0 {
                                                continue;
                                            }
                                            if db_guard.insert_majsoul_log(&r.uuid, player_id, r.start_time, Some(r.mode_id))? {
                                                day_new += 1;
                                            }
                                            for p in &r.players {
                                                if p.account_id == 0 {
                                                    continue;
                                                }
                                                db_guard.upsert_throne_player(p.account_id, &p.nickname)?;
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        warn!("[Dates] {} chunk {}-{} FAILED: {}", date_str, chunk_start, chunk_end, e);
                                        all_chunks_ok = false;
                                        failures += 1;
                                    }
                                }
                                chunk_start = chunk_end;
                                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                            }

                            // Only mark as fetched if ALL chunks succeeded
                            if all_chunks_ok {
                                let db_guard = db.lock().await;
                                db_guard.mark_majsoul_room_fetched_with_count(&date_str, 16, i32::try_from(day_new).unwrap_or(i32::MAX))?;
                                if day_new > 0 {
                                    info!("[Dates] {}: {} new games", date_str, day_new);
                                }
                            } else {
                                warn!("[Dates] {} NOT marked complete due to failures - will retry next round", date_str);
                            }
                            new_this_round += day_new;
                        }
                    }

                    // Phase 2: Player expander - BFS to get full UUIDs (WITH PAGINATION)
                    {
                        let players = {
                            let db = db.lock().await;
                            db.get_unfetched_throne_players(None)?
                        };

                        if players.is_empty() {
                            info!("[Players] All players already fetched");
                        } else {
                            info!("[Players] {} unfetched players to process (with pagination)", players.len());

                            let mut processed = 0;
                            let total_players = players.len();
                            let concurrent = 4; // Limit concurrent requests for pagination

                            for chunk in players.chunks(concurrent) {
                                let futures: Vec<_> = chunk.iter().map(|&player_id| {
                                    let api_client = api_client.clone();
                                    async move {
                                        let result = api_client.get_player_records_paginated(player_id, 16, None, None).await;
                                        (player_id, result)
                                    }
                                }).collect();

                                let results = futures::future::join_all(futures).await;

                                let db_guard = db.lock().await;
                                for (player_id, result) in results {
                                    match result {
                                        Ok((records, api_calls)) => {
                                            if api_calls > 1 {
                                                info!("[Players] {} fetched {} games in {} API calls",
                                                    player_id, records.len(), api_calls);
                                            }
                                            for r in &records {
                                                if db_guard.insert_majsoul_log_with_full_uuid(
                                                    &r.uuid, player_id, r.start_time, Some(r.mode_id)
                                                )? {
                                                    new_this_round += 1;
                                                }
                                                for p in &r.players {
                                                    if p.account_id == 0 {
                                                        continue;
                                                    }
                                                    db_guard.upsert_throne_player(p.account_id, &p.nickname)?;
                                                }
                                            }
                                            db_guard.mark_throne_player_fetched(player_id)?;
                                        }
                                        Err(e) => {
                                            warn!("[Players] {} fetch error: {}", player_id, e);
                                            failures += 1;
                                        }
                                    }
                                }
                                drop(db_guard);
                                processed += chunk.len();
                                if processed % 100 == 0 || processed == total_players {
                                    info!("[Players] {}/{} processed, {} new full UUIDs", processed, total_players, new_this_round);
                                }
                            }
                        }
                    }

                    // Check stats
                    let (total, with_full) = {
                        let db = db.lock().await;
                        db.count_majsoul_full_uuids()?
                    };
                    let (total_players, fetched_players) = {
                        let db = db.lock().await;
                        db.count_throne_players()?
                    };

                    // Phase 3: Cross-match orphans with newly fetched full UUIDs
                    {
                        let db_guard = db.lock().await;

                        let (matched, ambiguous) = db_guard.cross_match_orphan_uuids()?;
                        if matched > 0 {
                            info!("[Cross-match] Filled {} orphan full_uuids via timestamp matching (ambiguous skipped: {})", matched, ambiguous);
                            new_this_round += matched;
                        } else if ambiguous > 0 {
                            info!("[Cross-match] {} ambiguous orphans skipped", ambiguous);
                        }
                    }

                    info!("\n[Round {} Summary]", round);
                    info!("  New games this round: {}", new_this_round);
                    info!("  Total games: {} ({} with full UUID)", total, with_full);
                    info!("  Players: {} ({} fetched)\n", total_players, fetched_players);

                    // Date fetch discovers players who discover games who discover more players, so stop only when a round adds nothing AND nothing failed AND no unfetched dates/players remain.
                    if new_this_round == 0 && failures == 0 {
                        let dates_empty = dates_to_fetch.is_empty();
                        let players_empty = {
                            let db_guard = db.lock().await;
                            db_guard.get_unfetched_throne_players(None)?.is_empty()
                        };
                        if dates_empty && players_empty {
                            info!("=== CONVERGENCE REACHED ===");
                            info!("No new games found. Scraping complete!");
                            info!("Total unique games with paipu: {}", with_full);
                            break;
                        }
                    }
                    if failures > 0 {
                        warn!("Round {} had {} failures; retrying with backoff", round, failures);
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    }
                }
            }
            MajsoulCommands::ResetCappedPlayers => {
                let db = open_db(&cli)?;
                let count = db.reset_capped_throne_players()?;
                info!("Reset {} players who hit the 200-game cap", count);
                info!("Run 'majsoul scrape-all' to re-fetch with pagination");
            }
            MajsoulCommands::BulkDownload { limit, delay_ms, restart_every, server, username, password } => {
                use crate::majsoul::parallel_download::ParallelDownloader;
                use std::sync::Arc;
                use tokio::sync::Mutex;
                let db_outer = open_db(&cli)?;
                db_outer.enable_wal_mode()?;
                let downloadable = db_outer.count_majsoul_downloadable()?;
                info!("Downloadable records (with full_uuid): {}", downloadable);
                if downloadable == 0 {
                    info!("No records to download. Run 'majsoul fetch-full-uuids' first.");
                    return Ok(());
                }
                let db = Arc::new(Mutex::new(db_outer));
                let downloader = ParallelDownloader::new(delay_ms, restart_every);
                let (success, failed) = downloader.download_with_credentials(db, &username, &password, server.as_str(), limit).await?;
                info!("Bulk download complete: {} success, {} failed", success, failed);
            }
            MajsoulCommands::ResolvePhantoms { limit, delay_ms, server } => {
                let db = open_db(&cli)?;
                let phantoms = db.get_orphan_short_uuids(limit, Some(16))?;
                if phantoms.is_empty() {
                    info!("No phantom UUIDs to resolve");
                    return Ok(());
                }
                info!("Resolving {} phantom UUIDs via browser...", phantoms.len());
                let results = majsoul::browser::resolve_phantom_uuids(server.as_str(), &phantoms, delay_ms).await?;
                let mut resolved = 0;
                for (short, full) in results {
                    if db.set_orphan_full_uuid(&short, &full)? {
                        resolved += 1;
                    }
                }
                info!("Resolved {} phantom UUIDs", resolved);
            }
            MajsoulCommands::DownloadJson { output, limit, username, password, delay_ms, server } => {
                use crate::majsoul::json_download::download_as_json;
                let db = open_db(&cli)?;
                let (success, failed) = download_as_json(
                    &db,
                    &output,
                    limit,
                    &username,
                    &password,
                    delay_ms,
                    server.as_str(),
                ).await?;
                info!("Downloaded {} games as Tenhou JSON ({} failed)", success, failed);
            }
            MajsoulCommands::RawDownload { accounts, password, todo, completed, output, server, limit, delay_ms } => {
                use crate::majsoul::raw_download::raw_download;
                let (success, failed) = raw_download(
                    &accounts,
                    &password,
                    &todo,
                    &completed,
                    &output,
                    server.as_str(),
                    limit,
                    delay_ms,
                ).await?;
                info!("Raw download complete: {} success, {} failed", success, failed);
            }
            MajsoulCommands::ConvertRaw { input, output, delete } => {
                let (success, failed) = majsoul::convert::convert_raw_files(&input, &output, delete)?;
                info!("Converted {} .pb files to MJAI ({} failed)", success, failed);
            }
            MajsoulCommands::FetchDays { start, end, delay_ms } => {
                let db = open_db(&cli)?;
                let (start_date, end_date) = parse_date_range(&start, end, false)?;
                let (days_done, total_games, total_new_players, total_players_db, scraped) =
                    majsoul::AmaeKoromoClient::fetch_days_phase(&db, start_date, end_date, delay_ms)
                        .await?;
                info!("\n=== Phase 1 Complete ===");
                info!("Days fetched: {}", days_done);
                info!("Total games seen: {}", total_games);
                info!("New players discovered: {}", total_new_players);
                info!("Total players in DB: {} ({} scraped)", total_players_db, scraped);
                info!("\nRun 'majsoul scrape-players' for Phase 2");
            }
            MajsoulCommands::ScrapePlayers { limit, concurrent, delay_ms } => {
                let db = open_db(&cli)?;
                let unscraped = db.get_unscraped_players(limit)?;

                if unscraped.is_empty() {
                    info!("All players already scraped!");
                    let (total, scraped) = db.count_player_scrape_progress()?;
                    info!("Total: {}, Scraped: {}", total, scraped);
                    return Ok(());
                }

                info!("=== PHASE 2: Scrape Players ===");
                info!("Unscraped players: {}", unscraped.len());
                info!("Concurrent: {}", concurrent);

                let client = majsoul::AmaeKoromoClient::new(delay_ms)?;
                let mut processed = 0usize;
                let mut total_games = 0usize;
                let mut total_new_uuids = 0usize;
                let total_players = unscraped.len();

                for chunk in unscraped.chunks(concurrent) {
                    let futures: Vec<_> = chunk.iter().map(|&player_id| {
                        let client = &client;
                        async move {
                            let result = client.get_player_records_paginated(player_id, 16, None, None).await;
                            (player_id, result)
                        }
                    }).collect();

                    let results = futures::future::join_all(futures).await;

                    for (player_id, result) in results {
                        match result {
                            Ok((records, api_calls)) => {
                                let mut new_for_player = 0;
                                for r in &records {
                                    if db.insert_majsoul_log_with_full_uuid(
                                        &r.uuid,
                                        player_id,
                                        r.start_time,
                                        Some(r.mode_id),
                                    )? {
                                        new_for_player += 1;
                                    }
                                }

                                db.mark_player_scraped(player_id, i32::try_from(records.len()).unwrap_or(i32::MAX))?;
                                total_games += records.len();
                                total_new_uuids += new_for_player;

                                if api_calls > 1 || records.len() > 100 {
                                    info!(
                                        "Player {}: {} games ({} API calls, {} new)",
                                        player_id, records.len(), api_calls, new_for_player
                                    );
                                }
                            }
                            Err(e) => {
                                tracing::warn!("Failed to scrape player {}: {}", player_id, e);
                            }
                        }
                        processed += 1;
                    }

                    if processed.is_multiple_of(100) || processed == total_players {
                        let (total_p, scraped_p) = db.count_player_scrape_progress()?;
                        info!(
                            "Progress: {}/{} players | {} games | {} new UUIDs | {}/{} total scraped",
                            processed, total_players, total_games, total_new_uuids, scraped_p, total_p
                        );
                    }
                }

                let (total_logs, downloaded, _) = db.count_majsoul_logs()?;
                let (total_p, scraped_p) = db.count_player_scrape_progress()?;

                info!("\n=== Phase 2 Complete ===");
                info!("Players scraped: {}/{}", scraped_p, total_p);
                info!("Total games in DB: {}", total_logs);
                info!("Games downloaded: {}", downloaded);
                info!("New UUIDs this run: {}", total_new_uuids);
            }
        },
    }

    Ok(())
}
#[cfg(test)]
mod cli_tests {
    use super::*;
    #[test]
    fn reject_concurrent_zero_fetch() {
        let r = Cli::try_parse_from(["t", "fetch", "--start", "20240101", "--concurrent", "0"]);
        assert!(r.is_err(), "fetch --concurrent 0 must fail at parse");
    }
    #[test]
    fn reject_concurrent_zero_download() {
        let r = Cli::try_parse_from(["t", "download", "--concurrent", "0"]);
        assert!(r.is_err(), "download --concurrent 0 must fail at parse");
    }
    #[test]
    fn reject_limit_zero_download() {
        let r = Cli::try_parse_from(["t", "download", "--limit", "0"]);
        assert!(r.is_err(), "--limit 0 must fail at parse");
    }
    #[test]
    fn reject_rps_zero() {
        let r = Cli::try_parse_from(["t", "majsoul", "scrape-all", "--rps", "0"]);
        assert!(r.is_err(), "--rps 0 must fail at parse");
    }
    #[test]
    fn reject_count_zero_fetch_public() {
        let r = Cli::try_parse_from(["t", "majsoul", "fetch-public", "--room", "throne", "--count", "0", "--username", "u", "--password", "p"]);
        assert!(r.is_err(), "--count 0 must fail at parse");
    }
    #[test]
    fn reject_mode_99() {
        let r = Cli::try_parse_from(["t", "majsoul", "fetch", "--player-id", "1", "--mode", "99", "--start", "20240101"]);
        assert!(r.is_err(), "--mode 99 must fail at parse");
    }
    #[test]
    fn reject_players_zero_convert() {
        let r = Cli::try_parse_from(["t", "convert", "--players", "0"]);
        assert!(r.is_err(), "--players 0 must fail at parse");
    }
    #[test]
    fn reject_bad_room() {
        let r = Cli::try_parse_from(["t", "majsoul", "fetch-public", "--room", "thron", "--username", "u", "--password", "p"]);
        assert!(r.is_err(), "--room thron must fail at parse");
    }
    #[test]
    fn reject_bad_server() {
        let r = Cli::try_parse_from(["t", "majsoul", "fetch-public", "--server", "xx", "--username", "u", "--password", "p"]);
        assert!(r.is_err(), "--server xx must fail at parse");
    }
    #[test]
    fn accept_delete_bare_and_skip_fetched_false() {
        let r = Cli::try_parse_from(["t", "majsoul", "convert-raw", "--input", "a", "--output", "b", "--delete"]);
        assert!(r.is_ok(), "bare --delete must parse");
        let r = Cli::try_parse_from(["t", "fetch", "--start", "20240101", "--skip-fetched=false"]);
        assert!(r.is_ok(), "--skip-fetched=false must parse");
    }
}
