# Majsoul Integration Plan

> **REQUIRED:** Use `execute-plan` to implement this plan batch by batch.

**Goal:** Add Mahjong Soul (雀魂) log fetching and conversion to the existing `tenhou-scraper` CLI.
**Architecture:** Three-phase approach - Discovery → Download → Convert

---

## Overview

```
┌─────────────────────────────────────────────────────────────────────────┐
│                         MAJSOUL → MJAI PIPELINE                         │
├─────────────────────────────────────────────────────────────────────────┤
│                                                                         │
│  Phase 1: Discovery (amae-koromo API)                                   │
│  ┌──────────────────────────────────────────────────────────────────┐  │
│  │ GET /search_player/{name} → player_id                            │  │
│  │ GET /player_records/{id}/{start}/{end}?mode=16 → UUIDs           │  │
│  │ Store in SQLite (new majsoul_logs table)                         │  │
│  └──────────────────────────────────────────────────────────────────┘  │
│                                    │                                    │
│                                    ▼                                    │
│  Phase 2: Download (Majsoul WebSocket)                                  │
│  ┌──────────────────────────────────────────────────────────────────┐  │
│  │ Connect to wss://gateway.maj-soul.com                            │  │
│  │ Login with access_token (oauth2Login RPC)                        │  │
│  │ Fetch logs (fetchGameRecord RPC)                                 │  │
│  │ Decode Protobuf responses (dynamic schema from liqi.json)        │  │
│  └──────────────────────────────────────────────────────────────────┘  │
│                                    │                                    │
│                                    ▼                                    │
│  Phase 3: Convert (Majsoul → Tenhou → MJAI)                            │
│  ┌──────────────────────────────────────────────────────────────────┐  │
│  │ Port tensoul/convert.js logic to Rust                            │  │
│  │ Map Majsoul events → tenhou.net/6 JSON                           │  │
│  │ Use existing convlog crate → MJAI output                         │  │
│  └──────────────────────────────────────────────────────────────────┘  │
│                                                                         │
└─────────────────────────────────────────────────────────────────────────┘
```

---

## Phase 1: Discovery via amae-koromo API

**Complexity:** Easy
**Auth Required:** No
**Estimated Effort:** 2-3 hours

### API Endpoints

| Endpoint | URL | Returns |
|----------|-----|---------|
| Player Search | `GET https://amae-koromo.sapk.ch/api/v2/pl4/search_player/{nickname}?limit=20` | `[{id, nickname, level}]` |
| Player Records | `GET https://5-data.amae-koromo.com/api/v2/pl4/player_records/{player_id}/{start_ms}/{end_ms}?mode={mode}` | `[{uuid, startTime, players}]` |

### Mode Values

| Mode | Room |
|------|------|
| 16 | Throne (王座) |
| 12 | Jade (玉) |
| 9 | Gold (金) |

### CLI Commands

```bash
# Search for a player
tenhou-scraper majsoul search "PlayerName"

# Fetch game UUIDs for a player
tenhou-scraper majsoul fetch --player-id 12345 --mode 16 --start 20250101 --end 20251231

# Show stats
tenhou-scraper majsoul stats
```

### Database Schema Addition

```sql
CREATE TABLE IF NOT EXISTS majsoul_players (
    id INTEGER PRIMARY KEY,
    nickname TEXT NOT NULL,
    level_id INTEGER,
    created_at TEXT DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS majsoul_logs (
    uuid TEXT PRIMARY KEY,
    player_id INTEGER NOT NULL,
    start_time INTEGER NOT NULL,
    mode_id INTEGER,
    is_downloaded INTEGER DEFAULT 0,
    is_converted INTEGER DEFAULT 0,
    raw_data BLOB,
    created_at TEXT DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (player_id) REFERENCES majsoul_players(id)
);
```

### Files to Create/Modify

| File | Action | Purpose |
|------|--------|---------|
| `src/majsoul/mod.rs` | Create | Module root |
| `src/majsoul/api.rs` | Create | amae-koromo API client |
| `src/majsoul/types.rs` | Create | Response types |
| `src/db.rs` | Modify | Add majsoul tables |
| `src/main.rs` | Modify | Add majsoul subcommand |

### Dependencies

```toml
# Already have these
reqwest = { version = "0.12", features = ["gzip", "json"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
```

---

## Phase 2: Download via Majsoul WebSocket

**Complexity:** Hard
**Auth Required:** Yes (access_token from browser)
**Estimated Effort:** 8-12 hours

