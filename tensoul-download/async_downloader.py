#!/usr/bin/env python3
"""
Pure asyncio multi-account Majsoul game downloader.
Uses append-only journal for state tracking (no SQLite in download loop).
"""
import asyncio
import builtins
import json
import logging
import os
import signal
import sys
import tempfile
import time
import warnings
from dataclasses import dataclass
from enum import Enum
from pathlib import Path
from typing import Optional

# Suppress warnings before importing tensoul
warnings.filterwarnings("ignore", message=".*sustain.*")
logging.getLogger("tensoul").setLevel(logging.ERROR)

from tensoul import MajsoulPaipuDownloader
from tensoul.downloader import MajsoulLoginError


class DownloadStatus(Enum):
    OK = "ok"
    SKIP = "skip"
    FAIL = "fail"
    RETRY = "retry"


@dataclass
class DownloadResult:
    status: DownloadStatus
    uuid: str
    error: Optional[str] = None


class Colors:
    RESET = "\033[0m"
    BOLD = "\033[1m"
    DIM = "\033[2m"
    RED = "\033[91m"
    GREEN = "\033[92m"
    YELLOW = "\033[93m"
    BLUE = "\033[94m"
    MAGENTA = "\033[95m"
    CYAN = "\033[96m"


def log_header(text: str):
    print(f"\n{Colors.BOLD}{Colors.CYAN}{'=' * 60}{Colors.RESET}")
    print(f"{Colors.BOLD}{Colors.CYAN}  {text}{Colors.RESET}")
    print(f"{Colors.BOLD}{Colors.CYAN}{'=' * 60}{Colors.RESET}\n")


def log_info(text: str):
    print(f"  {Colors.BLUE}[INFO]{Colors.RESET}  {text}")


def log_success(text: str):
    print(f"  {Colors.GREEN}[OK]{Colors.RESET}    {text}")


def log_warning(text: str):
    print(f"  {Colors.YELLOW}[WARN]{Colors.RESET}  {Colors.YELLOW}{text}{Colors.RESET}")


def log_error(text: str):
    print(f"  {Colors.RED}[ERR]{Colors.RESET}   {Colors.RED}{text}{Colors.RESET}")


def format_time(seconds: float) -> str:
    if seconds < 60:
        return f"{seconds:.0f}s"
    elif seconds < 3600:
        return f"{seconds / 60:.1f}m"
    else:
        return f"{seconds / 3600:.1f}h"


def format_eta(seconds: float) -> str:
    if seconds < 60:
        return f"{seconds:.0f}s"
    elif seconds < 3600:
        return f"{int(seconds / 60)}m"
    else:
        return f"{seconds / 3600:.1f}h"


class AsyncAccountWorker:
    """Manages one account's connection and downloads."""

    def __init__(
        self,
        worker_id: int,
        username: str,
        password: str,
        output_dir: Path,
    ):
        self.worker_id = worker_id
        self.username = username
        self.password = password
        self.output_dir = output_dir
        self.connected = False
        self._downloader: Optional[MajsoulPaipuDownloader] = None

        # Stats
        self.success_count = 0
        self.failed_count = 0

    async def connect(self) -> bool:
        """Login to Majsoul. Returns True on success."""
        try:
            self._downloader = MajsoulPaipuDownloader()
            await self._downloader.__aenter__()
            await asyncio.wait_for(
                self._downloader.login(self.username, self.password),
                timeout=30.0
            )
            self.connected = True
            return True
        except (MajsoulLoginError, Exception) as e:
            self.connected = False
            if self._downloader:
                try:
                    await self._downloader.__aexit__(None, None, None)
                except Exception:
                    pass
                self._downloader = None
            return False

    async def download_game(self, uuid: str) -> DownloadResult:
        """Download one game. Returns DownloadResult."""
        if not self.connected or not self._downloader:
            return DownloadResult(DownloadStatus.FAIL, uuid, "Not connected")

        output_file = self.output_dir / f"{uuid.replace('-', '_')}.json"

        try:
            result = await asyncio.wait_for(
                self._downloader.download(uuid, lobby_id=0),
                timeout=30.0
            )

            if result.get("is_error"):
                error_msg = result.get("error_msg", "unknown")
                self.failed_count += 1
                return DownloadResult(DownloadStatus.FAIL, uuid, f"API:{error_msg}")

            # Offload blocking I/O to thread pool (unblocks event loop)
            def sync_save():
                temp_fd, temp_path = tempfile.mkstemp(
                    dir=self.output_dir,
                    prefix=".tmp_",
                    suffix=".json"
                )
                try:
                    with os.fdopen(temp_fd, 'w') as f:
                        json.dump(result, f, separators=(',', ':'))
                    os.rename(temp_path, output_file)
                except Exception:
                    try:
                        os.unlink(temp_path)
                    except Exception:
                        pass
                    raise

            loop = asyncio.get_running_loop()
            await loop.run_in_executor(None, sync_save)

            self.success_count += 1
            return DownloadResult(DownloadStatus.OK, uuid)

        except asyncio.TimeoutError:
            self.failed_count += 1
            return DownloadResult(DownloadStatus.FAIL, uuid, "Timeout")
        except Exception as e:
            self.failed_count += 1
            return DownloadResult(DownloadStatus.FAIL, uuid, str(e)[:50])

    async def close(self):
        """Graceful shutdown."""
        self.connected = False
        if self._downloader:
            # Suppress "sustain task cancelled" spam from tensoul
            original_print = builtins.print
            def silent_print(*args, **kwargs):
                msg = " ".join(str(a) for a in args)
                if "sustain" not in msg:
                    original_print(*args, **kwargs)
            builtins.print = silent_print
            try:
                await self._downloader.__aexit__(None, None, None)
            except Exception:
                pass
            finally:
                pass
            self._downloader = None


