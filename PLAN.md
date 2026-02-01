# Mahjong AI Data Pipeline - Implementation Plan

## Project Goal

Build a unified Rust tool to scrape Tenhou houou (Phoenix) logs, convert them to MJAI format, and prepare datasets for training a mahjong AI that outperforms Mortal.

---

## Phase 1: Project Setup & Crate Vendoring

**Objective:** Set up the workspace and vendor all necessary Rust crates.

### Tasks

- [ ] Restructure project as Cargo workspace
- [ ] Vendor `mjlog` crate from mjlog2json (XML parser)
- [ ] Vendor `mjlog2json-core` crate (XML → tenhou JSON conversion)
- [ ] Vendor `tenhou-json` crate (JSON model definitions)
- [ ] Keep existing `convlog` crate (tenhou JSON → MJAI)
- [ ] Verify all crates compile together
- [ ] Write integration test: XML → JSON → MJAI pipeline

### Deliverables

```
tenhou-to-mjai/
├── Cargo.toml (workspace root)
├── crates/
│   ├── mjlog/           # XML parser
│   ├── mjlog2json-core/ # XML → JSON
│   ├── tenhou-json/     # JSON models
│   └── convlog/         # JSON → MJAI
└── src/
    └── main.rs          # CLI entry
```

### Success Criteria

- `cargo build --release` succeeds
- Integration test passes: sample XML → valid MJAI output

---

## Phase 2: Houou-logs Rust Port

**Objective:** Port the Python houou-logs functionality to Rust.

### Tasks

- [ ] Implement SQLite database module (`rusqlite`)
  - [ ] `logs` table schema
  - [ ] `file_index` table schema
  - [ ] CRUD operations for log entries
- [ ] Implement log ID import from yearly archives
  - [ ] Parse `scraw{year}.zip` format
  - [ ] Extract log IDs with metadata (players, length)
- [ ] Implement log ID fetching from daily files
  - [ ] Parse `scc{date}.log.gz` format
  - [ ] Rate-limited HTTP requests
- [ ] Implement log content download
  - [ ] Fetch from `tenhou.net/0/log/?{id}`
  - [ ] Gzip compress and store in DB
  - [ ] Resume capability (skip already downloaded)
- [ ] Implement log export from DB

### CLI Commands

```bash
# Import log IDs from yearly archive
tenhou-scraper import --db 2024.db --archive scraw2024.zip

# Fetch latest log IDs (past 7 days)
tenhou-scraper fetch --db 2025.db

# Download log content
tenhou-scraper download --db 2025.db --players 4 --length h --limit 1000

# Export XML from DB
tenhou-scraper export --db 2025.db --output ./xml/
```

### Success Criteria

- Can import log IDs from tenhou yearly archives
- Can download log content with proper rate limiting
- Resume capability works (no duplicate downloads)

---

## Phase 3: Unified Conversion Pipeline

**Objective:** Build the complete XML → MJAI pipeline in a single tool.

### Tasks

- [ ] Implement `convert` command
  - [ ] Read XML from DB or files
  - [ ] Parse with `mjlog::parse_mjlogs()`
  - [ ] Convert with `mjlog2json_core::conv_to_tenhou_json()`
  - [ ] Transform with `convlog::tenhou_to_mjai()`
  - [ ] Write gzipped `.mjson` output
- [ ] Add parallel processing (rayon)
- [ ] Add progress reporting
- [ ] Handle conversion errors gracefully (log and continue)
- [ ] Implement batch export with yearly packaging

### CLI Commands

```bash
# Convert all logs in DB to MJAI
tenhou-scraper convert --db 2025.db --output ./mjai/2025/

# Convert with filtering
tenhou-scraper convert --db 2025.db --output ./mjai/ --players 4 --length h

# Package yearly dataset
tenhou-scraper package --input ./mjai/2025/ --output 2025.zip
```

### Success Criteria

- Convert 1000 logs without crashing
- Output matches expected MJAI format
- Conversion speed: >100 logs/second

---

## Phase 4: Scraping 2025-2026 Tenhou Logs

**Objective:** Scrape and convert all missing houou logs.

### Tasks

- [ ] Download 2025 yearly archive when available (or use daily fetch)
- [ ] Fetch 2025 daily log IDs (scb + scc)
- [ ] Fetch 2026 daily log IDs (up to current date)
- [ ] Download all log content (with overnight runs if needed)
- [ ] Convert all to MJAI format
- [ ] Package as yearly datasets
- [ ] Update GitHub releases

### Data Targets

| Year | Source | Status |
|------|--------|--------|
| 2009-2024 | Already have | Complete |
| 2025 | Partial, need rest | In Progress |
| 2026 | Need all | Not Started |

### Estimated Scale

- ~500k-800k games per year (houou room)
- ~1-2GB compressed per year
- Download time: ~24-48 hours with rate limiting

### Success Criteria

- 2025 dataset complete
- 2026 dataset up to date
- All files validate correctly

---

## Phase 5: Mahjong Soul Integration (Future)

**Objective:** Add Mahjong Soul as additional data source.

### Tasks

- [ ] Research Majsoul protobuf API
- [ ] Implement websocket client for Majsoul
- [ ] Handle authentication (email code flow)
- [ ] Scrape replay data from amae-koromo stats
- [ ] Convert Majsoul replays to MJAI format
- [ ] Integrate with existing pipeline

### Technical Challenges

- Majsoul uses protobuf over websockets
- EN/JP servers need email/social auth
- Rate limiting and anti-bot measures
- Different replay format than Tenhou

### Resources

- `ms-api` Python package (reference implementation)
- `majsoul_api` Go package
- amae-koromo stats API

### Success Criteria

- Can authenticate to Majsoul
- Can download replays
- Can convert to MJAI format

---

## Timeline Estimate

| Phase | Effort |
|-------|--------|
| Phase 1 | 1 session |
| Phase 2 | 2-3 sessions |
| Phase 3 | 1-2 sessions |
| Phase 4 | Background (days) |
| Phase 5 | 3-5 sessions |

---

## Dependencies

### Rust Crates

- `rusqlite` - SQLite database
- `reqwest` - HTTP client
- `tokio` - Async runtime
- `flate2` - Gzip compression
- `quick-xml` - XML parsing (via mjlog)
- `serde` / `serde_json` - Serialization
- `clap` - CLI argument parsing
- `indicatif` - Progress bars
- `rayon` - Parallel processing
- `tracing` - Logging

### External Tools

- Tenhou yearly archives (`scraw{year}.zip`)
- Tenhou daily logs (`scc{date}.log.gz`)
- GitHub releases for dataset distribution

---

## Notes

- Only 4-player hanchan games for AI training
- Houou room = highest level play quality
- MJAI format is the standard for mahjong AI (used by Mortal, akochan)
- All code will be open source (Apache 2.0)
- Datasets under CC BY 4.0