### Protocol Flow (from tensoul analysis)

```
1. GET https://game.maj-soul.com/1/version.json
   → { version: "0.x.x", liqi: <protobuf schema as JSON> }

2. Discover gateway via service discovery endpoints
   → wss://gateway-v2.maj-soul.com/...

3. WebSocket connect with headers:
   - Origin: https://game.maj-soul.com
   - User-Agent: browser UA

4. RPC: oauth2Login
   Request:  { client_version_string, access_token }
   Response: { account_id, ... }

5. RPC: fetchGameRecord
   Request:  { game_uuid }
   Response: { data: <protobuf bytes>, head: { accounts } }

6. Decode protobuf wrapper chain:
   - Wrapper.decode(data) → { name, data }
   - Iterate actions, decode each by type
```

### CLI Commands

```bash
# Download logs (requires token)
tenhou-scraper majsoul download --token "ACCESS_TOKEN_FROM_BROWSER"

# Or use config file
tenhou-scraper majsoul download --config ~/.config/majsoul/credentials.json
```

### Dependencies (New)

```toml
tokio-tungstenite = "0.21"     # WebSocket client
prost = "0.12"                  # Protobuf decoding
prost-reflect = "0.13"          # Runtime protobuf (dynamic schema)
```

### Key Challenges

1. **Dynamic Protobuf Schema**: Majsoul serves `liqi.json` at runtime. Schema changes with game updates. Need `prost-reflect` for runtime parsing.

2. **WebSocket RPC**: Custom message framing - index byte + protobuf payload.

3. **Auth Token**: User must extract `GameMgr.Inst.access_token` from browser devtools.

4. **Rate Limiting**: Majsoul bans aggressive scrapers (error 151). Need careful delays.

### Files to Create

| File | Action | Purpose |
|------|--------|---------|
| `src/majsoul/gateway.rs` | Create | WebSocket client |
| `src/majsoul/rpc.rs` | Create | RPC message handling |
| `src/majsoul/proto.rs` | Create | Dynamic protobuf parsing |
| `src/majsoul/download.rs` | Create | Download orchestration |

---

## Phase 3: Convert Majsoul → MJAI

**Complexity:** Medium
**Auth Required:** No
**Estimated Effort:** 4-6 hours

### Conversion Chain

```
Majsoul Protobuf Events
        │
        ▼ (port tensoul/convert.js)
tenhou.net/6 JSON Format
        │
        ▼ (existing convlog crate)
MJAI JSON Lines
```

### Key Mappings (from tensoul/convert.js)

| Majsoul Event | Tenhou Equivalent |
|---------------|-------------------|
| `RecordNewRound` | Round start (haipai) |
| `RecordDealTile` | Tsumo |
| `RecordDiscardTile` | Dahai |
| `RecordChiPengGang` | Chi/Pon/Kan |
| `RecordAnGangAddGang` | Ankan/Kakan |
| `RecordHule` | Hora (win) |
| `RecordNoTile` | Ryuukyoku (draw) |

### CLI Commands

```bash
# Convert downloaded logs to MJAI
tenhou-scraper majsoul convert --output mjai/

# With filters
tenhou-scraper majsoul convert --output mjai/ --players 4 --hanchan
```

### Files to Create

| File | Action | Purpose |
|------|--------|---------|
| `src/majsoul/convert.rs` | Create | Majsoul → Tenhou converter |
| `src/majsoul/events.rs` | Create | Majsoul event types |

---

## Implementation Order

| Phase | Batch | Tasks | Description |
|-------|-------|-------|-------------|
| 1 | 1 | 2 | Create majsoul module structure |
| 1 | 2 | 3 | Implement amae-koromo API client |
| 1 | 3 | 2 | Add database schema + CLI commands |
| 1 | 4 | 1 | Integration test |
| 2 | 5 | 2 | WebSocket client + gateway discovery |
| 2 | 6 | 3 | Protobuf parsing with prost-reflect |
| 2 | 7 | 2 | RPC implementation (login, fetchGameRecord) |
| 2 | 8 | 2 | Download orchestration + error handling |
| 3 | 9 | 3 | Port convert.js tile/event mappings |
| 3 | 10 | 2 | Wire up to convlog + MJAI output |
| 3 | 11 | 1 | End-to-end integration test |

**Total: 23 tasks across 11 batches**

---

## Phase 1 Detailed Batches

### Batch 1.1: Create Module Structure