class AsyncDownloadCoordinator:
    """Orchestrates all workers and manages the download pipeline."""

    def __init__(
        self,
        accounts: list[tuple[str, str]],
        output_dir: Path,
        journal: "Journal",
    ):
        self.accounts = accounts
        self.output_dir = output_dir
        self.journal = journal
        self.workers: list[AsyncAccountWorker] = []
        self.queue: asyncio.Queue[str] = asyncio.Queue()
        self._shutdown_event = asyncio.Event()
        self._error_log = None

        # Stats
        self.success = 0
        self.failed = 0
        self.skipped = 0
        self.total_queued = 0
        self.start_time: Optional[float] = None

        # Error tracking
        self.error_counts: dict[str, int] = {}

    async def connect_all_workers(self) -> int:
        """Connect all accounts concurrently. Returns count of successful connections."""
        log_info(f"Connecting {len(self.accounts)} accounts...")

        async def connect_one(idx: int, username: str, password: str) -> Optional[AsyncAccountWorker]:
            worker = AsyncAccountWorker(
                worker_id=idx,
                username=username,
                password=password,
                output_dir=self.output_dir,
            )
            if await worker.connect():
                log_success(f"[{idx}] {username}")
                return worker
            else:
                log_error(f"[{idx}] {username} - login failed")
                return None

        tasks = [
            connect_one(i, user, pwd)
            for i, (user, pwd) in enumerate(self.accounts)
        ]
        results = await asyncio.gather(*tasks)
        self.workers = [w for w in results if w is not None]
        return len(self.workers)

    async def _feed_uuids(self, limit: Optional[int] = None):
        """Feed UUIDs from journal to queue."""
        uuids = self.journal.remaining()
        if limit:
            uuids = uuids[:limit]
        self.total_queued = len(uuids)

        for uuid in uuids:
            if self._shutdown_event.is_set():
                break
            await self.queue.put(uuid)

    async def _worker_loop(self, worker: AsyncAccountWorker):
        """Worker loop: pull from queue, download, record result."""
        try:
            while not self._shutdown_event.is_set():
                try:
                    uuid = await asyncio.wait_for(self.queue.get(), timeout=1.0)
                except asyncio.TimeoutError:
                    continue

                result = await worker.download_game(uuid)

                if result.status == DownloadStatus.OK:
                    self.success += 1
                    self.journal.mark_done(uuid)
                elif result.status == DownloadStatus.SKIP:
                    self.skipped += 1
                    self.journal.mark_done(uuid)
                elif result.status == DownloadStatus.FAIL:
                    self.failed += 1
                    error_key = result.error[:30] if result.error else "unknown"
                    self.error_counts[error_key] = self.error_counts.get(error_key, 0) + 1
                    if self._error_log:
                        self._error_log.write(f"{uuid}\t{result.error}\n")

                self.queue.task_done()

                # Small delay to avoid rate limiting (only for actual API calls)
                if result.status != DownloadStatus.SKIP:
                    await asyncio.sleep(0.3)

        except asyncio.CancelledError:
            pass

    async def _display_loop(self):
        """Update progress display every 100ms."""
        try:
            while not self._shutdown_event.is_set():
                await asyncio.sleep(0.1)

                elapsed = time.time() - self.start_time if self.start_time else 0
                done = self.success + self.skipped + self.failed
                remaining = self.total_queued - done

                # Calculate rate based on actual downloads only (not skips)
                actual_downloads = self.success + self.failed
                download_rate = actual_downloads / elapsed if elapsed > 0 else 0
                eta = remaining / download_rate if download_rate > 0 else 0

                # Build status line
                status = (
                    f"\r  {Colors.GREEN}{self.success:,}{Colors.RESET} ok | "
                    f"{Colors.YELLOW}{self.skipped:,}{Colors.RESET} skip | "
                    f"{Colors.RED}{self.failed:,}{Colors.RESET} fail | "
                    f"{Colors.CYAN}{format_time(elapsed)}{Colors.RESET} | "
                    f"{Colors.MAGENTA}{download_rate:.1f}/s{Colors.RESET} | "
                    f"ETA {Colors.DIM}{format_eta(eta)}{Colors.RESET} | "
                    f"Q:{self.queue.qsize()}    "
                )
                sys.stdout.write(status)
                sys.stdout.flush()

        except asyncio.CancelledError:
            pass

    def shutdown(self):
        """Signal shutdown to all tasks."""
        self._shutdown_event.set()

    async def download_all(self, limit: Optional[int] = None) -> dict:
        """Main orchestration: connect, feed queue, run workers, display progress."""
        self.output_dir.mkdir(parents=True, exist_ok=True)
        self.start_time = time.time()

        # Open error log
        self._error_log = open(self.output_dir.parent / "failed.log", "a", buffering=1)

        # Connect all workers
        connected = await self.connect_all_workers()
        if connected == 0:
            log_error("No accounts connected, aborting")
            return {"success": 0, "failed": 0, "skipped": 0}

        log_info(f"Connected {connected}/{len(self.accounts)} accounts")

        # Feed UUIDs from journal
        log_info("Loading UUIDs from journal...")
        await self._feed_uuids(limit)
        log_info(f"Queued {self.total_queued:,} games for download")

        if self.total_queued == 0:
            log_warning("No games to download")
            return {"success": 0, "failed": 0, "skipped": 0}

        # Start background tasks
        tasks = []

        # Worker loops
        for worker in self.workers:
            tasks.append(asyncio.create_task(self._worker_loop(worker)))

        # Display loop
        display_task = asyncio.create_task(self._display_loop())
        tasks.append(display_task)

        # Wait for queue to drain or shutdown
        try:
            while not self._shutdown_event.is_set():
                if self.queue.empty() and (self.success + self.skipped + self.failed) >= self.total_queued:
                    break
                await asyncio.sleep(0.5)
        except asyncio.CancelledError:
            pass

        # Shutdown
        self._shutdown_event.set()

        for task in tasks:
            task.cancel()

        try:
            await asyncio.wait_for(
                asyncio.gather(*tasks, return_exceptions=True),
                timeout=3.0
            )
        except (asyncio.TimeoutError, TimeoutError):
            pass

        # Close all workers
        for worker in self.workers:
            await worker.close()

        # Close journal and error log
        self.journal.close()
        if self._error_log:
            self._error_log.close()

        print()  # Newline after progress bar
        return {"success": self.success, "failed": self.failed, "skipped": self.skipped}


