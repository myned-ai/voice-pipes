#!/usr/bin/env python3
"""
Fetch the Nemotron 3.5 ASR ONNX export into `models/nemotron-3.5`.

Standard library only. Every file is pinned to one repository revision and
checked against the SHA-256 recorded here, so a changed upload is refused
rather than silently used. A file already present with the right hash is
skipped, so the script can be re-run after an interrupted download.

The two encoder graphs (first chunk, steady state) reference separate
external-weight files that are byte-identical, so the 1.2 GB of weights is
downloaded once and the second name is a hard link (or a copy where links
are unavailable). ONNX Runtime resolves each graph's weights by the file name
the graph carries, so both names must exist.

Two pinned sources on Hugging Face, each checked against its own hashes:

  huggingface (default)  this project's export on Hugging Face
                         (myned-ai/nemotron-3.5-asr-streaming-0.6b-onnx),
                         file by file, no token needed
  community              the community export
                         (codavidgarcia/nemotron-3.5-asr-streaming-0.6b-onnx)

The two exports share byte-identical encoder weights and decode
identically; their graph files, decoder, joiner and config differ.

    python scripts/download_nemotron35.py
    python scripts/download_nemotron35.py --source community
    python scripts/download_nemotron35.py --target /srv/models/nemotron-3.5
"""

from __future__ import annotations

import argparse
import hashlib
import os
import shutil
import sys
import urllib.error
import urllib.request
from pathlib import Path

TARGET = Path("models") / "nemotron-3.5"

# This project's export: hosted on Hugging Face (myned-ai).
REPO = "myned-ai/nemotron-3.5-asr-streaming-0.6b-onnx"
REVISION = "5a5ad09528e47909eb97f3040352c5dc6b5f529c"

# The community export the plugin was originally written and validated against.
COMMUNITY_REPO = "codavidgarcia/nemotron-3.5-asr-streaming-0.6b-onnx"
COMMUNITY_REVISION = "ae46ba711456c2d71f17d1c6d6f5ac2ba65077aa"

# name -> sha256 per source. The two `.onnx.data` entries share a hash.
HUGGINGFACE_FILES = {
    "nemotron_onnx_config.json": "555d1ee660dbbbe96d652136a8dff95c701df56bb392bb8887069be08ca19805",
    "tokens.txt": "2b485ebebc4d2f592237f3b6bcd3c75bc354980b2f696dbeb2fc2d5f7eb9c9ab",
    "decoder.onnx": "5ab61e81767f89c057203f7553238e2c08337031571756f58fdf525bcbd497d9",
    "joiner.onnx": "fe4c19f38b863cf3ad2cf15d0cacba8c24928129a8277169c5200ef833854059",
    "encoder_320ms_first_fp16.onnx": "5c2a5465bcdd048acd6daa63b18427c9d3f90e5941a25335c71fc8f4ff4d7f22",
    "encoder_320ms_fp16.onnx": "af455f3be36115f3091fa67c43112432d5630c51bc57f8c4d7dbea9ce3f7d38c",
    "encoder_320ms_first_fp16.onnx.data": "159887b78f1e36fd59f5cf17be810fd61e98a96b61673b6de0dbe2b8dfb47a31",
    "encoder_320ms_fp16.onnx.data": "159887b78f1e36fd59f5cf17be810fd61e98a96b61673b6de0dbe2b8dfb47a31",
    "LICENSE-OpenMDW-1.1": "cc66dc17fcccd959c9749222445f6525ca448d575892d6925b4c5519221ec906",
    "NOTICE": "5ac23b1ceda50921279a3d44bfcde7069165006ef238eb2dd305e30d6e9cd126",
}
COMMUNITY_FILES = {
    "nemotron_onnx_config.json": "81fcb81636c4915fd1600fb877b9935f5e73da693554b2c91c8bcb791f248f5c",
    "tokens.txt": "2b485ebebc4d2f592237f3b6bcd3c75bc354980b2f696dbeb2fc2d5f7eb9c9ab",
    "decoder.onnx": "784fdad7f6d510057ddb58467b7cd1500de03fc4517d90a1654858d870175113",
    "joiner.onnx": "2d8d8ccef0a655a59be23c1aaa89fb10927d7057da3c3a49ae67e3d7756db489",
    "encoder_320ms_first_fp16.onnx": "1df652c127ecbfd5fb7fd8975f3444f487dffe971613aeea68f54a969de218b7",
    "encoder_320ms_fp16.onnx": "78af98ec11d27975e3e8fdc7bc480c6e3c174f3eb8a4d21cc52541db8b4912e3",
    "encoder_320ms_first_fp16.onnx.data": "159887b78f1e36fd59f5cf17be810fd61e98a96b61673b6de0dbe2b8dfb47a31",
    "encoder_320ms_fp16.onnx.data": "159887b78f1e36fd59f5cf17be810fd61e98a96b61673b6de0dbe2b8dfb47a31",
    "LICENSE-OpenMDW-1.1": "cc66dc17fcccd959c9749222445f6525ca448d575892d6925b4c5519221ec906",
    "NOTICE": "b74ed89c5f5157d4355da5c447c7e1f403abd0b30dfd19a008be97c305ccb1f6",
}
SOURCES = {
    "huggingface": HUGGINGFACE_FILES,
    "community": COMMUNITY_FILES,
    "github": HUGGINGFACE_FILES,
}

