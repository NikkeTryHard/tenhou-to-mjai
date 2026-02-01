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
                start,
                end,
                delay_ms,
            } => {
                let start_date = NaiveDate::parse_from_str(&start, "%Y%m%d")?;
                let end_date = match end {
                    Some(e) => NaiveDate::parse_from_str(&e, "%Y%m%d")?,
                    None => chrono::Local::now().date_naive(),
                };

                let start_ms = start_date
                    .and_hms_opt(0, 0, 0)
                    .unwrap()
                    .and_utc()
                    .timestamp_millis();
                let end_ms = end_date
                    .and_hms_opt(23, 59, 59)
                    .unwrap()
                    .and_utc()
                    .timestamp_millis();

                let client = majsoul::AmaeKoromoClient::new(delay_ms)?;
                let records = client.get_player_records(player_id, start_ms, end_ms, mode).await?;

                info!("Found {} records", records.len());

                let mut new_count = 0;
                for r in &records {
                    if db.insert_majsoul_log(&r.uuid, player_id, r.start_time, Some(r.mode_id))? {
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
                    let server = server.unwrap_or_else(|| "cn".to_string());
                    info!("Using browser-based download for {} server", server);

                    let uuids = db.get_majsoul_undownloaded(limit)?;
                    if uuids.is_empty() {
                        info!("No pending downloads");
                        return Ok(());
                    }

                    info!("Downloading {} records via browser (login required)", uuids.len());
                    let mut success = 0;
                    let mut failed = 0;

                    for uuid in &uuids {
                        match majsoul::browser::fetch_game_record_via_browser(&server, uuid).await {
                            Ok(data) => {
                                if let Err(e) = db.mark_majsoul_downloaded(uuid, &data) {
                                    warn!("Failed to save {}: {}", uuid, e);
                                    failed += 1;
                                } else {
                                    success += 1;
                                    info!("Downloaded {} ({} bytes)", uuid, data.len());
                                }
                            }
                            Err(e) => {
                                warn!("Failed to fetch {}: {}", uuid, e);
                                db.mark_majsoul_download_error(uuid)?;
                                failed += 1;
                            }
                        }
                        if delay_ms > 0 {
                            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
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
        },
    }

    Ok(())
}
