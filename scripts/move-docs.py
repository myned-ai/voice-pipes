#!/usr/bin/env python3
"""Move markdown files and rewrite every relative link in the repo to match.

Order of operations matters: links are rewritten *before* files move, using
the current filesystem to resolve targets (doc-relative first, then
repo-root, the same two conventions scripts/docs-check.ps1 accepts). Then the
moves happen with `git mv` (plain rename for untracked files).

Usage:
    python scripts/move-docs.py moves.json --dry-run   # print plan only
    python scripts/move-docs.py moves.json             # execute

moves.json is an object of {"old/repo/relative/path.md": "new/path.md"}.
Directories are created as needed; emptied source directories are removed.

Used for the 2026-09-06 docs/internal split and the WS-B public tree
restructure (docs/internal/plans/PLAN-OSS-DOCS-AND-RELEASE-PACKAGING.md).
"""
from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
from pathlib import Path, PurePosixPath

REPO = Path(__file__).resolve().parent.parent
EXCLUDE_DIR_PARTS = {"node_modules", "target", ".git", ".cache", "VibeVoice", "book"}
LINK_RE = re.compile(r"\]\(([^)\s]+)(\s+\"[^\"]*\")?\)")


def rel(p: Path) -> str:
    return p.relative_to(REPO).as_posix()


def all_markdown() -> list[Path]:
    out = []
    for p in REPO.rglob("*.md"):
        parts = p.relative_to(REPO).parts
        if any(part in EXCLUDE_DIR_PARTS for part in parts):
            continue
        if "/models/" in p.as_posix() and ".cache" in p.as_posix():
            continue
        out.append(p)
    return out


def resolve_target(doc_dir: Path, target: str) -> str | None:
    for base in (doc_dir, REPO):
        cand_n = Path(os.path.normpath(base / target))
        if cand_n.exists():
            try:
                return cand_n.relative_to(REPO).as_posix()
            except ValueError:
                return None
    return None


def rewrite_links(moves: dict[str, str], dry: bool) -> int:
    changed_files = 0
    for doc in all_markdown():
        doc_rel = rel(doc)
        doc_new_rel = moves.get(doc_rel, doc_rel)
        doc_moved = doc_new_rel != doc_rel
        old_dir = doc.parent
        new_dir = REPO / PurePosixPath(doc_new_rel).parent
        text = doc.read_text(encoding="utf-8")
        edits = 0

        def sub(m: re.Match) -> str:
            nonlocal edits
            raw, title = m.group(1), m.group(2) or ""
            if raw.startswith(("http:", "https:", "mailto:", "tel:", "data:", "//", "#")):
                return m.group(0)
            if re.match(r"^[a-z][a-z0-9+.-]*://", raw):
                return m.group(0)
            path_part, sep, anchor = raw.partition("#")
            if not path_part:
                return m.group(0)
            target_rel = resolve_target(old_dir, path_part)
            if target_rel is None:
                return m.group(0)
            target_new = moves.get(target_rel, target_rel)
            if not (doc_moved or target_new != target_rel):
                return m.group(0)
            new_path = os.path.relpath(REPO / target_new, new_dir).replace("\\", "/")
            if path_part.endswith("/") and not new_path.endswith("/"):
                new_path += "/"
            if new_path == path_part:
                return m.group(0)
            edits += 1
            return f"]({new_path}{sep}{anchor}{title})"

        new_text = LINK_RE.sub(sub, text)
        if edits:
            changed_files += 1
            print(f"  relink {doc_rel}: {edits} link(s)")
            if not dry:
                doc.write_text(new_text, encoding="utf-8", newline="")
    return changed_files


def tracked_files() -> set[str]:
    out = subprocess.run(["git", "ls-files", "-z"], cwd=REPO, check=True, capture_output=True).stdout
    return {s.decode("utf-8") for s in out.split(b"\0") if s}


def do_moves(moves: dict[str, str], dry: bool) -> None:
    tracked = tracked_files()
    src_dirs: set[Path] = set()
    for src, dst in moves.items():
        src_dirs.add((REPO / src).parent)
        (REPO / dst).parent.mkdir(parents=True, exist_ok=True)
        if src in tracked:
            print(f"  git mv {src} -> {dst}")
            if not dry:
                subprocess.run(["git", "mv", "-k", src, dst], cwd=REPO, check=True)
        else:
            print(f"  mv (untracked) {src} -> {dst}")
            if not dry:
                os.replace(REPO / src, REPO / dst)
    if not dry:
        for d in sorted(src_dirs, key=lambda p: len(p.parts), reverse=True):
            while d != REPO and d.exists() and not any(d.iterdir()):
                d.rmdir()
                d = d.parent


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("moves_file")
    ap.add_argument("--dry-run", action="store_true")
    args = ap.parse_args()
    moves: dict[str, str] = json.loads(Path(args.moves_file).read_text(encoding="utf-8"))
    missing = [s for s in moves if not (REPO / s).exists()]
    if missing:
        print("missing sources:\n  " + "\n  ".join(missing), file=sys.stderr)
        return 2
    print(f"== {len(moves)} moves; rewriting links")
    n = rewrite_links(moves, args.dry_run)
    print(f"   {n} file(s) with rewritten links")
    print("== moving files")
    do_moves(moves, args.dry_run)
    print("done" + (" (dry run)" if args.dry_run else ""))
    return 0


if __name__ == "__main__":
    sys.exit(main())