def load_accounts(accounts_file: str, password: str, output_dir: str = "./tenhou-json") -> list[tuple[str, str]]:
    """Load accounts from file, excluding slow accounts."""
    accounts = []
    seen = set()
    duplicates = 0
    slow_skipped = 0

    # Load slow accounts to exclude
    slow_accounts_file = Path(output_dir).parent / "slow_accounts.txt"
    slow_accounts = set()
    if slow_accounts_file.exists():
        with open(slow_accounts_file, "r") as f:
            slow_accounts = {line.strip() for line in f if line.strip()}

    if os.path.exists(accounts_file):
        with open(accounts_file, "r") as f:
            for line in f:
                email = line.strip()
                if email and not email.startswith("#"):
                    if email in seen:
                        duplicates += 1
                    elif email in slow_accounts:
                        slow_skipped += 1
                    else:
                        seen.add(email)
                        accounts.append((email, password))

    if duplicates:
        log_warning(f"Found {duplicates} duplicate account(s), skipped")
    if slow_skipped:
        log_warning(f"Skipped {slow_skipped} slow account(s) from slow_accounts.txt")

    return accounts


def validate_accounts_file(accounts_file: str) -> bool:
    """Validate accounts file exists and has content."""
    if not os.path.exists(accounts_file):
        log_error(f"Accounts file not found: {accounts_file}")
        return False

    with open(accounts_file, "r") as f:
        lines = [l.strip() for l in f if l.strip() and not l.startswith("#")]

    if not lines:
        log_error(f"No accounts in {accounts_file}")
        return False

    return True


