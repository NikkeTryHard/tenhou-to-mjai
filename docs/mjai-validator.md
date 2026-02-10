# mjai-validator

Rust-based streaming validator for MJAI dataset archives. Validates every `.mjai.json` file inside `.tar.zst` archives without extracting to disk.

## Installation

```bash
cd dataset/mjai-validator
cargo build --release
```

Binary: `../../target/release/mjai-validator`

## Usage

### Validate

Streams through all `.tar.zst` archives in the given directory, validates every file, and reports results.

```bash
mjai-validator /path/to/dataset/mjai
```

Memory usage is constant (~single file buffer) regardless of archive size.

### Clean

Removes invalid files from archives and rewrites them in-place. Uses a staging directory for safe atomic replacement.

```bash
mjai-validator --clean /path/to/dataset/mjai
```

## Validation Checks

Each `.mjai.json` file is checked for:

| Check | Description |
|-------|-------------|
| Valid JSON | Every line parses as a JSON object |
| Event type | Every event has a `type` field with a known MJAI event type |
| Game structure | First event is `start_game`, last is `end_game` |
| Kyoku balance | Every `start_kyoku` has a matching `end_kyoku` |
| Tile validity | All tile strings (`pai`, `dora_marker`, `consumed`, `ura_markers`, `tehais`) are valid mahjong tiles |
| Actor range | All `actor` values are 0-3 |
| Duplicate detection | Warns on consecutive identical lines (corruption signal) |

## Output

Prints per-archive results to stderr and a final summary table to stdout:

```
PER-SOURCE BREAKDOWN:
Source                         Files      Valid    Invalid           Lines
---------------------------------------------------------------------------
tenhou-houou                 2512433    2512433          0      2718655390 ✅
```
