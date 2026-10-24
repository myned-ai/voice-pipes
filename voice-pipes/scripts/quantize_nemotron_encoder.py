#!/usr/bin/env python3
"""Weight-only quantize the Nemotron encoder to int4/int8 `MatMulNBits`.

The companion to `export_nemotron_streaming.py` and the self-quant path.
It takes an exported FP32 model directory
and produces a *drop-in* sibling directory whose `encoder.onnx` has its large
linear (`MatMul`) weights folded into the `MatMulNBits` contrib op, leaving
the decoder, joint, mel, and tokenizer untouched.

Why only the encoder, why MatMulNBits:
  - The FastConformer encoder is >95% of the parameters (paper §6.1); the
    decoder+joint stay FP32 because the joint runs per-timestep in RNN-T
    greedy decode and rounding compounds.
  - `MatMulNBits` is the *weight-only* quantized matmul (int weights,
    float activations/accumulators). The paper (§5.4 / Table 6) shows the
    integer-ONLY ops `MatMulInteger` / `ConvInteger` wreck WER
    (8.20 -> 10.14). This script must NEVER emit those — it verifies the
    output graph and fails if it finds any. See the guard comment in
    `voice-pipes-stt-parakeet/src/encoder.rs`.

Standalone: needs only `onnx` + `onnxruntime` (NO NeMo / torch / CUDA), so
it runs in a plain venv, decoupled from the heavy export environment:

    python -m pip install "onnx>=1.16" "onnxruntime>=1.19"

USAGE
-----
    python quantize_nemotron_encoder.py \
        --model-dir ../models/nemotron-streaming/7-10-7 \
        --out-dir   ../models/nemotron-streaming/7-10-7-int4 \
        --bits 4 --block-size 32

Then A/B it against the FP32 baseline with the WER gate:
    scripts/benchmarks/run-nemotron-wer.ps1 -Release `
        -ModelDir models/nemotron-streaming/7-10-7 `
        -ModelDir models/nemotron-streaming/7-10-7-int4 `
        -Manifest <labeled-corpus>.tsv

NOTES
-----
  - Default algorithm is RTN (round-to-nearest) — no calibration data
    needed. GPTQ (calibration-based, usually lower WER loss) is flagged as
    a follow-up: it needs a representative dataset wired in.
  - `--exclude-substr` keeps WER-sensitive layers in FP32 by excluding any
    node whose name contains the given substring (repeatable). Good
    candidates if int4 regresses: the subsampling front-end and the final
    output projection.
"""

from __future__ import annotations

import argparse
import shutil
import sys
from collections import Counter
from pathlib import Path

# Forbidden ops — emitting either of these means we quantized the wrong way
# and would tank WER (paper §5.4 / Table 6). Verified against the output.
FORBIDDEN_OPS = ("MatMulInteger", "ConvInteger")


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(
        description="Weight-only quantize the Nemotron encoder to MatMulNBits.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=__doc__,
    )
    p.add_argument("--model-dir", type=Path, required=True,
                   help="Exported FP32 model dir (contains encoder.onnx).")
    p.add_argument("--out-dir", type=Path, default=None,
                   help="Output dir (default: <model-dir>-int<bits>). "
                        "Created as a drop-in sibling: quantized encoder.onnx "
                        "plus copies of decoder/joint/mel/tokenizer/metadata.")
    p.add_argument("--bits", type=int, choices=(4, 8), default=4,
                   help="Weight bit-width (default: 4).")
    p.add_argument("--block-size", type=int, default=32,
                   help="Quantization block size (default: 32). Smaller = "
                        "more accurate, larger = smaller file.")
    p.add_argument("--symmetric", dest="symmetric", action="store_true",
                   default=True, help="Symmetric quantization (default).")
    p.add_argument("--asymmetric", dest="symmetric", action="store_false",
                   help="Asymmetric quantization.")
    p.add_argument("--accuracy-level", type=int, default=None,
                   help="MatMulNBits accuracy_level (0-4). Higher = more "
                        "accurate accumulation. Leave unset for the kernel "
                        "default.")
    p.add_argument("--exclude-substr", action="append", default=[],
                   metavar="SUBSTR",
                   help="Exclude any MatMul node whose name contains SUBSTR "
                        "from quantization (keep it FP32). Repeatable.")
    p.add_argument("--external-data", action="store_true",
                   help="Force external-data output even if the model fits "
                        "in a single <2GB file.")
    return p.parse_args()


def external_data_files(model, base_dir: Path) -> set[str]:
    """Filenames referenced as external data by the model's initializers."""
    import onnx

    files: set[str] = set()
    for init in model.graph.initializer:
        if init.data_location != onnx.TensorProto.EXTERNAL:
            continue
        for entry in init.external_data:
            if entry.key == "location":
                files.add(entry.value)
    return files


