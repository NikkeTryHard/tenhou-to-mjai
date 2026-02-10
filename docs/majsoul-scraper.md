# Majsoul Scraper

The `majsoul` subcommand group provides a complete pipeline for scraping, downloading, and converting Mahjong Soul (雀魂) game records to MJAI format.

All Majsoul commands use the same database (`-d` flag, defaults to `tenhou.db`).

## Data Pipeline Overview

```
Amae-Koromo API ──→ UUIDs in DB ──→ Protobuf download ──→ MJAI conversion
  (fetch-days)       (scrape-      (raw-download /        (convert-raw /
  (fetch-room)        players)      bulk-download)          convert)
```

## UUID Discovery Commands

### `majsoul fetch-days`

Phase 1 of the two-phase scrape. Fetches player IDs by querying daily game lists from the Amae-Koromo API.

```bash
tenhou-scraper majsoul fetch-days --start 20190801 --end 20261231
```

| Flag | Default | Description |
|------|---------|-------------|
| `--start` | required | Start date (YYYYMMDD) |
| `--end` | yesterday | End date (YYYYMMDD) |
| `--delay-ms` | 100 | Delay between API requests |

### `majsoul scrape-players`

Phase 2 of the two-phase scrape. Fetches full game history for each player discovered in Phase 1. Resumable — tracks which players have been scraped.

```bash
tenhou-scraper majsoul scrape-players --concurrent 5 --limit 1000
```

| Flag | Default | Description |
|------|---------|-------------|
| `--concurrent` | 5 | Parallel player fetches |
| `--limit` | all | Max players to scrape |
| `--delay-ms` | 200 | Delay between API requests |

### `majsoul scrape-all`

Exhaustive automated scraper that runs both phases in a loop until no new games are found.

```bash
tenhou-scraper majsoul scrape-all --rps 4 --start 20190801
```

| Flag | Default | Description |
|------|---------|-------------|
| `--rps` | 4 | Requests per second (keep under 5) |
| `--start` | 20190801 | Start date for date fetcher |

### `majsoul fetch-room`

Fetch game UUIDs for a specific room by date range using Amae-Koromo's room endpoint. Uses 6-hour chunks to avoid the API's 500-record cap.

```bash
tenhou-scraper majsoul fetch-room --room throne --start 20240101 --end 20241231
```

| Flag | Default | Description |
|------|---------|-------------|
| `--room` | throne | Room type: `throne`, `jade`, `gold` |
| `--start` | required | Start date (YYYYMMDD) |
| `--end` | today | End date (YYYYMMDD) |
| `--delay-ms` | 1000 | Delay between API requests |
| `--skip-fetched` | true | Skip dates already fetched |

### `majsoul fetch`

Fetch game UUIDs for a specific player by their Amae-Koromo player ID.

```bash
tenhou-scraper majsoul fetch --player-id 12345678 --mode 16 --start 20240101
```

| Flag | Default | Description |
|------|---------|-------------|
| `--player-id` | required | Amae-Koromo player ID |
| `--mode` | 16 | Room mode (16=Throne, 12=Jade, 9=Gold) |
| `--start` | required | Start date (YYYYMMDD) |
| `--end` | today | End date |
| `--delay-ms` | 300 | Delay between API requests |

### `majsoul search`

Search for a player by nickname using the Amae-Koromo API.

```bash
tenhou-scraper majsoul search "playername"
```

### `majsoul fetch-full-uuids`

Fetch full UUIDs by querying player records in parallel. Used to resolve short UUIDs to full UUIDs needed for download.

```bash
tenhou-scraper majsoul fetch-full-uuids --concurrent 10 --limit 500
```

### `majsoul recover-orphans`

Recover orphaned games (games with short UUIDs but no full UUID) by re-fetching player histories with pagination and cross-matching.

```bash
tenhou-scraper majsoul recover-orphans --concurrent 5 --limit 1000
```

## Download Commands

### `majsoul raw-download`

Multi-account raw protobuf download. Saves `.pb` files without conversion. Designed for bulk downloading with account rotation.

```bash
tenhou-scraper majsoul raw-download \
  --accounts accounts.txt \
  --password "shared_pass" \
  --todo todo.txt \
  --completed completed.log \
  --output jade-raw/ \
  --server cn
```