**Goal:** Set up the majsoul module skeleton.

#### Task 1.1.1: Create majsoul module files

**Files:**
- Create: `src/majsoul/mod.rs`
- Create: `src/majsoul/types.rs`

**Step 1: Create mod.rs**

```rust
pub mod api;
pub mod types;
```

**Step 2: Create types.rs**

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerSearchResult {
    pub id: i64,
    pub nickname: String,
    pub level: Option<PlayerLevel>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerLevel {
    pub id: i32,
    pub score: i32,
    pub delta: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameRecord {
    pub uuid: String,
    #[serde(rename = "startTime")]
    pub start_time: i64,
    #[serde(rename = "modeId")]
    pub mode_id: Option<i32>,
}
```

**Step 3: Verify**
Run: `cargo check`
Expected: Warning about unused module

**Step 4: Commit**
```bash
git add src/majsoul/
git commit -m "feat(majsoul): create module structure"
```

---

### Batch 1.2: Implement API Client

**Goal:** Create the amae-koromo API client.

#### Task 1.2.1: Create api.rs

**Files:**
- Create: `src/majsoul/api.rs`

**Implementation:**

```rust
use anyhow::{Context, Result};
use tracing::info;

use super::types::{GameRecord, PlayerSearchResult};

const SEARCH_BASE: &str = "https://amae-koromo.sapk.ch/api/v2/pl4";
const DATA_BASE: &str = "https://5-data.amae-koromo.com/api/v2/pl4";

pub struct AmaeKoromoClient {
    client: reqwest::Client,
    delay_ms: u64,
}

impl AmaeKoromoClient {
    pub fn new(delay_ms: u64) -> Self {
        Self {
            client: reqwest::Client::new(),
            delay_ms,
        }
    }

    pub async fn search_player(&self, nickname: &str) -> Result<Vec<PlayerSearchResult>> {
        let url = format!("{}/search_player/{}?limit=20", SEARCH_BASE, nickname);
        info!("Searching for player: {}", nickname);

        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .context("Failed to search player")?;

        let results: Vec<PlayerSearchResult> = resp.json().await?;
        Ok(results)
    }

    pub async fn get_player_records(
        &self,
        player_id: i64,
        start_ms: i64,
        end_ms: i64,
        mode: Option<i32>,
    ) -> Result<Vec<GameRecord>> {
        let mut url = format!(
            "{}/player_records/{}/{}/{}",
            DATA_BASE, player_id, start_ms, end_ms
        );

        if let Some(m) = mode {
            url.push_str(&format!("?mode={}", m));
        }

        info!("Fetching records for player {}", player_id);

        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .context("Failed to fetch player records")?;

        let records: Vec<GameRecord> = resp.json().await?;

        // Rate limiting
        tokio::time::sleep(std::time::Duration::from_millis(self.delay_ms)).await;

        Ok(records)
    }
}
```

**Step 2: Update mod.rs**
```rust
pub mod api;
pub mod types;

pub use api::AmaeKoromoClient;
```

**Step 3: Verify**
Run: `cargo check`
Expected: Compiles (with unused warnings)

**Step 4: Commit**
```bash
git add src/majsoul/
git commit -m "feat(majsoul): implement amae-koromo API client"
```

---

### Batch 1.3: Database Schema + CLI

**Goal:** Add majsoul tables and CLI subcommand.

#### Task 1.3.1: Add majsoul tables to db.rs

**Files:**
- Modify: `src/db.rs`

**Add to init():**
```rust
// Majsoul tables
conn.execute(
    "CREATE TABLE IF NOT EXISTS majsoul_players (
        id INTEGER PRIMARY KEY,
        nickname TEXT NOT NULL,
        level_id INTEGER,
        created_at TEXT DEFAULT CURRENT_TIMESTAMP
    )",
    [],
)?;

conn.execute(
    "CREATE TABLE IF NOT EXISTS majsoul_logs (
        uuid TEXT PRIMARY KEY,
        player_id INTEGER NOT NULL,
        start_time INTEGER NOT NULL,
        mode_id INTEGER,
        is_downloaded INTEGER DEFAULT 0,
        is_converted INTEGER DEFAULT 0,
        raw_data BLOB,
        created_at TEXT DEFAULT CURRENT_TIMESTAMP
    )",
    [],
)?;
```

**Add helper methods:**
```rust
pub fn insert_majsoul_player(&self, id: i64, nickname: &str) -> Result<()> {
    self.conn.execute(
        "INSERT OR IGNORE INTO majsoul_players (id, nickname) VALUES (?1, ?2)",
        params![id, nickname],
    )?;
    Ok(())
}

