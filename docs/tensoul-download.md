# tensoul-download

Python-based Majsoul game downloader and converter. Uses [tensoul-py-ng](https://github.com/unStatiK/tensoul-py-ng) for protobuf handling.

This toolset is designed for high-volume Majsoul Jade room downloads where the Rust CLI's `raw-download` handles the actual network I/O and these scripts handle conversion and orchestration.

## Prerequisites

```bash
cd tensoul-download
uv sync
```

## Tools

### `async_downloader.py`

Multi-account asyncio downloader. Reads UUIDs from a journal file and distributes downloads across multiple Majsoul accounts.

```bash
uv run python async_downloader.py \
  --accounts accounts.txt \
  --password "shared_pass" \
  --todo todo.txt \
  --completed completed.log \
  --output jade-raw/
```

The `accounts.txt` file contains one email per line. All accounts share the same password.

State tracking uses an append-only journal (`completed.log`) instead of SQLite to avoid contention during parallel downloads.

### `convert_pb.py`

Batch converter: `.pb` (Majsoul protobuf) → Tenhou JSON (via tensoul) → MJAI (via `convlog` binary). Runs as a multiprocessing pipeline alongside the active download.

```bash
uv run python convert_pb.py
```

Deletes `.pb` files after successful MJAI conversion to save disk space.

### `download.py`

Simple single-account downloader. Downloads games as Tenhou JSON format.

```bash
uv run python download.py <username> <password> <uuid>
```

### `export_uuids.py`

One-time export: dumps all downloadable UUIDs from the SQLite database to a `todo.txt` file for use with the journal-based downloaders.

```bash
uv run python export_uuids.py ../majsoul-jade-filtered.db todo.txt
```

### `validate_pb.py`

Debug utility for inspecting raw `.pb` protobuf files. Parses a protobuf record and prints its structure.

```bash
uv run python validate_pb.py <file.pb>
```

### `journal.py`

Library module providing the `Journal` class for append-only download state tracking. Used by `async_downloader.py`.