def op_histogram(model) -> Counter:
    return Counter(node.op_type for node in model.graph.node)


def find_excluded_nodes(model, substrs: list[str]) -> list[str]:
    if not substrs:
        return []
    excluded = []
    for node in model.graph.node:
        if node.op_type != "MatMul":
            continue
        if any(s in node.name for s in substrs):
            excluded.append(node.name)
    return excluded


def _construct(cls, model, args, nodes_to_exclude, algo_config):
    """Call a quantizer constructor, progressively dropping kwargs the
    installed onnxruntime version doesn't accept.

    Across ORT 1.16-1.22 the NBits constructor signature drifted: `bits`,
    `accuracy_level`, and `algo_config` came and went, while `block_size` /
    `is_symmetric` / `nodes_to_exclude` are the stable core. We try the
    richest call first and peel optional kwargs off on `TypeError`.
    """
    rich = dict(
        block_size=args.block_size,
        is_symmetric=args.symmetric,
        nodes_to_exclude=nodes_to_exclude,
    )
    if args.bits != 4:
        rich["bits"] = args.bits  # 4-bit is the implicit default everywhere
    if args.accuracy_level is not None:
        rich["accuracy_level"] = args.accuracy_level
    if algo_config is not None:
        rich["algo_config"] = algo_config

    # Peel order: most-volatile kwargs first.
    for drop in ([], ["accuracy_level"], ["bits"], ["accuracy_level", "bits"],
                 ["algo_config"], ["accuracy_level", "bits", "algo_config"]):
        kwargs = {k: v for k, v in rich.items() if k not in drop}
        try:
            return cls(model, **kwargs)
        except TypeError:
            continue
    # Last resort: bare core.
    return cls(model, block_size=args.block_size, is_symmetric=args.symmetric,
               nodes_to_exclude=nodes_to_exclude)


def build_quantizer(model, args, nodes_to_exclude):
    """Construct an NBits quantizer, tolerant of onnxruntime API churn.

    Prefers the newer `MatMulNBitsQuantizer` (4 *and* 8 bit); falls back to
    the older 4-bit-only `MatMul4BitsQuantizer` when bits==4.
    """
    last_err = None

    # Path 1: MatMulNBitsQuantizer (ORT >= 1.19). Try to attach an RTN algo
    # config if the class exists and is constructible, but the quantizer
    # works without one (defaults to RTN 4-bit).
    try:
        from onnxruntime.quantization.matmul_nbits_quantizer import (
            MatMulNBitsQuantizer,
        )
        algo_config = None
        try:
            from onnxruntime.quantization.matmul_nbits_quantizer import (
                RTNWeightOnlyQuantConfig,
            )
            for kw in (dict(bits=args.bits), {}):
                try:
                    algo_config = RTNWeightOnlyQuantConfig(**kw)
                    break
                except TypeError:
                    continue
        except ImportError:
            pass
        return _construct(MatMulNBitsQuantizer, model, args,
                          nodes_to_exclude, algo_config)
    except ImportError as exc:
        last_err = exc

    # Path 2: legacy MatMul4BitsQuantizer (ORT 1.16-1.19, 4-bit only).
    if args.bits == 4:
        try:
            from onnxruntime.quantization.matmul_4bits_quantizer import (
                MatMul4BitsQuantizer,
            )
            return _construct(MatMul4BitsQuantizer, model, args,
                              nodes_to_exclude, None)
        except ImportError as exc:
            last_err = exc

    raise RuntimeError(
        f"could not construct an NBits quantizer (bits={args.bits}). "
        f"Install onnxruntime>=1.19 (>=1.20 for 8-bit). "
        f"Underlying error: {last_err!r}"
    )


def copy_companions(src: Path, dst: Path, encoder_externals: set[str]) -> None:
    """Copy everything except the old encoder graph + its weight sidecars."""
    skip = {"encoder.onnx", "export_metadata.json"} | encoder_externals
    for item in src.iterdir():
        if item.name in skip or not item.is_file():
            continue
        shutil.copy2(item, dst / item.name)
        print(f"[quant]   copied {item.name}")