pub fn insert_majsoul_log(&self, uuid: &str, player_id: i64, start_time: i64, mode_id: Option<i32>) -> Result<()> {
    self.conn.execute(
        "INSERT OR IGNORE INTO majsoul_logs (uuid, player_id, start_time, mode_id) VALUES (?1, ?2, ?3, ?4)",
        params![uuid, player_id, start_time, mode_id],
    )?;
    Ok(())
}

pub fn get_majsoul_stats(&self) -> Result<(usize, usize, usize)> {
    let total: usize = self.conn.query_row(
        "SELECT COUNT(*) FROM majsoul_logs",
        [],
        |row| row.get(0),
    )?;
    let downloaded: usize = self.conn.query_row(
        "SELECT COUNT(*) FROM majsoul_logs WHERE is_downloaded = 1",
        [],
        |row| row.get(0),
    )?;
    let converted: usize = self.conn.query_row(
        "SELECT COUNT(*) FROM majsoul_logs WHERE is_converted = 1",
        [],
        |row| row.get(0),
    )?;
    Ok((total, downloaded, converted))
}
```

#### Task 1.3.2: Add CLI subcommand

**Files:**
- Modify: `src/main.rs`

**Add to Commands enum:**
```rust
    /// Mahjong Soul (Majsoul) operations
    #[command(subcommand)]
    Majsoul(MajsoulCommands),
```

**Add new enum:**
```rust
#[derive(Subcommand)]
enum MajsoulCommands {
    /// Search for a player by nickname
    Search {
        /// Player nickname to search
        nickname: String,
    },

    /// Fetch game records for a player
    Fetch {
        /// Player ID from amae-koromo
        #[arg(long)]
        player_id: i64,

        /// Room mode (16=Throne, 12=Jade, 9=Gold)
        #[arg(long, default_value = "16")]
        mode: i32,

        /// Start date (YYYYMMDD)
        #[arg(long)]
        start: Option<String>,

        /// End date (YYYYMMDD)
        #[arg(long)]
        end: Option<String>,
    },

    /// Show Majsoul stats
    Stats,
}
```

**Add match arm:**
```rust
Commands::Majsoul(cmd) => match cmd {
    MajsoulCommands::Search { nickname } => {
        let client = majsoul::AmaeKoromoClient::new(300);
        let results = client.search_player(&nickname).await?;
        for p in results {
            println!("{}: {} (ID: {})", p.nickname, p.level.map_or(0, |l| l.id), p.id);
        }
    }
    MajsoulCommands::Fetch { player_id, mode, start, end } => {
        let client = majsoul::AmaeKoromoClient::new(300);
        // Parse dates to timestamps...
        let records = client.get_player_records(player_id, start_ms, end_ms, Some(mode)).await?;
        info!("Found {} records", records.len());
        for r in &records {
            db.insert_majsoul_log(&r.uuid, player_id, r.start_time, r.mode_id)?;
        }
        info!("Stored {} UUIDs in database", records.len());
    }
    MajsoulCommands::Stats => {
        let (total, downloaded, converted) = db.get_majsoul_stats()?;
        println!("Majsoul logs: {} total, {} downloaded, {} converted", total, downloaded, converted);
    }
},
```

---

## Success Criteria

### Phase 1
- [ ] `majsoul search` returns player IDs
- [ ] `majsoul fetch` stores UUIDs in database
- [ ] `majsoul stats` shows correct counts
- [ ] Rate limiting prevents bans (300ms delay)

### Phase 2
- [ ] WebSocket connects to Majsoul gateway
- [ ] Login succeeds with valid token
- [ ] fetchGameRecord returns protobuf data
- [ ] Protobuf decodes correctly with dynamic schema

### Phase 3
- [ ] Majsoul events map to tenhou format
- [ ] convlog produces valid MJAI output
- [ ] Full pipeline: UUID → downloaded → converted → .mjson.gz

---

## References

- [tensoul](https://github.com/Equim-chan/tensoul) - Node.js reference implementation
- [mjai-reviewer](https://github.com/Equim-chan/mjai-reviewer) - Rust convlog crate
- [amae-koromo](https://github.com/SAPikachu/amae-koromo) - Stats site source
- [mjsoul npm](https://www.npmjs.com/package/mjsoul) - WebSocket client reference
