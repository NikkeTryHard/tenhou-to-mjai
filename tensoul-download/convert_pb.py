#!/usr/bin/env python3
"""
Convert .pb (Majsoul protobuf) files to MJAI format.

Pipeline: .pb → Tenhou JSON (tensoul) → MJAI (tenhou2mjai binary)
Runs alongside the active download without interfering.
Deletes .pb files after successful MJAI conversion to save disk.
"""

import os
import sys
import json
import signal
import subprocess
import time
import tempfile
from multiprocessing import Pool, cpu_count
from pathlib import Path

# Project paths
BASE_DIR = Path(__file__).parent
RAW_DIR = BASE_DIR / "jade-raw"
MJAI_DIR = BASE_DIR / "jade-mjai"
TENHOU2MJAI = BASE_DIR.parent / "target" / "release" / "tenhou2mjai"
FAILED_LOG = BASE_DIR / "convert_failed.log"

# Ensure tensoul is importable
sys.path.insert(0, str(BASE_DIR))

# Global shutdown flag
shutdown = False


def signal_handler(sig, frame):
    global shutdown
    shutdown = True
    print("\nShutting down after current batch...")


def convert_one(pb_path: str) -> tuple[str, str]:
    """Convert a single .pb file to MJAI. Returns (status, filename).

    status: 'ok', 'skip', 'fail'
    """
    pb_path = Path(pb_path)
    stem = pb_path.stem  # e.g. 190823_08ba5286_1e96_47e3_8f78_fbb7b73c5644
    mjai_path = MJAI_DIR / f"{stem}.mjai.json"

    # Already converted
    if mjai_path.exists():
        return ("skip", stem)

    try:
        # Import inside worker process (each process needs its own import)
        import ms.protocol_pb2 as pb
        from tensoul.downloader import MajsoulPaipuDownloader

        # Step 1: .pb → Tenhou JSON (in-memory)
        with open(pb_path, "rb") as f:
            data = f.read()

        if len(data) < 20:
            return ("fail", f"{stem}: file too small ({len(data)} bytes)")

        res = pb.ResGameRecord()
        res.ParseFromString(data)

        dl = MajsoulPaipuDownloader()
        dl.version_to_force = "0.11.216"
        tenhou_json = dl._handle_game_record(res, 0)

        if not tenhou_json or not tenhou_json.get("log"):
            return ("fail", f"{stem}: empty tenhou conversion")

        # Wrap in tensoul format (is_error + log)
        wrapped = {"is_error": False, "log": tenhou_json}

        # Step 2: Write temp JSON, run tenhou2mjai
        with tempfile.NamedTemporaryFile(
            mode="w", suffix=".json", delete=False, dir="/tmp"
        ) as tmp:
            json.dump(wrapped, tmp, ensure_ascii=False)
            tmp_json = tmp.name

        try:
            result = subprocess.run(
                [str(TENHOU2MJAI), tmp_json, str(mjai_path)],
                capture_output=True,
                text=True,
                timeout=30,
            )
            if result.returncode != 0:
                # Clean up partial output
                if mjai_path.exists():
                    mjai_path.unlink()
                return ("fail", f"{stem}: tenhou2mjai error: {result.stderr.strip()}")
        finally:
            os.unlink(tmp_json)

        # Step 3: Delete .pb to save disk
        pb_path.unlink(missing_ok=True)

        return ("ok", stem)

    except Exception as e:
        # Clean up partial output
        if mjai_path.exists():
            mjai_path.unlink()
        return ("fail", f"{stem}: {type(e).__name__}: {e}")


def get_pending_files() -> list[str]:
    """Get list of .pb files that haven't been converted yet."""
    # Get set of already-converted stems
    converted = set()
    if MJAI_DIR.exists():
        for f in MJAI_DIR.iterdir():
            if f.suffix == ".json" and f.name.endswith(".mjai.json"):
                converted.add(f.name.replace(".mjai.json", ""))

    # Get all .pb files not yet converted
    pending = []
    for f in RAW_DIR.iterdir():
        if f.suffix == ".pb" and f.stem not in converted:
            pending.append(str(f))

    return sorted(pending)


def main():
    signal.signal(signal.SIGINT, signal_handler)
    signal.signal(signal.SIGTERM, signal_handler)

    MJAI_DIR.mkdir(exist_ok=True)

    if not TENHOU2MJAI.exists():
        print(f"ERROR: tenhou2mjai binary not found at {TENHOU2MJAI}")
        sys.exit(1)

    # Use half the cores to leave room for the download process
    workers = max(1, cpu_count() // 2)
    batch_size = workers * 20  # Process in batches for progress reporting

    print(f"Converting .pb → MJAI with {workers} workers")
    print(f"  Raw dir:  {RAW_DIR}")
    print(f"  MJAI dir: {MJAI_DIR}")
    print()

    total_ok = 0
    total_fail = 0
    total_skip = 0
    start_time = time.time()

    while not shutdown:
        pending = get_pending_files()
        if not pending:
            print("No pending files. Waiting 30s for new downloads...")
            for _ in range(30):
                if shutdown:
                    break
                time.sleep(1)
            continue

        print(f"Found {len(pending)} pending files")

        # Process in batches
        for batch_start in range(0, len(pending), batch_size):
            if shutdown:
                break

            batch = pending[batch_start : batch_start + batch_size]

            with Pool(workers) as pool:
                results = pool.map(convert_one, batch)

            batch_ok = 0
            batch_fail = 0
            batch_skip = 0

            with open(FAILED_LOG, "a") as fail_f:
                for status, info in results:
                    if status == "ok":
                        batch_ok += 1
                    elif status == "fail":
                        batch_fail += 1
                        fail_f.write(f"{info}\n")
                    elif status == "skip":
                        batch_skip += 1

            total_ok += batch_ok
            total_fail += batch_fail
            total_skip += batch_skip

            elapsed = time.time() - start_time
            rate = total_ok / elapsed if elapsed > 0 else 0
            remaining = len(pending) - batch_start - len(batch)

            print(
                f"  Converted: {total_ok:,} | Failed: {total_fail:,} | "
                f"Skipped: {total_skip:,} | Rate: {rate:.1f}/s | "
                f"Remaining in batch: {remaining:,}"
            )

    elapsed = time.time() - start_time
    print(f"\nDone. {total_ok:,} converted, {total_fail:,} failed in {elapsed:.0f}s")


if __name__ == "__main__":
    main()
