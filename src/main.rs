mod convert;
mod db;
mod download;
mod export;
mod fetch;

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

    /// Export XML logs from database to files
    Export {
        /// Output directory for XML files
        #[arg(short, long, default_value = "xml")]
        output: PathBuf,

        /// Maximum logs to export (default: all)
        #[arg(short, long)]
        limit: Option<usize>,
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

        Commands::Export { output, limit } => {
            let (success, failed) = export::export_logs(&db, &output, limit)?;
            info!("Exported {} logs ({} failed)", success, failed);
        }
    }

    Ok(())
}
