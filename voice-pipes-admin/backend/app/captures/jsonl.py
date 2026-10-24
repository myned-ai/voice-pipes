"""Reading a capture's `.jsonl` files one line at a time, for real.

Every reader used to parse these inside a single `try` around the whole file.
One unparseable line -- a record torn by an interrupted or overlapping write --
raised out of the loop, the `except` swallowed it, and everything after that
line was silently never read. A session with a torn line on row 5 of a
hundred lost the timings of every turn and still rendered as if nothing were
wrong.

A line is the unit of failure: a bad one is skipped and counted, and the rest
of the file is read as usual.
"""

from __future__ import annotations

import json
import logging
from collections.abc import Iterator
from pathlib import Path

logger = logging.getLogger(__name__)


def parse_jsonl_line(line: str, source: Path | str | None = None) -> dict | None:
    """One line as an object, or `None` when it is not one."""
    try:
        row = json.loads(line)
    except ValueError:
        logger.debug("skipping unreadable line in %s: %r", source, line[:80])
        return None
    return row if isinstance(row, dict) else None


def iter_jsonl(path: Path) -> Iterator[dict]:
    """Every readable object in the file, in order. A missing file is empty."""
    try:
        with open(path, encoding="utf-8", errors="replace") as f:
            for line in f:
                if not line.strip():
                    continue
                row = parse_jsonl_line(line, path)
                if row is not None:
                    yield row
    except OSError:
        return


def count_unreadable_lines(path: Path) -> int:
    """How many non-empty lines of the file are not JSON objects."""
    bad = 0
    try:
        with open(path, encoding="utf-8", errors="replace") as f:
            for line in f:
                if not line.strip():
                    continue
                try:
                    if not isinstance(json.loads(line), dict):
                        bad += 1
                except ValueError:
                    bad += 1
    except OSError:
        return 0
    return bad