async def async_main(args):
    """Async entry point."""
    from journal import Journal

    accounts = load_accounts(args.accounts, args.password, args.output)
    if not accounts:
        sys.exit(1)

    log_success(f"Loaded {len(accounts)} unique accounts")

    # Load journal
    journal = Journal(
        todo_file=Path(args.todo),
        completed_file=Path(args.completed),
    )
    scan_dir = Path(args.output) if args.scan else None
    journal.load(scan_dir=scan_dir)
    log_info(f"Total: {journal.total:,} | Done: {journal.completed_count:,} | Remaining: {len(journal.remaining()):,}")

    coordinator = AsyncDownloadCoordinator(
        accounts=accounts,
        output_dir=Path(args.output),
        journal=journal,
    )

    # Setup signal handler
    def signal_handler():
        print(f"\n\n  {Colors.YELLOW}[STOP]{Colors.RESET}  Shutting down...")
        coordinator.shutdown()

    loop = asyncio.get_event_loop()
    for sig in (signal.SIGINT, signal.SIGTERM):
        loop.add_signal_handler(sig, signal_handler)

    stats = await coordinator.download_all(args.limit)

    elapsed = time.time() - coordinator.start_time if coordinator.start_time else 0
    rate = stats['success'] / elapsed if elapsed > 0 else 0

    # Print error breakdown if any failures
    if coordinator.error_counts:
        print(f"\n  {Colors.RED}Error breakdown:{Colors.RESET}")
        for reason, count in sorted(coordinator.error_counts.items(), key=lambda x: -x[1])[:5]:
            print(f"    {reason}: {count:,}")

    print()
    print(f"  {Colors.GREEN}Done:{Colors.RESET} {stats['success']:,} | "
          f"{Colors.YELLOW}Skip:{Colors.RESET} {stats['skipped']:,} | "
          f"{Colors.RED}Fail:{Colors.RESET} {stats['failed']:,} | "
          f"{Colors.CYAN}{format_time(elapsed)}{Colors.RESET} | "
          f"{Colors.MAGENTA}{rate:.1f}/s{Colors.RESET}")
    print()


def main():
    import argparse

    parser = argparse.ArgumentParser(description="Async multi-account Majsoul downloader")
    parser.add_argument("--output", "-o", default="./jade-json", help="Output directory")
    parser.add_argument("--todo", "-t", default="todo.txt", help="File with all UUIDs to download")
    parser.add_argument("--completed", "-c", default="completed.log", help="Append-only log of completed UUIDs")
    parser.add_argument("--scan", action="store_true", help="Scan output dir on first run to detect existing files")
    parser.add_argument("--limit", "-l", type=int, help="Max games to download")
    parser.add_argument("--accounts", "-a", default="accounts.txt", help="File with emails")
    parser.add_argument("--password", "-p", required=True, help="Shared password")

    args = parser.parse_args()

    log_header("Majsoul Async Multi-Account Downloader")

    if not validate_accounts_file(args.accounts):
        sys.exit(1)

    if not os.path.exists(args.todo):
        log_error(f"Todo file not found: {args.todo}")
        log_info("Run: uv run python export_uuids.py ../majsoul-jade-filtered.db todo.txt")
        sys.exit(1)

    asyncio.run(async_main(args))


if __name__ == "__main__":
    main()
