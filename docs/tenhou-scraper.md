# Tenhou Scraper

The base `tenhou-scraper` commands handle the Tenhou.net (天鳳) phoenix room (鳳凰卓) game log pipeline: fetch log IDs → download XML → convert to MJAI → package.

## Installation

```bash
git clone https://github.com/NikkeTryHard/tenhou-to-mjai.git
cd tenhou-to-mjai
cargo build --release
```

Binary: `./target/release/tenhou-scraper`

## Database

All commands use a SQLite database (default: `tenhou.db`). Override with `-d`:

```bash
tenhou-scraper -d custom.db stats
```

## Commands

### `fetch`

Scrape daily HTML.gz index files from Tenhou to collect game log IDs.

```bash
tenhou-scraper fetch --start 20250101 --end 20251231
```

| Flag | Default | Description |
|------|---------|-------------|
| `--start` | required | Start date (YYYYMMDD) |
| `--end` | today | End date (YYYYMMDD) |
| `--log-types` | scc | Log types (comma-separated, scc=houou) |
| `--delay-ms` | 200 | Delay between requests |
| `--concurrent` | 1 | Number of concurrent date fetches |
| `--skip-fetched` | true | Skip already fetched dates |

### `download`

Download the actual game XML data for all pending log IDs.

```bash
tenhou-scraper download --limit 1000 --concurrent 4
```

| Flag | Default | Description |
|------|---------|-------------|
| `--limit` | all | Max logs to download |
| `--delay-ms` | 200 | Delay between requests |
| `--concurrent` | 1 | Number of concurrent downloads |

### `convert`

Convert downloaded Tenhou XML logs to MJAI format. Parallelized with rayon.

```bash
tenhou-scraper convert --output mjai/ --players 4 --hanchan
```

| Flag | Default | Description |
|------|---------|-------------|
| `--output` | mjai | Output directory for MJAI files |
| `--limit` | all | Max logs to convert |
| `--players` | any | Filter by player count (e.g., 4) |
| `--hanchan` | false | Only convert hanchan (full) games |

### `stats`

Show database statistics: total log IDs, downloaded, converted, pending.

```bash
tenhou-scraper stats
```

### `export`

Export raw XML logs from the database to individual files.

```bash
tenhou-scraper export --output xml/ --limit 100
```

### `package`

Bundle converted MJAI files into a distributable zip archive.

```bash
tenhou-scraper package --input mjai/ --output houou-2025.zip
```

## Full Pipeline

```bash
# 1. Fetch all log IDs for a year
tenhou-scraper fetch --start 20250101 --end 20251231

# 2. Download XML content (hours with rate limiting)
tenhou-scraper download

# 3. Convert to MJAI format
tenhou-scraper convert --output mjai/ --players 4 --hanchan

# 4. Package for distribution
tenhou-scraper package --input mjai/ --output houou-2025.zip
```
