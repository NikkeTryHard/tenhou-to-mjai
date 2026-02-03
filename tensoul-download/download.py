#!/usr/bin/env python3
"""
Download Majsoul games using tensoul-py-ng and save as Tenhou JSON.
Output files can be converted to MJAI using: ./target/release/tenhou2mjai <file>.json
"""
import asyncio
import json
import sys
import os
from pathlib import Path

from tensoul import MajsoulPaipuDownloader


async def download_game(username: str, password: str, uuid: str, output_dir: Path) -> str:
    """Download a single game and save to JSON file."""
    output_file = output_dir / f"{uuid.replace('-', '_')}.json"

    if output_file.exists():
        print(f"  [SKIP] Already exists: {output_file.name}")
        return str(output_file)

    async with MajsoulPaipuDownloader() as downloader:
        await downloader.login(username, password)
        log = await downloader.download(uuid, lobby_id=0)

        with open(output_file, "w", encoding="utf-8") as f:
            json.dump(log, f, ensure_ascii=False, indent=2)

        print(f"  [OK] Downloaded: {output_file.name}")
        return str(output_file)


async def download_batch(username: str, password: str, uuids: list[str], output_dir: Path) -> list[str]:
    """Download multiple games, reusing connection."""
    output_dir.mkdir(parents=True, exist_ok=True)
    downloaded = []

    async with MajsoulPaipuDownloader() as downloader:
        print(f"Logging in as {username}...")
        await downloader.login(username, password)
        print(f"Downloading {len(uuids)} games to {output_dir}/")

        for i, uuid in enumerate(uuids, 1):
            output_file = output_dir / f"{uuid.replace('-', '_')}.json"

            if output_file.exists():
                print(f"  [{i}/{len(uuids)}] SKIP: {uuid} (exists)")
                downloaded.append(str(output_file))
                continue

            try:
                log = await downloader.download(uuid, lobby_id=0)

                with open(output_file, "w", encoding="utf-8") as f:
                    json.dump(log, f, ensure_ascii=False, indent=2)

                print(f"  [{i}/{len(uuids)}] OK: {uuid}")
                downloaded.append(str(output_file))

                # Small delay to be nice to the server
                await asyncio.sleep(0.5)

            except Exception as e:
                print(f"  [{i}/{len(uuids)}] FAIL: {uuid} - {e}")

    return downloaded


def main():
    if len(sys.argv) < 4:
        print("Usage: python download.py <username> <password> <uuid> [uuid2 ...] [--output-dir DIR]")
        print("")
        print("Examples:")
        print("  # Download single game")
        print("  python download.py user@example.com password 190823-0c8903ea-a9ac-489f-bbe4-77e85d2a8319")
        print("")
        print("  # Download multiple games")
        print("  python download.py user@example.com password uuid1 uuid2 uuid3")
        print("")
        print("  # Download to specific directory")
        print("  python download.py user@example.com password uuid1 --output-dir ./games")
        print("")
        print("Output: Tenhou JSON files (convert with ./target/release/tenhou2mjai)")
        sys.exit(1)

    username = sys.argv[1]
    password = sys.argv[2]

    # Parse remaining args
    uuids = []
    output_dir = Path("./tenhou-json")

    i = 3
    while i < len(sys.argv):
        if sys.argv[i] == "--output-dir":
            output_dir = Path(sys.argv[i + 1])
            i += 2
        else:
            uuids.append(sys.argv[i])
            i += 1

    if not uuids:
        print("Error: No UUIDs provided")
        sys.exit(1)

    # Run download
    downloaded = asyncio.run(download_batch(username, password, uuids, output_dir))

    print(f"\nDownloaded {len(downloaded)} files to {output_dir}/")
    print(f"\nTo convert to MJAI:")
    print(f"  for f in {output_dir}/*.json; do ./target/release/tenhou2mjai \"$f\"; done")


if __name__ == "__main__":
    main()
