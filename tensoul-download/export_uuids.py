#!/usr/bin/env python3
"""One-time export: dump all downloadable UUIDs from SQLite to todo.txt"""
import sqlite3
import sys

def main():
    db_path = sys.argv[1] if len(sys.argv) > 1 else "../majsoul-jade-filtered.db"
    output = sys.argv[2] if len(sys.argv) > 2 else "todo.txt"

    conn = sqlite3.connect(db_path)
    cursor = conn.execute(
        "SELECT full_uuid FROM majsoul_logs WHERE full_uuid IS NOT NULL AND mode_id = 12"
    )

    count = 0
    with open(output, "w") as f:
        for (uuid,) in cursor:
            f.write(uuid + "\n")
            count += 1

    conn.close()
    print(f"Exported {count:,} UUIDs to {output}")

if __name__ == "__main__":
    main()
