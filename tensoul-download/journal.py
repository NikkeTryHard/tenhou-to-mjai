"""
Append-only journal for tracking download progress.
Replaces SQLite for download state tracking.
"""
import os
from pathlib import Path


class Journal:
    def __init__(self, todo_file: Path, completed_file: Path):
        self.todo_file = todo_file
        self.completed_file = completed_file
        self.done_set: set[str] = set()
        self._all_uuids: list[str] = []
        self._log_fd = None

    def load(self, scan_dir: Path | None = None):
        """Load todo list and completed log into memory."""
        # Load all UUIDs from todo file
        with open(self.todo_file, "r") as f:
            self._all_uuids = [line.strip() for line in f if line.strip()]

        # Load completed UUIDs (skip partial lines from crashes)
        if self.completed_file.exists():
            with open(self.completed_file, "r") as f:
                content = f.read()
            for line in content.split("\n"):
                stripped = line.strip()
                if stripped and content.find(line + "\n") != -1:
                    self.done_set.add(stripped)

        # Scan existing files on disk
        if scan_dir and scan_dir.exists():
            for fname in os.listdir(scan_dir):
                if fname.endswith(".json"):
                    uuid = fname[:-5].replace("_", "-")
                    self.done_set.add(uuid)

        # Open log file for appending (line-buffered)
        self._log_fd = open(self.completed_file, "a", buffering=1)

    def remaining(self) -> list[str]:
        """Return UUIDs not yet completed, preserving original order."""
        return [u for u in self._all_uuids if u not in self.done_set]

    def mark_done(self, uuid: str):
        """Record a UUID as completed. Appends to log file immediately."""
        if uuid in self.done_set:
            return
        self.done_set.add(uuid)
        if self._log_fd:
            self._log_fd.write(uuid + "\n")

    def close(self):
        """Flush and close the log file."""
        if self._log_fd:
            self._log_fd.flush()
            self._log_fd.close()
            self._log_fd = None

    @property
    def total(self) -> int:
        return len(self._all_uuids)

    @property
    def completed_count(self) -> int:
        return len(self.done_set)
