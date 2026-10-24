#!/usr/bin/env python3
"""Export the pretrained Voice Activity Projection (VAP) checkpoint to ONNX.

VAP predicts, continuously and from *both* sides of a conversation, who will
hold the floor over the next two seconds. That is what the anticipatory turn
detector consumes: unlike an end-of-turn classifier it can be asked while the
user is still talking, so the pipeline gets lead time instead of a verdict
after the fact.

This is a one-off model-preparation tool, not part of the build. It needs a
Python environment the runtime does not: torch, torchaudio, onnx, einops, and
a checkout of the VAP source for the model definition.

Usage:
    git clone --depth 1 https://github.com/ErikEkstedt/VAP.git
    uv venv --python 3.11 VAP/.venv
    uv pip install --python VAP/.venv/Scripts/python.exe \
        torch torchaudio --index-url https://download.pytorch.org/whl/cpu
    uv pip install --python VAP/.venv/Scripts/python.exe onnx einops
    VAP/.venv/Scripts/python.exe scripts/export-vap-onnx.py \
        --vap-root VAP --window-seconds 2 --out models/vap/vap-2s.onnx

Upstream is MIT licensed (Erik Ekstedt); the checkpoint ships in the repo at
`example/checkpoints/VAP_state_dict.pt`. Constructing `EncoderCPC` also pulls
the LibriLight CPC weights into the torch hub cache, but they are immediately
overwritten by the checkpoint, so the export does not depend on them.

Fixed window, deliberately
--------------------------
The graph is exported at one fixed input length. A dynamic-length export bakes
an attention-sized tensor at trace length and then fails on every other input
("Attempting to broadcast an axis by a dimension other than 1"), so the window
is a property of the exported file. The Rust detector keeps a ring buffer of
exactly this many seconds and re-runs the whole window each time.

Measured on CPU (onnxruntime, 1 thread per run, x86_64), with `%core` the
share of one core consumed when the detector re-runs every 200 ms:

    window   frames   ms/run    xRT   %core
       1 s       50     12.3     81      6%
       2 s      100     21.1     95     11%
       3 s      150     30.6     98     15%
       5 s      250     51.1     98     26%
      10 s      500    117.9     85     59%
      20 s     1000    345.6     58    173%

The checkpoint was trained with 20 s of context, which is not affordable to
run continuously; 2-3 s is. Shorter context is reported to cost little
accuracy, but that has not been measured here -- validate a window on real
dialogue audio before trusting it in a pipeline.

Outputs
-------
    waveform   (1, 2, window * 16000) float32, channel 0 = user, 1 = agent
    p_now      (1, frames) P(channel 0 holds the floor), bins 0-1
    p_future   (1, frames) same over bins 2-3, i.e. ~0.6-1.2 s ahead
    vad        (1, frames, 2) per-channel voice activity

`p_now`/`p_future` are aggregated inside the graph so the caller does not have
to fold 256 raw logits back through the objective's codebook.
"""
from __future__ import annotations

import argparse
import sys
from pathlib import Path

SAMPLE_RATE = 16_000


def build_model(vap_root: Path, checkpoint: Path):
    """Construct VAP and load the pretrained weights."""
    sys.path.insert(0, str(vap_root))

    import torch
    from vap.modules.VAP import VAP
    from vap.modules.encoder import EncoderCPC
    from vap.modules.modules import TransformerStereo

    model = VAP(EncoderCPC(load_pretrained=False), TransformerStereo())
    state = torch.load(checkpoint, map_location="cpu")
    missing, unexpected = model.load_state_dict(state, strict=False)
    if missing or unexpected:
        raise SystemExit(
            f"checkpoint does not match the model definition\n"
            f"  missing:    {missing[:5]}\n"
            f"  unexpected: {unexpected[:5]}"
        )
    model.eval()
    return model


def wrap(model):
    """Wrap VAP so the graph emits aggregated probabilities as a tuple."""
    import torch.nn as nn

    class VapOnnx(nn.Module):
        def __init__(self, inner):
            super().__init__()
            self.inner = inner

        def forward(self, waveform):
            out = self.inner(waveform)
            probs = out["logits"].softmax(dim=-1)
            objective = self.inner.objective
            p_now = objective.probs_next_speaker_aggregate(probs, from_bin=0, to_bin=1)
            p_future = objective.probs_next_speaker_aggregate(probs, from_bin=2, to_bin=3)
            return p_now, p_future, out["vad"].sigmoid()

    wrapper = VapOnnx(model)
    wrapper.eval()
    return wrapper


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--vap-root", type=Path, required=True,
                    help="checkout of github.com/ErikEkstedt/VAP")
    ap.add_argument("--checkpoint", type=Path, default=None,
                    help="state dict (default: <vap-root>/example/checkpoints/VAP_state_dict.pt)")
    ap.add_argument("--window-seconds", type=float, default=2.0,
                    help="fixed input window; see the table in this file's docstring")
    ap.add_argument("--out", type=Path, required=True, help="output .onnx path")
    ap.add_argument("--opset", type=int, default=17)
    ap.add_argument("--skip-verify", action="store_true",
                    help="skip the torch-vs-onnxruntime parity check")
    args = ap.parse_args()

    checkpoint = args.checkpoint or (
        args.vap_root / "example" / "checkpoints" / "VAP_state_dict.pt"
    )
    if not checkpoint.is_file():
        raise SystemExit(f"checkpoint not found: {checkpoint}")

    import torch

    model = build_model(args.vap_root, checkpoint)
    wrapper = wrap(model)

    samples = int(SAMPLE_RATE * args.window_seconds)
    dummy = torch.randn(1, 2, samples)

    args.out.parent.mkdir(parents=True, exist_ok=True)
    torch.onnx.export(
        wrapper,
        (dummy,),
        str(args.out),
        input_names=["waveform"],
        output_names=["p_now", "p_future", "vad"],
        opset_version=args.opset,
        do_constant_folding=True,
        # The dynamo exporter writes weights to a sidecar .data file and its
        # dynamic shapes do not survive; the legacy path gives one file.
        dynamo=False,
    )
    print(f"wrote {args.out} ({args.out.stat().st_size / 1048576:.1f} MB), "
          f"window={args.window_seconds}s, samples={samples}")

    if args.skip_verify:
        return

    import numpy as np
    import onnxruntime as ort

    with torch.no_grad():
        expected = wrapper(dummy)

    session = ort.InferenceSession(str(args.out), providers=["CPUExecutionProvider"])
    actual = session.run(None, {"waveform": dummy.numpy()})

    ok = True
    for name, want, got in zip(("p_now", "p_future", "vad"), expected, actual):
        want = want.numpy()
        close = np.allclose(want, got, rtol=1e-3, atol=1e-4)
        ok &= close
        print(f"  {name:<9} shape={got.shape} match={close} "
              f"max_abs_diff={np.abs(want - got).max():.2e}")
    if not ok:
        raise SystemExit("parity check failed: the graph does not match torch")


if __name__ == "__main__":
    main()
