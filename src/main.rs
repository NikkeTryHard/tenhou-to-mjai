mod convert;
mod db;
mod download;
mod export;
mod fetch;
mod majsoul;
mod package;

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

        /// Number of concurrent date fetches (default: 1)
        #[arg(short, long, default_value = "1")]
        concurrent: usize,

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

        /// Number of concurrent downloads (default: 1)
        #[arg(short, long, default_value = "1")]
        concurrent: usize,
    },

    /// Convert downloaded logs to MJAI format
    Convert {
        /// Output directory for MJAI files
        #[arg(short, long, default_value = "mjai")]
        output: PathBuf,

        /// Maximum logs to convert (default: all)
        #[arg(short, long)]
        limit: Option<usize>,

        /// Filter by player count (e.g., 4 for 4-player games)
        #[arg(short, long)]
        players: Option<i32>,

        /// Only convert hanchan (full games)
        #[arg(long)]
        hanchan: bool,
    },

    /// Show database statistics
    Stats,

    /// Export XML logs from database to files
    Export {
        /// Output directory for XML files
        #[arg(short, long, default_value = "xml")]
        output: PathBuf,

        /// Maximum logs to export (default: all)
        #[arg(short, long)]
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

#[derive(Subcommand)]
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

        /// Room mode (16=Throne, 12=Jade, 9=Gold)
        #[arg(long, default_value = "16")]
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
    FetchPublic {
        /// Room type: throne, jade, gold, silver, bronze, all
        #[arg(long, default_value = "throne")]
        room: String,

        /// Number of games to fetch
        #[arg(short, long, default_value = "100")]
        count: u32,

        /// Server region: en, jp, cn
        #[arg(long, default_value = "en")]
        server: String,
    },

    /// Download game records
    ///
    /// Uses cached token from `majsoul auth` if --token not provided.
    Download {
        /// Access token (optional if authenticated via `majsoul auth`)
        #[arg(long)]
        token: Option<String>,

        /// Maximum records to download
        #[arg(short, long)]
        limit: Option<usize>,

        /// Delay between requests in ms
        #[arg(long, default_value = "1500")]
        delay_ms: u64,

        /// Server region: cn, en, jp (uses cached token's server if not specified)
        #[arg(long)]
        server: Option<String>,

        /// Use browser to download (bypasses token issues for CN server)
        #[arg(long)]
        browser: bool,
    },

    /// Authenticate with Majsoul via browser (interactive)
    ///
    /// Opens Chrome, navigates to Majsoul, and captures your access token
    /// when you login. Token is cached for future use.
    Auth {
        /// Force re-authentication even if cached token exists
        #[arg(long)]
        force: bool,

        /// Server region: cn, en, jp (default: en)
        #[arg(long, default_value = "en")]
        server: String,
    },

    /// Convert downloaded Majsoul logs to MJAI format
    Convert {
        /// Output directory for MJAI files
        #[arg(short, long, default_value = "mjai-majsoul")]
        output: PathBuf,

        /// Maximum logs to convert (default: all)
        #[arg(short, long)]
        limit: Option<usize>,

        /// Filter by player count (e.g., 4 for 4-player games)
        #[arg(short, long)]
        players: Option<i32>,

        /// Only convert hanchan (full games)
        #[arg(long)]
        hanchan: bool,
    },

    /// Fetch game UUIDs from ranked rooms (no player ID needed)
    FetchRoom {
        /// Room type: throne (16), jade (12), gold (9)
        #[arg(long, default_value = "throne")]
        room: String,

        /// Start date (YYYYMMDD)
        #[arg(long)]
        start: String,

        /// End date (YYYYMMDD), defaults to today
        #[arg(long)]
        end: Option<String>,

        /// Delay between API requests in ms
        #[arg(long, default_value = "1000")]
        delay_ms: u64,

        /// Skip dates already fetched
        #[arg(long, default_value = "true")]
        skip_fetched: bool,
    },

    /// Resolve short UUIDs to full paipu URLs via Amae-Koromo
    ResolvePaipu {
        /// Maximum UUIDs to resolve (default: all unresolved)
        #[arg(short, long)]
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
        #[arg(short, long, default_value = "10")]
        concurrent: usize,

        /// Maximum players to process
        #[arg(short, long)]
        limit: Option<usize>,

        /// Delay between batches in ms
        #[arg(long, default_value = "100")]
        delay_ms: u64,
    },

    /// Recover orphaned games by re-fetching players with pagination
    RecoverOrphans {
        /// Number of concurrent API requests
        #[arg(short, long, default_value = "5")]
        concurrent: usize,

        /// Maximum players to process
        #[arg(short, long)]
        limit: Option<usize>,

        /// Delay between batches in ms
        #[arg(long, default_value = "200")]
        delay_ms: u64,
    },

    /// Resolve short UUIDs to full UUIDs via Majsoul RPC
    ResolveUuids {
        /// Maximum UUIDs to resolve
        #[arg(short, long)]
        limit: Option<usize>,

        /// Concurrent RPC requests (default: 4)
        #[arg(short, long, default_value = "4")]
        concurrent: usize,

        /// Delay between request batches in ms
        #[arg(long, default_value = "200")]
        delay_ms: u64,

        /// Server region: en, jp (default: en)
        #[arg(long, default_value = "en")]
        server: String,
    },

    /// Exhaustive scrape: fetch ALL Throne games (runs until no new games found)
    ScrapeAll {
        /// Requests per second (stay under 5 to be safe)
        #[arg(long, default_value = "4")]
        rps: u32,

        /// Start date for date fetcher (YYYYMMDD)
        #[arg(long, default_value = "20190801")]
        start: String,
    },

    /// Reset fetch status for players who hit the 200-game cap
    ResetCappedPlayers,

    /// Bulk download with multiple accounts (parallel)
    BulkDownload {
        /// Path to tokens file (format: uid,token,server per line)
        #[arg(long)]
        tokens: PathBuf,

        /// Maximum records to download
        #[arg(short, long)]
        limit: Option<usize>,

        /// Delay between requests per account in ms
        #[arg(long, default_value = "2000")]
        delay_ms: u64,

        /// Restart RPC connection every N records (prevents memory leaks)
        #[arg(long, default_value = "10000")]
        restart_every: usize,
    },

    /// Resolve phantom UUIDs via browser injection
    ResolvePhantoms {
        /// Maximum UUIDs to resolve
        #[arg(short, long)]
        limit: Option<usize>,

        /// Delay between requests in ms
        #[arg(long, default_value = "2000")]
        delay_ms: u64,

        /// Server region: en, jp
        #[arg(long, default_value = "en")]
        server: String,
    },
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
            concurrent,
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
                .fetch_date_range(&db, start_date, end_date, &log_types, skip_fetched, concurrent)
                .await?;

            info!("Fetched {} new log IDs", new_count);
        }

        Commands::Download { limit, delay_ms, concurrent } => {
            let downloader = download::Downloader::new(delay_ms)?;
            let (success, failed) = downloader.download_logs(&db, limit, concurrent).await?;
            info!("Downloaded {} logs ({} failed)", success, failed);
        }

        Commands::Convert { output, limit, players, hanchan } => {
            let converter = convert::Converter::new(&output)?;
            let (success, failed) = converter.convert_logs(&db, limit, players, hanchan)?;
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

        Commands::Export { output, limit } => {
            let (success, failed) = export::export_logs(&db, &output, limit)?;
            info!("Exported {} logs ({} failed)", success, failed);
        }

        Commands::Package { input, output } => {
            let count = package::package_directory(&input, &output)?;
            info!("Packaged {} files into {:?}", count, output);
        }

        Commands::Majsoul(cmd) => match cmd {
            MajsoulCommands::Search { nickname } => {
                let client = majsoul::AmaeKoromoClient::new(300)?;
                let results = client.search_player(&nickname).await?;
                if results.is_empty() {
                    println!("No players found for '{}'", nickname);
                } else {
                    for p in &results {
                        let level_id = p.level.as_ref().map(|l| l.id);
                        println!("{} (ID: {}, Level: {})", p.nickname, p.id, level_id.unwrap_or(0));
                        // Store player in database
                        let _ = db.insert_majsoul_player(p.id, &p.nickname, level_id);
                    }
                }
            }
            MajsoulCommands::Fetch {
                player_id,
                mode,
                start: _,
                end: _,
                delay_ms,
            } => {
                let client = majsoul::AmaeKoromoClient::new(delay_ms)?;
                let (records, api_calls) = client.get_player_records_paginated(player_id, mode).await?;

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
                let (total, downloaded, converted) = db.count_majsoul_logs()?;
                println!("Majsoul logs:");
                println!("  Total UUIDs:      {}", total);
                println!("  Downloaded:       {}", downloaded);
                println!("  Converted:        {}", converted);
                println!("  Pending download: {}", total - downloaded);
                println!("  Pending convert:  {}", downloaded - converted);

                println!("\nBy room:");
                let by_mode = db.count_majsoul_logs_by_mode()?;
                for (mode_id, count) in by_mode {
                    let room_name = match mode_id {
                        16 => "Throne",
                        12 => "Jade",
                        9 => "Gold",
                        _ => "Other",
                    };
                    println!("  {} (mode {}): {}", room_name, mode_id, count);
                }

                println!("\nRoom fetch progress (days):");
                let fetch_days = db.count_majsoul_room_fetch_days()?;
                for (mode_id, days) in fetch_days {
                    let room_name = match mode_id {
                        16 => "Throne",
                        12 => "Jade",
                        9 => "Gold",
                        _ => "Other",
                    };
                    println!("  {} (mode {}): {} days", room_name, mode_id, days);
                }
            }
            MajsoulCommands::FetchPublic { room, count, server } => {
                use crate::majsoul::browser::CachedToken;
                use crate::majsoul::gateway::discover_gateway;
                use crate::majsoul::rpc::MajsoulRpc;

                let room_type: u32 = match room.to_lowercase().as_str() {
                    "throne" => 5,
                    "jade" => 4,
                    "gold" => 3,
                    "silver" => 2,
                    "bronze" => 1,
                    "all" => 0,
                    _ => anyhow::bail!("Invalid room type: {}. Use: throne, jade, gold, silver, bronze, all", room),
                };

                // Get token
                let access_token = match CachedToken::load()? {
                    Some(cached) => {
                        info!("Using cached token (server: {})", cached.server);
                        cached.access_token
                    }
                    None => anyhow::bail!("No cached token. Run `majsoul auth` first."),
                };

                info!("Fetching {} public {} room games from {} server...", count, room, server);

                // Connect and login
                let client = reqwest::Client::new();
                let (endpoint, version) = discover_gateway(&client, &server).await?;
                let rpc = MajsoulRpc::connect(&endpoint).await?;
                rpc.login(&access_token, &version, &server).await?;

                // Fetch public game list
                let response = rpc.fetch_game_record_list(0, count, room_type).await?;
                info!("GameRecordList: {} bytes", response.len());

                // Also try live games
                let live_response = rpc.fetch_game_live_list(0).await?;
                info!("GameLiveList: {} bytes", live_response.len());

                // Debug: dump responses
                if response.len() > 0 {
                    info!("RecordList preview: {:02x?}", &response[..response.len().min(100)]);
                }
                if live_response.len() > 0 {
                    info!("LiveList preview: {:02x?}", &live_response[..live_response.len().min(200)]);
                }

                info!("Done");
            }
            MajsoulCommands::Auth { force, server } => {
                use crate::majsoul::browser::{capture_token_interactive, CachedToken};

                // Check for existing token
                if !force {
                    if let Ok(Some(token)) = CachedToken::load() {
                        info!(
                            "Found cached token for {} server (captured at {})",
                            token.server,
                            chrono::DateTime::from_timestamp(token.captured_at, 0)
                                .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
                                .unwrap_or_else(|| "unknown".to_string())
                        );
                        info!("Use --force to re-authenticate");
                        println!(
                            "Token: {}... (server: {})",
                            &token.access_token[..8.min(token.access_token.len())],
                            token.server
                        );
                        return Ok(());
                    }
                }

                let token = capture_token_interactive(&server).await?;
                println!(
                    "Token captured: {}... (server: {})",
                    &token.access_token[..8.min(token.access_token.len())],
                    token.server
                );
            }
            MajsoulCommands::Download {
                token,
                limit,
                delay_ms,
                server,
                browser,
            } => {
                use crate::majsoul::browser::CachedToken;

                // Browser mode: use browser's authenticated session directly (bypasses token issues)
                if browser {
                    use tracing::warn;

                    // Default to EN server if not specified
                    let server = server.unwrap_or_else(|| "en".to_string());

                    info!("Using browser-based batch download for {} server", server);

                    let uuids = db.get_majsoul_undownloaded(limit)?;
                    if uuids.is_empty() {
                        info!("No pending downloads in DB - will fetch game list from server");
                    } else {
                        info!("Will download {} records from DB (browser stays open)", uuids.len());
                    }

                    // Single browser session for all downloads
                    // If uuids is empty, browser will fetch game list from server
                    let results = majsoul::browser::fetch_game_records_batch(&server, &uuids, delay_ms).await?;

                    let mut success = 0;
                    let mut failed = 0;

                    for (uuid, result) in results {
                        match result {
                            Ok(data) => {
                                if let Err(e) = db.mark_majsoul_downloaded(&uuid, &data) {
                                    warn!("Failed to save {}: {}", uuid, e);
                                    failed += 1;
                                } else {
                                    success += 1;
                                    info!("Saved {} ({} bytes)", uuid, data.len());
                                }
                            }
                            Err(e) => {
                                warn!("Failed {}: {}", uuid, e);
                                db.mark_majsoul_download_error(&uuid)?;
                                failed += 1;
                            }
                        }
                    }

                    info!("Downloaded {} records ({} failed)", success, failed);
                    return Ok(());
                }

                let (access_token, server) = match token {
                    Some(t) => (t, server.unwrap_or_else(|| "en".to_string())),
                    None => {
                        // Try to load cached token
                        match CachedToken::load()? {
                            Some(cached) => {
                                info!("Using cached token from majsoul auth (server: {})", cached.server);
                                let srv = server.unwrap_or(cached.server.clone());
                                (cached.access_token, srv)
                            }
                            None => {
                                anyhow::bail!(
                                    "No token provided and no cached token found.\n\
                                     Run `majsoul auth` first, or provide --token"
                                );
                            }
                        }
                    }
                };

                let downloader = majsoul::MajsoulDownloader::new(delay_ms);
                let (success, failed) = downloader.download_logs(&db, &access_token, limit, &server).await?;
                info!("Downloaded {} records ({} failed)", success, failed);
            }
            MajsoulCommands::Convert { output, limit, players, hanchan } => {
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
                let mode_id: i32 = match room.to_lowercase().as_str() {
                    "throne" => 16,
                    "jade" => 12,
                    "gold" => 9,
                    _ => anyhow::bail!(
                        "Invalid room: {}. Use: throne, jade, gold",
                        room
                    ),
                };

                let start_date = NaiveDate::parse_from_str(&start, "%Y%m%d")?;
                let end_date = match end {
                    Some(e) => NaiveDate::parse_from_str(&e, "%Y%m%d")?,
                    None => chrono::Local::now().date_naive(),
                };

                info!(
                    "Fetching {} room games from {} to {}",
                    room,
                    start_date.format("%Y-%m-%d"),
                    end_date.format("%Y-%m-%d")
                );

                let client = majsoul::AmaeKoromoClient::new(delay_ms)?;
                let mut total_new = 0;
                let mut current_date = start_date;

                // Use 6-hour chunks to avoid API's 500 record cap per request
                const CHUNK_HOURS: i64 = 6;
                let chunk_ms = CHUNK_HOURS * 60 * 60 * 1000;

                while current_date <= end_date {
                    let date_str = current_date.format("%Y-%m-%d").to_string();

                    if skip_fetched && db.is_majsoul_room_fetched(&date_str, mode_id)? {
                        info!("Skipping {} (already fetched)", date_str);
                        current_date += chrono::Duration::days(1);
                        continue;
                    }

                    let day_start_ms = current_date
                        .and_hms_opt(0, 0, 0)
                        .unwrap()
                        .and_utc()
                        .timestamp_millis();
                    let day_end_ms = current_date
                        .and_hms_opt(23, 59, 59)
                        .unwrap()
                        .and_utc()
                        .timestamp_millis();

                    // Fetch in 6-hour chunks to stay under 500 record API limit
                    let mut day_total = 0;
                    let mut day_new = 0;
                    let mut chunk_start = day_start_ms;

                    while chunk_start < day_end_ms {
                        let chunk_end = (chunk_start + chunk_ms).min(day_end_ms);

                        match client.get_room_records(chunk_start, chunk_end, mode_id, 500).await {
                            Ok(records) => {
                                for r in &records {
                                    // Use first player's account_id for paipu URL generation
                                    let account_id = r.players.first().map(|p| p.account_id).unwrap_or(0);
                                    if db.insert_majsoul_log(&r.uuid, account_id, r.start_time, Some(r.mode_id))? {
                                        day_new += 1;
                                    }
                                }
                                day_total += records.len();

                                // Warn if we hit the cap (might be missing records)
                                if records.len() >= 500 {
                                    tracing::warn!(
                                        "{} chunk {}-{}: hit 500 cap, may be missing records!",
                                        date_str,
                                        chunk_start,
                                        chunk_end
                                    );
                                }
                            }
                            Err(e) => {
                                tracing::warn!("Failed to fetch {} chunk: {}", date_str, e);
                            }
                        }

                        chunk_start = chunk_end;
                    }

                    info!(
                        "{}: {} records ({} new)",
                        date_str,
                        day_total,
                        day_new
                    );
                    total_new += day_new;
                    db.mark_majsoul_room_fetched_with_count(&date_str, mode_id, day_total as i32)?;

                    current_date += chrono::Duration::days(1);
                }

                info!("Total new UUIDs stored: {}", total_new);
            }
            MajsoulCommands::ResolvePaipu { limit, delay_ms } => {
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

                for (uuid, player_id) in &unresolved {
                    let url = format!(
                        "https://5-data.amae-koromo.com/api/v2/pl4/view_game/1/16/{}/{}",
                        uuid, player_id
                    );

                    match client.get(&url).send().await {
                        Ok(resp) => {
                            let status = resp.status();
                            let headers = resp.headers().clone();

                            // Debug: print first few
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
                let urls = db.get_majsoul_resolved_paipu()?;
                if urls.is_empty() {
                    info!("No resolved paipu URLs to export. Run `majsoul resolve-paipu` first.");
                    return Ok(());
                }

                std::fs::write(&output, urls.join("\n"))?;
                info!("Exported {} paipu URLs to {:?}", urls.len(), output);
            }
            MajsoulCommands::FetchFullUuids { concurrent, limit, delay_ms } => {
                use futures::stream::{self, StreamExt};

                // First, populate throne_players from existing majsoul_logs if needed
                let player_count: i64 = db.conn_query_row(
                    "SELECT COUNT(*) FROM throne_players",
                )?;

                if player_count == 0 {
                    info!("Populating throne_players from existing logs...");
                    db.populate_throne_players()?;
                }

                let (total_players, fetched_players) = db.count_throne_players()?;
                info!("Throne players: {} total, {} fetched, {} remaining",
                    total_players, fetched_players, total_players - fetched_players);

                let players = db.get_unfetched_throne_players(limit)?;
                if players.is_empty() {
                    info!("No unfetched players remaining");
                    return Ok(());
                }

                info!("Fetching records for {} players ({} concurrent)...", players.len(), concurrent);

                let client = reqwest::Client::builder()
                    .user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36")
                    .timeout(std::time::Duration::from_secs(30))
                    .build()?;

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
                            let start_ms: i64 = 1262304000000; // 2010-01-01

                            loop {
                                // Descending mode: swap end/start, add descending=true, limit=500
                                let url = format!(
                                    "https://5-data.amae-koromo.com/api/v2/pl4/player_records/{}/{}/{}?mode=16&limit=500&descending=true",
                                    player_id, end_ms, start_ms
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

                // Get unfetched players (includes reset 200-limit players)
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
                                let result = client.get_player_records_paginated(player_id, 16).await;
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
                                        // INSERT with full_uuid (will update existing or add new)
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

                        if processed % 100 == 0 || processed == players.len() {
                            info!("Progress: {}/{} players | {} records | {} new full UUIDs | {} API calls",
                                processed, players.len(), total_records, total_new, total_api_calls);
                        }
                    }

                    info!("\nFetch complete. {} records, {} new full UUIDs", total_records, total_new);
                }

                // Phase 2: Cross-player matching by start_time
                info!("\n=== CROSS-PLAYER MATCHING ===");
                let before_orphans = db.count_orphaned_games()?;

                let matched = db.cross_match_orphan_uuids()?;

                let after_orphans = db.count_orphaned_games()?;
                info!("\n=== RECOVERY COMPLETE ===");
                info!("Cross-player matched: {}", matched);
                info!("Orphans before: {}", before_orphans);
                info!("Orphans after: {}", after_orphans);
                info!("Total recovered: {}", before_orphans - after_orphans);
            }
            MajsoulCommands::ResolveUuids { limit, concurrent, delay_ms, server } => {
                use crate::majsoul::browser::CachedToken;
                use crate::majsoul::gateway::discover_gateway;
                use crate::majsoul::rpc::{MajsoulRpc, extract_full_uuid_from_record};
                use futures::stream::{self, StreamExt};
                use std::sync::Arc;
                use std::sync::atomic::{AtomicUsize, Ordering};
                use tokio::sync::Mutex;

                // Load cached token
                let cached = match CachedToken::load()? {
                    Some(t) => t,
                    None => anyhow::bail!("No cached token. Run `majsoul auth` first."),
                };
                info!("Using cached token (server: {})", cached.server);

                // Get orphan UUIDs
                let orphans = db.get_orphan_short_uuids(limit, Some(16))?;
                if orphans.is_empty() {
                    info!("No orphan UUIDs to resolve!");
                    return Ok(());
                }

                info!("=== UUID RESOLUTION VIA RPC ===");
                info!("Orphans to resolve: {}", orphans.len());
                info!("Concurrent requests: {}", concurrent);
                info!("Server: {}", server);

                // Connect to gateway
                let client = reqwest::Client::new();
                let (endpoint, version) = discover_gateway(&client, &server).await?;
                info!("Gateway: {}", endpoint);

                let rpc = MajsoulRpc::connect(&endpoint).await?;
                rpc.login(&cached.access_token, &version, &server).await?;
                info!("Logged in successfully\n");

                // Wrap RPC and DB in Arc for sharing across concurrent tasks
                let rpc = Arc::new(rpc);
                let db = Arc::new(Mutex::new(db));
                let resolved = Arc::new(AtomicUsize::new(0));
                let failed = Arc::new(AtomicUsize::new(0));
                let processed = Arc::new(AtomicUsize::new(0));
                let total = orphans.len();

                // Process orphans with true parallelism using buffer_unordered
                stream::iter(orphans.into_iter().enumerate())
                    .map(|(i, short_uuid)| {
                        let rpc = Arc::clone(&rpc);
                        let db = Arc::clone(&db);
                        let resolved = Arc::clone(&resolved);
                        let failed = Arc::clone(&failed);
                        let processed = Arc::clone(&processed);

                        async move {
                            // Optional delay between batches
                            if delay_ms > 0 && i > 0 && i % concurrent == 0 {
                                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                            }

                            match rpc.fetch_game_record(&short_uuid).await {
                                Ok(data) => {
                                    match extract_full_uuid_from_record(&data) {
                                        Ok(full_uuid) => {
                                            let db_guard = db.lock().await;
                                            if db_guard.set_orphan_full_uuid(&short_uuid, &full_uuid).unwrap_or(false) {
                                                resolved.fetch_add(1, Ordering::Relaxed);
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
                            if current % 100 == 0 || current == total {
                                let db_guard = db.lock().await;
                                let remaining = db_guard.count_orphaned_games().unwrap_or(0);
                                info!(
                                    "Progress: {}/{} | Resolved: {} | Failed: {} | Remaining: {}",
                                    current, total,
                                    resolved.load(Ordering::Relaxed),
                                    failed.load(Ordering::Relaxed),
                                    remaining
                                );
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

                // Enable WAL mode for better concurrency
                db.enable_wal_mode()?;

                let start_date = NaiveDate::parse_from_str(&start, "%Y%m%d")?;
                let delay_ms = 1000 / rps as u64;

                info!("=== EXHAUSTIVE THRONE SCRAPER ===");
                info!("Start date: {}", start_date);
                info!("Rate: {} req/s ({}ms delay)", rps, delay_ms);
                info!("Running until no new games found...\n");

                let db = Arc::new(Mutex::new(db));
                let client = Arc::new(
                    reqwest::Client::builder()
                        .user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36")
                        .timeout(std::time::Duration::from_secs(30))
                        .build()?
                );
                let api_client = Arc::new(majsoul::AmaeKoromoClient::new(delay_ms)?);

                // Retry helper with exponential backoff
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
                                                anyhow::bail!("JSON parse failed after {} attempts: {}", attempt, e);
                                            }
                                            tracing::warn!("Parse error (attempt {}): {}", attempt, e);
                                        }
                                    }
                                } else if resp.status() == 429 {
                                    // Rate limited - wait longer
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
                                    anyhow::bail!("Network error after {} attempts: {}", attempt, e);
                                }
                                tracing::warn!("Network error (attempt {}): {}", attempt, e);
                            }
                        }
                        // Exponential backoff: 1s, 2s, 4s, 8s...
                        let backoff = std::time::Duration::from_secs(1 << (attempt - 1).min(4));
                        tokio::time::sleep(backoff).await;
                    }
                }

                let mut round = 0;
                loop {
                    round += 1;
                    let mut new_this_round = 0;

                    info!("=== Round {} ===", round);

                    // Phase 1: Date fetcher - get games by date range
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

                    if !dates_to_fetch.is_empty() {
                        info!("[Dates] {} unfetched dates to process", dates_to_fetch.len());

                        for date in dates_to_fetch {
                            let date_str = date.format("%Y-%m-%d").to_string();
                            let day_start_ms = date.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp_millis();
                            let day_end_ms = date.and_hms_opt(23, 59, 59).unwrap().and_utc().timestamp_millis();

                            // 6-hour chunks to avoid 500 cap
                            let chunk_ms: i64 = 6 * 60 * 60 * 1000;
                            let mut chunk_start = day_start_ms;
                            let mut day_new = 0;
                            let mut all_chunks_ok = true;

                            while chunk_start < day_end_ms {
                                let chunk_end = (chunk_start + chunk_ms).min(day_end_ms);
                                let url = format!(
                                    "https://5-data.amae-koromo.com/api/v2/pl4/games/{}/{}?mode=16&limit=500",
                                    chunk_start, chunk_end
                                );

                                match fetch_with_retry(&client, &url, 3).await {
                                    Ok(records) => {
                                        // Warn if we hit the cap
                                        if records.len() >= 500 {
                                            warn!("{} chunk hit 500 cap - may be missing records!", date_str);
                                        }
                                        let db_guard = db.lock().await;
                                        for r in &records {
                                            let player_id = r.players.first().map(|p| p.account_id).unwrap_or(0);
                                            if db_guard.insert_majsoul_log(&r.uuid, player_id, r.start_time, Some(r.mode_id))? {
                                                day_new += 1;
                                            }
                                            for p in &r.players {
                                                let _ = db_guard.upsert_throne_player(p.account_id, &p.nickname);
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        warn!("[Dates] {} chunk {}-{} FAILED: {}", date_str, chunk_start, chunk_end, e);
                                        all_chunks_ok = false;
                                    }
                                }
                                chunk_start = chunk_end;
                                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                            }

                            // Only mark as fetched if ALL chunks succeeded
                            if all_chunks_ok {
                                let db_guard = db.lock().await;
                                db_guard.mark_majsoul_room_fetched_with_count(&date_str, 16, day_new as i32)?;
                                if day_new > 0 {
                                    info!("[Dates] {}: {} new games", date_str, day_new);
                                }
                            } else {
                                warn!("[Dates] {} NOT marked complete due to failures - will retry next round", date_str);
                            }
                            new_this_round += day_new;
                        }
                    } else {
                        info!("[Dates] All dates already fetched");
                    }

                    // Phase 2: Player expander - BFS to get full UUIDs (WITH PAGINATION)
                    {
                        let players = {
                            let db = db.lock().await;
                            db.get_unfetched_throne_players(None)?
                        };

                        if !players.is_empty() {
                            info!("[Players] {} unfetched players to process (with pagination)", players.len());

                            let mut processed = 0;
                            let total_players = players.len();
                            let concurrent = 4; // Limit concurrent requests for pagination

                            for chunk in players.chunks(concurrent) {
                                let futures: Vec<_> = chunk.iter().map(|&player_id| {
                                    let api_client = api_client.clone();
                                    async move {
                                        let result = api_client.get_player_records_paginated(player_id, 16).await;
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
                                                // Discover new players
                                                for p in &r.players {
                                                    let _ = db_guard.upsert_throne_player(p.account_id, &p.nickname);
                                                }
                                            }
                                            let _ = db_guard.mark_throne_player_fetched(player_id);
                                        }
                                        Err(e) => {
                                            warn!("[Players] {} fetch error: {}", player_id, e);
                                        }
                                    }
                                }
                                drop(db_guard);
                                processed += chunk.len();
                                if processed % 100 == 0 || processed == total_players {
                                    info!("[Players] {}/{} processed, {} new full UUIDs", processed, total_players, new_this_round);
                                }
                            }
                        } else {
                            info!("[Players] All players already fetched");
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

                        let matched = db_guard.cross_match_orphan_uuids()?;

                        if matched > 0 {
                            info!("[Cross-match] Filled {} orphan full_uuids via timestamp matching", matched);
                            new_this_round += matched;
                        }
                    }

                    info!("\n[Round {} Summary]", round);
                    info!("  New games this round: {}", new_this_round);
                    info!("  Total games: {} ({} with full UUID)", total, with_full);
                    info!("  Players: {} ({} fetched)\n", total_players, fetched_players);

                    // Convergence check
                    if new_this_round == 0 {
                        info!("=== CONVERGENCE REACHED ===");
                        info!("No new games found. Scraping complete!");
                        info!("Total unique games with paipu: {}", with_full);
                        break;
                    }
                }
            }
            MajsoulCommands::ResetCappedPlayers => {
                let count = db.reset_capped_throne_players()?;
                info!("Reset {} players who hit the 200-game cap", count);
                info!("Run 'majsoul scrape-all' to re-fetch with pagination");
            }
            MajsoulCommands::BulkDownload { tokens, limit, delay_ms, restart_every } => {
                use crate::majsoul::token_pool::TokenPool;
                use crate::majsoul::parallel_download::ParallelDownloader;
                use std::sync::Arc;
                use tokio::sync::Mutex;

                // Enable WAL mode for concurrent writes
                db.enable_wal_mode()?;

                // Load token pool
                let pool = TokenPool::from_file(&tokens)?;
                if pool.is_empty() {
                    anyhow::bail!("No tokens found in {:?}", tokens);
                }
                info!("Loaded {} accounts from {:?}", pool.len(), tokens);

                // Check downloadable count
                let downloadable = db.count_majsoul_downloadable()?;
                info!("Downloadable records (with full_uuid): {}", downloadable);

                if downloadable == 0 {
                    info!("No records to download. Run 'majsoul fetch-full-uuids' first.");
                    return Ok(());
                }

                let db = Arc::new(Mutex::new(db));
                let downloader = ParallelDownloader::new(delay_ms, restart_every);

                let (success, failed) = downloader.download_with_pool(db, &pool, limit).await?;

                info!("Bulk download complete: {} success, {} failed", success, failed);
            }
            MajsoulCommands::ResolvePhantoms { limit, delay_ms, server } => {
                let phantoms = db.get_orphan_short_uuids(limit, Some(16))?;
                if phantoms.is_empty() {
                    info!("No phantom UUIDs to resolve");
                    return Ok(());
                }

                info!("Resolving {} phantom UUIDs via browser...", phantoms.len());

                let results = majsoul::browser::resolve_phantom_uuids(&server, &phantoms, delay_ms).await?;

                let mut resolved = 0;
                for (short, full) in results {
                    if db.set_orphan_full_uuid(&short, &full)? {
                        resolved += 1;
                    }
                }

                info!("Resolved {} phantom UUIDs", resolved);
            }
        },
    }

    Ok(())
}
