import os
import tempfile
from pathlib import Path
from journal import Journal


def test_load_empty():
    with tempfile.TemporaryDirectory() as d:
        j = Journal(todo_file=Path(d) / "todo.txt", completed_file=Path(d) / "completed.log")
        (Path(d) / "todo.txt").write_text("aaa\nbbb\nccc\n")
        j.load()
        assert j.remaining() == ["aaa", "bbb", "ccc"]
        assert len(j.done_set) == 0


def test_load_with_existing_completed():
    with tempfile.TemporaryDirectory() as d:
        (Path(d) / "todo.txt").write_text("aaa\nbbb\nccc\nddd\n")
        (Path(d) / "completed.log").write_text("bbb\nccc\n")
        j = Journal(todo_file=Path(d) / "todo.txt", completed_file=Path(d) / "completed.log")
        j.load()
        assert j.remaining() == ["aaa", "ddd"]
        assert len(j.done_set) == 2


def test_mark_done():
    with tempfile.TemporaryDirectory() as d:
        (Path(d) / "todo.txt").write_text("aaa\nbbb\n")
        completed_path = Path(d) / "completed.log"
        j = Journal(todo_file=Path(d) / "todo.txt", completed_file=completed_path)
        j.load()
        j.mark_done("aaa")
        assert "aaa" in j.done_set
        assert "aaa\n" in completed_path.read_text()


def test_mark_done_idempotent():
    with tempfile.TemporaryDirectory() as d:
        (Path(d) / "todo.txt").write_text("aaa\n")
        j = Journal(todo_file=Path(d) / "todo.txt", completed_file=Path(d) / "completed.log")
        j.load()
        j.mark_done("aaa")
        j.mark_done("aaa")
        assert len(j.done_set) == 1


def test_load_survives_partial_line():
    with tempfile.TemporaryDirectory() as d:
        (Path(d) / "todo.txt").write_text("aaa\nbbb\nccc\n")
        (Path(d) / "completed.log").write_text("aaa\nbb")
        j = Journal(todo_file=Path(d) / "todo.txt", completed_file=Path(d) / "completed.log")
        j.load()
        assert "aaa" in j.done_set
        assert "bb" not in j.done_set
        assert j.remaining() == ["bbb", "ccc"]


def test_scan_existing_files():
    with tempfile.TemporaryDirectory() as d:
        (Path(d) / "todo.txt").write_text(
            "220101-aaaa-bbbb-cccc-dddd-eeeeeeeeeeee\n"
            "220102-1111-2222-3333-4444-555555555555\n"
        )
        output = Path(d) / "output"
        output.mkdir()
        (output / "220101_aaaa_bbbb_cccc_dddd_eeeeeeeeeeee.json").write_text("{}")
        j = Journal(todo_file=Path(d) / "todo.txt", completed_file=Path(d) / "completed.log")
        j.load(scan_dir=output)
        assert "220101-aaaa-bbbb-cccc-dddd-eeeeeeeeeeee" in j.done_set
        assert j.remaining() == ["220102-1111-2222-3333-4444-555555555555"]
