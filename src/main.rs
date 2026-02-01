use anyhow::{Context, Result};
use chrono::NaiveDate;
use clap::Parser;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use indicatif::{ProgressBar, ProgressStyle};
use regex::Regex;
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::time::Duration;
use tracing::{info, warn};

#[derive(Parser, Debug)]
#[command(name = "tenhou-scraper")]
#[command(about = "Scrape Tenhou logs and convert to MJAI format")]
struct Args {
    /// Start date (YYYYMMDD)
    #[arg(short, long)]
    start: String,

    /// End date (YYYYMMDD), defaults to today
    #[arg(short, long)]
    end: Option<String>,

    /// Output directory
    #[arg(short, long, default_value = "output")]
    output: PathBuf,

    /// Log types to scrape (sca=private, scb=rank, scc=houou, scd=jansou, sce=amber)
    #[arg(short = 't', long, default_value = "scb,scc")]
    log_types: String,

    /// Only download 4-player hanchan games
    #[arg(long, default_value = "true")]
    four_player_hanchan: bool,

    /// Rate limit delay between requests (ms)
    #[arg(long, default_value = "100")]
    delay_ms: u64,

    /// Skip already downloaded logs
    #[arg(long, default_value = "true")]
    skip_existing: bool,
}

const TENHOU_RAW_BASE: &str = "https://tenhou.net/sc/raw/dat";
const TENHOU_LOG_BASE: &str = "https://tenhou.net/5/mjlog2json.cgi";

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("tenhou_scraper=info".parse()?),
        )
        .init();

    let args = Args::parse();

    let start_date = NaiveDate::parse_from_str(&args.start, "%Y%m%d")
        .context("Invalid start date format, use YYYYMMDD")?;

    let end_date = match &args.end {
        Some(e) => NaiveDate::parse_from_str(e, "%Y%m%d")
            .context("Invalid end date format, use YYYYMMDD")?,
        None => chrono::Local::now().date_naive(),
    };

    let log_types: Vec<&str> = args.log_types.split(',').collect();

    info!(
        "Scraping from {} to {}, types: {:?}",
        start_date, end_date, log_types
    );

    fs::create_dir_all(&args.output)?;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;

    // Collect all log IDs first
    let mut all_log_ids: Vec<String> = Vec::new();
    let mut current = start_date;

    while current <= end_date {
        let date_str = current.format("%Y%m%d").to_string();

        for log_type in &log_types {
            let filename = format!("{}{}.log.gz", log_type, date_str);
            let url = format!("{}/{}", TENHOU_RAW_BASE, filename);

            match fetch_log_ids(&client, &url, args.four_player_hanchan).await {
                Ok(ids) => {
                    info!("Found {} logs for {} {}", ids.len(), log_type, date_str);
                    all_log_ids.extend(ids);
                }
                Err(e) => {
                    warn!("Failed to fetch {}: {}", url, e);
                }
            }

            tokio::time::sleep(Duration::from_millis(args.delay_ms)).await;
        }

        current = current.succ_opt().unwrap();
    }

    info!("Total log IDs collected: {}", all_log_ids.len());

    // Filter out existing if requested
    let log_ids: Vec<String> = if args.skip_existing {
        let existing = get_existing_logs(&args.output)?;
        all_log_ids
            .into_iter()
            .filter(|id| !existing.contains(id))
            .collect()
    } else {
        all_log_ids
    };

    info!("Logs to download: {}", log_ids.len());

    if log_ids.is_empty() {
        info!("Nothing to download!");
        return Ok(());
    }

    // Download and convert
    let pb = ProgressBar::new(log_ids.len() as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({eta})")?
            .progress_chars("#>-"),
    );

    for log_id in log_ids {
        match download_and_convert(&client, &log_id, &args.output).await {
            Ok(_) => {}
            Err(e) => {
                warn!("Failed to process {}: {}", log_id, e);
            }
        }
        pb.inc(1);
        tokio::time::sleep(Duration::from_millis(args.delay_ms)).await;
    }

    pb.finish_with_message("Done!");
    Ok(())
}

async fn fetch_log_ids(
    client: &reqwest::Client,
    url: &str,
    four_player_hanchan: bool,
) -> Result<Vec<String>> {
    let response = client.get(url).send().await?;
    let bytes = response.bytes().await?;

    let mut decoder = GzDecoder::new(&bytes[..]);
    let mut content = String::new();
    decoder.read_to_string(&mut content)?;

    // Log format example:
    // 四鳳南喰赤|2025010100gm-00a9-0000-12345678|...
    // The log ID is in the second column

    let re = Regex::new(r"(\d{10}gm-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{8})")?;

    let mut log_ids = Vec::new();

    for line in content.lines() {
        // Filter for 4-player hanchan if requested
        // 00a9 = 4-player hanchan with red dora
        // 0029 = 4-player hanchan without red dora
        if four_player_hanchan {
            if !line.contains("gm-00a9-") && !line.contains("gm-0029-") {
                continue;
            }
        }

        if let Some(cap) = re.captures(line) {
            log_ids.push(cap[1].to_string());
        }
    }

    Ok(log_ids)
}

fn get_existing_logs(output_dir: &PathBuf) -> Result<HashSet<String>> {
    let mut existing = HashSet::new();

    if !output_dir.exists() {
        return Ok(existing);
    }

    for entry in fs::read_dir(output_dir)? {
        let entry = entry?;
        let filename = entry.file_name();
        let name = filename.to_string_lossy();
        if name.ends_with(".mjson") {
            // Extract log ID from filename
            let id = name.trim_end_matches(".mjson");
            existing.insert(id.to_string());
        }
    }

    Ok(existing)
}

async fn download_and_convert(
    client: &reqwest::Client,
    log_id: &str,
    output_dir: &PathBuf,
) -> Result<()> {
    // Fetch JSON log from tenhou
    let url = format!("{}?{}", TENHOU_LOG_BASE, log_id);
    let response = client.get(&url).send().await?;

    if !response.status().is_success() {
        anyhow::bail!("HTTP {}", response.status());
    }

    let json_text = response.text().await?;

    // Parse and convert using convlog
    let log = convlog::tenhou::Log::from_json_str(&json_text)?;
    let events = convlog::tenhou_to_mjai(&log)?;

    // Write gzipped MJAI output
    let output_path = output_dir.join(format!("{}.mjson", log_id));
    let file = File::create(&output_path)?;
    let mut encoder = GzEncoder::new(file, Compression::default());

    for event in events {
        let line = serde_json::to_string(&event)?;
        writeln!(encoder, "{}", line)?;
    }

    encoder.finish()?;

    Ok(())
}