def patch_metadata(src: Path, dst: Path, args, nodes_excluded: int) -> None:
    import json

    meta_path = src / "export_metadata.json"
    meta = {}
    if meta_path.is_file():
        try:
            meta = json.loads(meta_path.read_text())
        except Exception:  # noqa: BLE001
            meta = {}
    meta["precision"] = f"encoder-int{args.bits}-nbits/decoder-fp32/joint-fp32"
    meta["encoder_quant"] = {
        "op": "MatMulNBits",
        "bits": args.bits,
        "block_size": args.block_size,
        "symmetric": bool(args.symmetric),
        "algo": "rtn",
        "accuracy_level": args.accuracy_level,
        "nodes_excluded": nodes_excluded,
    }
    (dst / "export_metadata.json").write_text(json.dumps(meta, indent=2))
    print("[quant]   wrote export_metadata.json (precision + encoder_quant)")


def main() -> int:
    args = parse_args()
    import onnx

    src = args.model_dir
    if not src.is_dir():
        sys.stderr.write(f"ERROR: --model-dir not found: {src}\n")
        return 2
    enc_path = src / "encoder.onnx"
    if not enc_path.is_file():
        sys.stderr.write(f"ERROR: encoder.onnx not found in {src}\n")
        return 2

    out = args.out_dir or src.parent / f"{src.name}-int{args.bits}"
    out.mkdir(parents=True, exist_ok=True)
    print(f"[quant] src = {src}")
    print(f"[quant] out = {out}")
    print(f"[quant] bits={args.bits} block_size={args.block_size} "
          f"symmetric={args.symmetric}")

    # Load with external data resolved relative to the encoder's directory.
    print("[quant] loading FP32 encoder (resolving external data)...")
    model = onnx.load(str(enc_path), load_external_data=True)
    externals = external_data_files(model, src)

    before = op_histogram(model)
    n_matmul = before.get("MatMul", 0)
    n_gemm = before.get("Gemm", 0)
    print(f"[quant] op histogram (encoder): MatMul={n_matmul} Gemm={n_gemm} "
          f"Conv={before.get('Conv', 0)}")
    if n_matmul == 0:
        print("[quant] WARNING: zero MatMul nodes found. This graph likely "
              "exports linear layers as `Gemm`, which MatMulNBits does NOT "
              "target. The quantized output would be identical to FP32. "
              "Re-export with MatMul-form linears, or extend this script to "
              "pre-convert Gemm->MatMul, before trusting any size/WER win.")

    nodes_to_exclude = find_excluded_nodes(model, args.exclude_substr)
    if args.exclude_substr:
        print(f"[quant] excluding {len(nodes_to_exclude)} MatMul node(s) "
              f"matching {args.exclude_substr}")

    print("[quant] quantizing (this can take a minute)...")
    quantizer = build_quantizer(model, args, nodes_to_exclude)
    quantizer.process()
    qmodel = quantizer.model.model  # ONNXModel -> ModelProto

    after = op_histogram(qmodel)
    n_nbits = after.get("MatMulNBits", 0)
    print(f"[quant] result: MatMulNBits={n_nbits} "
          f"MatMul(remaining)={after.get('MatMul', 0)} "
          f"Conv={after.get('Conv', 0)}")

    # Hard guard: the integer-only ops are a known WER trap.
    found_forbidden = [op for op in FORBIDDEN_OPS if after.get(op, 0) > 0]
    if found_forbidden:
        sys.stderr.write(
            f"ERROR: output graph contains forbidden integer-only op(s) "
            f"{found_forbidden} — this would tank WER (paper §5.4). Aborting "
            f"without writing.\n"
        )
        return 3
    if n_nbits == 0:
        print("[quant] WARNING: produced 0 MatMulNBits nodes — nothing was "
              "actually quantized (see the Gemm note above). Writing anyway "
              "so the dir is a valid drop-in, but expect no size/WER change.")

    # Save the quantized encoder. int4 of a 0.6B encoder fits well under the
    # 2GB single-file protobuf limit, so default to a clean self-contained
    # file; fall back to external data on request or if it overflows.
    out_enc = out / "encoder.onnx"
    use_ext = args.external_data
    try:
        quantizer.model.save_model_to_file(str(out_enc), use_ext)
    except (ValueError, RuntimeError) as exc:
        if use_ext:
            raise
        print(f"[quant]   single-file save failed ({exc}); retrying with "
              f"external data.")
        quantizer.model.save_model_to_file(str(out_enc), True)
    print(f"[quant]   wrote {out_enc.name} "
          f"({out_enc.stat().st_size / 1e6:.1f} MB graph)")

    copy_companions(src, out, externals)
    patch_metadata(src, out, args, len(nodes_to_exclude))

    print("[quant] done. Validate with the WER gate before shipping:")
    print(f"        run-nemotron-wer.ps1 -Release -ModelDir {src.name} "
          f"-ModelDir {out.name} -Manifest <corpus>.tsv")
    return 0


if __name__ == "__main__":
    sys.exit(main())