CHUNK = 1 << 20


def sha256_of(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for block in iter(lambda: f.read(CHUNK), b""):
            h.update(block)
    return h.hexdigest()


def fetch(url: str, dest: Path, expected: str, headers: dict[str, str] | None = None) -> None:
    tmp = dest.with_suffix(dest.suffix + ".part")
    h = hashlib.sha256()
    done = 0
    req = urllib.request.Request(url, headers=headers or {})
    with urllib.request.urlopen(req) as resp, tmp.open("wb") as out:
        total = int(resp.headers.get("Content-Length") or 0)
        while True:
            block = resp.read(CHUNK)
            if not block:
                break
            out.write(block)
            h.update(block)
            done += len(block)
            if total:
                sys.stdout.write(f"\r  {done / 1e6:8.1f} / {total / 1e6:.1f} MB")
                sys.stdout.flush()
    if total:
        sys.stdout.write("\n")
    got = h.hexdigest()
    if got != expected:
        tmp.unlink(missing_ok=True)
        sys.exit(f"{dest.name}: SHA-256 mismatch\n  expected {expected}\n  got      {got}")
    tmp.replace(dest)


def link_or_copy(src: Path, dest: Path) -> str:
    try:
        os.link(src, dest)
        return "linked"
    except OSError:
        shutil.copyfile(src, dest)
        return "copied"


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--target", type=Path, default=TARGET, help=f"destination directory (default: {TARGET})")
    ap.add_argument(
        "--source",
        choices=["huggingface", "community", "github"],
        default="huggingface",
        help="where to fetch from (default: huggingface; 'github' is migrated to huggingface)",
    )
    ap.add_argument("--repo", default=None, help="Hugging Face repository id (overrides default)")
    ap.add_argument("--revision", default=None, help="commit or branch to fetch from (overrides default)")
    args = ap.parse_args()

    if args.source == "github":
        print("Note: The GitHub release has been migrated to Hugging Face. Fetching from myned-ai on Hugging Face.")
        args.source = "huggingface"

    if args.source == "community":
        default_repo = COMMUNITY_REPO
        default_revision = COMMUNITY_REVISION
    else:
        default_repo = REPO
        default_revision = REVISION

    repo = args.repo or default_repo
    revision = args.revision or default_revision

    target: Path = args.target
    target.mkdir(parents=True, exist_ok=True)
    print(f"Target: {target.resolve()}")
    files = SOURCES[args.source]
    print(f"Source: {repo} @ {revision[:12]}")

    # Every file is checked against its expected hash. The second weights file
    # (identical to the first) is linked/copied locally once the first is downloaded.
    seen_by_hash: dict[str, Path] = {}
    for name, expected in files.items():
        dest = target / name
        if dest.is_file() and sha256_of(dest) == expected:
            print(f"ok       {name}")
            seen_by_hash.setdefault(expected, dest)
            continue
        if expected in seen_by_hash:
            how = link_or_copy(seen_by_hash[expected], dest)
            print(f"{how:8} {name}  (same bytes as {seen_by_hash[expected].name})")
            continue
        print(f"fetching {name}")
        url = f"https://huggingface.co/{repo}/resolve/{revision}/{name}"
        fetch(url, dest, expected)
        seen_by_hash[expected] = dest

    print()
    if (target / "NOTICE").is_file():
        print((target / "NOTICE").read_text(encoding="utf-8").rstrip())
    print()
    print("Done. Point `[stt] model_dir` at this directory with `provider = \"nemotron-3.5\"`.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
