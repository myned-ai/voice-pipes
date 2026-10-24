#!/usr/bin/env python3
"""
Produce this project's own Nemotron 3.5 ASR ONNX export, ready to release.

One run, from the ungated NVIDIA checkpoint, yields the directory the
`voice-pipes-stt-nemotron35` plugin serves and the archive the release
carries:

    nemotron-3.5-onnx-320ms/
        nemotron_onnx_config.json  tokens.txt  decoder.onnx  joiner.onnx
        encoder_320ms_first_fp16.onnx  encoder_320ms_first_fp16.onnx.data
        encoder_320ms_fp16.onnx        encoder_320ms_fp16.onnx.data
        LICENSE-OpenMDW-1.1  NOTICE  SHA256SUMS
    nemotron-3.5-onnx-320ms-<rev>.tar

The steps are the community tooling's, vendored under `vendor/` unchanged
(Apache-2.0, see `vendor/LICENSE`):

  1. `vendor/export_onnx.py`  — HF checkpoint -> fp32 graphs, one chunk size
                                 (first-chunk and steady encoders, decoder,
                                 joiner, config, tokens), with its own
                                 ONNX-vs-PyTorch parity checks
  2. `vendor/quantize.py`     — `convert_fp16`, the >2 GiB-safe weight cast
                                 that keeps every graph input and output fp32

Then this script assembles the layout, writes NOTICE, hashes everything and
tars it. The decoder and joiner stay fp32, as in the export the plugin was
validated against. The two encoder weight files come out byte-identical
(the graphs differ, the weights do not); both names are kept because ONNX
Runtime resolves each graph's weights by the name the graph carries, and
the download script fetches such a pair once.

Needs the environment of `vendor/../requirements` (torch, transformers 5.13+,
onnx, onnxruntime, librosa, sentencepiece); CPU is enough. The checkpoint
(about 2.5 GB) is fetched into the Hugging Face cache on first use.

    python voice-pipes/scripts/nemotron35/make_export.py --work /tmp/n35 --rev r2
"""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import subprocess
import sys
import tarfile
from datetime import date
from pathlib import Path

HERE = Path(__file__).resolve().parent
VENDOR = HERE / "vendor"
MODEL_ID = "nvidia/nemotron-3.5-asr-streaming-0.6b"
CHUNK_MS = 320
LAYOUT = "nemotron-3.5-onnx-320ms"

NOTICE = """\
NOTICE
======

This directory holds an ONNX export of

    {model_id}
    https://huggingface.co/{model_id}
    checkpoint revision {checkpoint_rev}

made by the Voice Pipes project on {today} ({rev}) at the {chunk_ms} ms chunk
size, encoder weights cast to fp16, decoder and joiner fp32.

The model weights, including this conversion, remain the property of NVIDIA
Corporation and are distributed under the OpenMDW License Agreement v1.1
(see LICENSE-OpenMDW-1.1 and https://openmdw.ai/license/1-1/).

Attribution: "NVIDIA Nemotron 3.5 ASR ({model_id}), NVIDIA Corporation".
This export is not affiliated with, sponsored by, or endorsed by NVIDIA.

The export and fp16 conversion code is the community tooling by
@codavidgarcia (https://github.com/codavidgarcia/nemotron-3.5-asr-streaming-onnx),
Apache-2.0, vendored unchanged at commit {tooling_commit}.
"""

FILES = [
    "nemotron_onnx_config.json",
    "tokens.txt",
    "decoder.onnx",
    "joiner.onnx",
    f"encoder_{CHUNK_MS}ms_first_fp16.onnx",
    f"encoder_{CHUNK_MS}ms_fp16.onnx",
    f"encoder_{CHUNK_MS}ms_first_fp16.onnx.data",
    f"encoder_{CHUNK_MS}ms_fp16.onnx.data",
    "LICENSE-OpenMDW-1.1",
    "NOTICE",
]


def sha256_of(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def run(cmd: list[str]) -> None:
    print("+", " ".join(str(c) for c in cmd), flush=True)
    subprocess.run(cmd, check=True)


def checkpoint_revision(model_id: str) -> str:
    """The commit of the checkpoint the cache holds, for the NOTICE."""
    try:
        from huggingface_hub import HfApi

        return HfApi().model_info(model_id).sha or "unknown"
    except Exception:  # noqa: BLE001 — the NOTICE says unknown rather than the run failing
        return "unknown"


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--work", type=Path, required=True, help="scratch directory (needs ~12 GB)")
    ap.add_argument("--rev", required=True, help="release revision label, e.g. r2")
    ap.add_argument("--model-id", default=MODEL_ID)
    ap.add_argument("--device", default="cpu")
    ap.add_argument("--tooling-commit", default="unknown", help="vendored tooling's upstream commit, for the NOTICE")
    ap.add_argument("--skip-export", action="store_true", help="reuse fp32 graphs already in --work/fp32")
    args = ap.parse_args()

    py = sys.executable
    fp32 = args.work / "fp32"
    out = args.work / LAYOUT
    args.work.mkdir(parents=True, exist_ok=True)

    # 1. fp32 export with the tooling's own parity checks
    if not args.skip_export:
        run([py, str(VENDOR / "export_onnx.py"), "--model-id", args.model_id, "--output-dir", str(fp32),
             "--chunk-ms", str(CHUNK_MS), "--device", args.device, "--validate"])

    # 2. fp16 cast of the two encoders, in their own directories
    sys.path.insert(0, str(VENDOR))
    from quantize import convert_fp16  # noqa: E402

    if out.exists():
        shutil.rmtree(out)
    out.mkdir(parents=True)
    for stem in (f"encoder_{CHUNK_MS}ms_first", f"encoder_{CHUNK_MS}ms"):
        src = fp32 / stem / f"{stem}.onnx"
        dst = fp32 / stem / f"{stem}_fp16.onnx"
        convert_fp16(src, dst)
        shutil.copyfile(dst, out / dst.name)
        shutil.copyfile(dst.with_name(dst.name + ".data"), out / (dst.name + ".data"))

    # 3. the rest of the layout
    for name in ("decoder.onnx", "joiner.onnx", "nemotron_onnx_config.json", "tokens.txt"):
        shutil.copyfile(fp32 / name, out / name)
    shutil.copyfile(HERE / "LICENSE-OpenMDW-1.1", out / "LICENSE-OpenMDW-1.1")
    (out / "NOTICE").write_text(NOTICE.format(
        model_id=args.model_id, checkpoint_rev=checkpoint_revision(args.model_id),
        today=date.today().isoformat(), rev=args.rev, chunk_ms=CHUNK_MS,
        tooling_commit=args.tooling_commit,
    ), encoding="utf-8", newline="\n")

    # 4. hashes and the archive
    sums = {name: sha256_of(out / name) for name in FILES}
    (out / "SHA256SUMS").write_text("".join(f"{h}  {n}\n" for n, h in sums.items()), encoding="utf-8", newline="\n")
    archive = args.work / f"{LAYOUT}-{args.rev}.tar"
    with tarfile.open(archive, "w") as tar:
        for name in FILES + ["SHA256SUMS"]:
            if name == f"encoder_{CHUNK_MS}ms_fp16.onnx.data" and sums[name] == sums[f"encoder_{CHUNK_MS}ms_first_fp16.onnx.data"]:
                continue  # identical weights ship once; the download script links the second name
            tar.add(out / name, arcname=f"{LAYOUT}/{name}")
    print()
    print(json.dumps({"archive": str(archive), "archive_sha256": sha256_of(archive),
                      "archive_bytes": archive.stat().st_size, "files": sums}, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
