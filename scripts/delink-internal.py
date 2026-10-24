#!/usr/bin/env python3
"""Turn links from public docs into docs/internal/ into plain text.

The public tree (see scripts/assemble-public.sh) does not contain
docs/internal/, so any such link would be broken there. This replaces
`[label](path/into/internal.md#anchor)` with `label` in every markdown file
outside docs/internal/.

Usage:
    python scripts/delink-internal.py [--dry-run]
"""
from __future__ import annotations

import argparse
import os
import re
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
INTERNAL = REPO / "docs" / "internal"
EXCLUDE_PARTS = {"node_modules", "target", ".git", ".cache", "VibeVoice", "book", "internal", "marketing"}
LINK_RE = re.compile(r"\[([^\]]*)\]\(([^)\s]+)(?:\s+\"[^\"]*\")?\)")


def resolves_into_internal(doc_dir: Path, target: str) -> bool:
    path_part = target.partition("#")[0]
    if not path_part or re.match(r"^[a-z][a-z0-9+.-]*:", path_part) or path_part.startswith("//"):
        return False
    for base in (doc_dir, REPO):
        cand = Path(os.path.normpath(base / path_part))
        if cand.exists():
            try:
                cand.relative_to(INTERNAL)
                return True
            except ValueError:
                return False
    return False


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--dry-run", action="store_true")
    dry = ap.parse_args().dry_run
    total = 0
    for doc in sorted(REPO.rglob("*.md")):
        parts = doc.relative_to(REPO).parts
        if any(p in EXCLUDE_PARTS for p in parts):
            continue
        if "/models/" in doc.as_posix() and ".cache" in doc.as_posix():
            continue
        text = doc.read_text(encoding="utf-8")
        n = 0

        def sub(m: re.Match) -> str:
            nonlocal n
            label, target = m.group(1), m.group(2)
            if resolves_into_internal(doc.parent, target):
                n += 1
                return label
            return m.group(0)

        new = LINK_RE.sub(sub, text)
        if n:
            total += n
            print(f"  {doc.relative_to(REPO).as_posix()}: {n} link(s) de-linked")
            if not dry:
                doc.write_text(new, encoding="utf-8", newline="")
    print(f"{total} link(s) into docs/internal/ removed from public pages" + (" (dry run)" if dry else ""))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