| Flag | Default | Description |
|------|---------|-------------|
| `--accounts` | accounts.txt | File with account emails (one per line) |
| `--password` | required | Shared password for all accounts |
| `--todo` | todo.txt | File with UUIDs to download (one per line) |
| `--completed` | completed.log | Append-only log of completed UUIDs |
| `--output` | jade-raw | Output directory for .pb files |
| `--server` | cn | Server region: `cn`, `en`, `jp` |
| `--limit` | all | Max games to download |
| `--delay-ms` | 300 | Delay between requests per worker |

### `majsoul bulk-download`

Bulk download with a single account. Automatically restarts the RPC connection periodically to prevent memory leaks.

```bash
tenhou-scraper majsoul bulk-download \
  --username user@example.com \
  --password "pass" \
  --server en \
  --limit 10000
```

| Flag | Default | Description |
|------|---------|-------------|
| `--username` | required | Login username |
| `--password` | required | Login password |
| `--server` | en | Server region |
| `--limit` | all | Max records to download |
| `--delay-ms` | 2000 | Delay between requests |
| `--restart-every` | 10000 | Restart RPC every N records |

### `majsoul download`

Single-account download. Simpler than `bulk-download`, without auto-restart.

```bash
tenhou-scraper majsoul download \
  --username user@example.com \
  --password "pass" \
  --server en \
  --limit 1000
```

### `majsoul download-json`

Download games and convert directly to Tenhou JSON format (intermediate format).

```bash
tenhou-scraper majsoul download-json \
  --output tenhou-json/ \
  --username user@example.com \
  --password "pass"
```

## Conversion Commands

### `majsoul convert-raw`

Convert raw `.pb` protobuf files to MJAI format. Does not require a database — works directly on files.

```bash
tenhou-scraper majsoul convert-raw --input jade-raw/ --output jade-mjai/

# Delete .pb files after successful conversion
tenhou-scraper majsoul convert-raw --input jade-raw/ --output jade-mjai/ --delete
```

| Flag | Default | Description |
|------|---------|-------------|
| `--input` | jade-raw | Directory containing .pb files |
| `--output` | jade-mjai | Output directory for .mjai.json files |
| `--delete` | false | Delete .pb files after conversion |

### `majsoul convert`

Convert downloaded Majsoul logs from the database to MJAI format.

```bash
tenhou-scraper majsoul convert --output mjai-majsoul/ --players 4 --hanchan
```

| Flag | Default | Description |
|------|---------|-------------|
| `--output` | mjai-majsoul | Output directory |
| `--limit` | all | Max logs to convert |
| `--players` | any | Filter by player count |
| `--hanchan` | false | Only convert hanchan games |

## UUID Resolution Commands

### `majsoul resolve-uuids`

Resolve short UUIDs to full UUIDs via Majsoul RPC. Requires authentication.

```bash
tenhou-scraper majsoul resolve-uuids \
  --username user@example.com \
  --password "pass" \
  --concurrent 4 \
  --limit 5000
```

### `majsoul resolve-paipu`

Resolve short UUIDs to full paipu URLs via the Amae-Koromo redirect API.

```bash
tenhou-scraper majsoul resolve-paipu --limit 5000 --delay-ms 300
```

### `majsoul resolve-phantoms`

Resolve phantom UUIDs via browser injection. Last-resort for UUIDs that can't be resolved through RPC or API.

```bash
tenhou-scraper majsoul resolve-phantoms --limit 1000 --server en
```

## Utility Commands

### `majsoul stats`

Show full pipeline statistics: day fetch progress, player scrape progress, game log counts by room.

```bash
tenhou-scraper majsoul stats
```

### `majsoul export-paipu`

Export resolved paipu URLs to a text file.

```bash
tenhou-scraper majsoul export-paipu --output paipu_urls.txt
```

### `majsoul reset-capped-players`

Reset fetch status for players who hit the 200-game API cap, allowing them to be re-scraped with pagination.

```bash
tenhou-scraper majsoul reset-capped-players
```

### `majsoul fetch-public`

Fetch public game records from ranked rooms. Requires authentication. Primarily used for debugging/testing the RPC connection.

```bash
tenhou-scraper majsoul fetch-public \
  --room throne \
  --count 100 \
  --username user@example.com \
  --password "pass"
```
