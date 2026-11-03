#!/usr/bin/env python3
"""Export NVIDIA Nemotron-Speech-Streaming-0.6B to cache-aware streaming ONNX.

Part of the Nemotron FP32 spike work.

This script wraps NeMo's built-in export path for cache-aware streaming
EncDecRNNTBPEModel (FastConformer-RNNT family) to produce three ONNX graphs
plus the SentencePiece vocabulary:

    <out_dir>/
        nemo_mel.onnx              — log-mel preprocessor (separately exported)
        encoder.onnx               — cache-aware FastConformer encoder
        decoder_joint.onnx         — RNN-T decoder + joint network
        tokenizer.model            — SentencePiece BPE
        export_metadata.json       — streaming config + I/O contract dump

The "streaming config" is the (left, chunk, right) tuple from paper Table 5
expressed in *subsampled* encoder frames.  Two configurations are exported by
default per the plan:

    (7, 10, 7)   — 560 ms chunk / 5.6 s history / 560 ms delay
    (2, 35, 2)   — 160 ms chunk / 28 s history / 160 ms delay

Each configuration produces its own subdirectory because cache tensor shapes
depend on `left`.

USAGE
-----

    python export_nemotron_streaming.py \
        --nemo-file ./nvidia_nemotron-speech-streaming-en-0.6b.nemo \
        --out-dir   ../models/nemotron-streaming \
        --config 7,10,7 \
        --config 2,35,2

Run with no `--config` flags to export both default configurations.

ENVIRONMENT
-----------

NeMo currently requires a CUDA-capable PyTorch build for `restore_from`,
even though export itself runs on CPU.  See README-export-nemotron.md for
the exact pinned environment.

LICENSE
-------

The Nemotron checkpoint is distributed under the NVIDIA Open Model License.
Re-distribution of the exported ONNX requires preserving the original
attribution; see README-export-nemotron.md.
"""

from __future__ import annotations

import argparse
import json
import shutil
import sys
from dataclasses import dataclass
from pathlib import Path

# NeMo imports are deferred so `--help` works without the heavy env present.


@dataclass(frozen=True)
class StreamingConfig:
    left: int
    chunk: int
    right: int

    @classmethod
    def parse(cls, raw: str) -> "StreamingConfig":
        parts = [p.strip() for p in raw.split(",")]
        if len(parts) != 3:
            raise argparse.ArgumentTypeError(
                f"streaming config must be 'left,chunk,right' (got '{raw}')"
            )
        return cls(*(int(p) for p in parts))

    @property
    def label(self) -> str:
        return f"{self.left}-{self.chunk}-{self.right}"


DEFAULT_CONFIGS: list[StreamingConfig] = [
    StreamingConfig(7, 10, 7),
    StreamingConfig(2, 35, 2),
]


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(
        description="Export Nemotron-Speech-Streaming to cache-aware ONNX.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=__doc__,
    )
    p.add_argument(
        "--nemo-file",
        type=Path,
        required=True,
        help="Path to the .nemo checkpoint downloaded from HuggingFace.",
    )
    p.add_argument(
        "--out-dir",
        type=Path,
        required=True,
        help="Output directory; one subdirectory per streaming config will be created.",
    )
    p.add_argument(
        "--config",
        type=StreamingConfig.parse,
        action="append",
        help="Streaming config as 'left,chunk,right' in subsampled frames. "
        "Repeat to export multiple. Defaults to both paper Table 5 configs.",
    )
    p.add_argument(
        "--skip-mel",
        action="store_true",
        help="Skip mel preprocessor export (use the existing models/parakeet-tdt/nemo128.onnx if compatible).",
    )
    p.add_argument(
        "--opset",
        type=int,
        default=17,
        help="ONNX opset version (default: 17).",
    )
    return p.parse_args()


def lazy_import_nemo():
    try:
        from nemo.collections.asr.models import EncDecRNNTBPEModel  # type: ignore
    except ImportError as exc:  # pragma: no cover
        sys.stderr.write(
            "ERROR: NeMo is not installed. See README-export-nemotron.md "
            f"for the pinned environment.\nUnderlying error: {exc}\n"
        )
        sys.exit(2)
    return EncDecRNNTBPEModel


def configure_streaming(model, cfg: StreamingConfig) -> None:
    """Apply (left, chunk, right) cache-aware streaming config to the encoder.

    NeMo's FastConformer encoder exposes `set_default_att_context_size` and
    `setup_streaming_params` for this purpose.  We set both because different
    NeMo versions expose them under slightly different names.

    CRITICAL: setting the streaming config above only changes how the encoder
    runs in PyTorch — it does NOT cause `torch.onnx.export` to expose the
    attention/conv caches as graph I/O. For that we must additionally enable
    `export_cache_support`, which switches `forward_for_export` to the
    streaming variant whose signature is
        (audio_signal, length, cache_last_channel, cache_last_time,
         cache_last_channel_len)
        -> (encoded, encoded_len, *cache_next).
    Without this flag the resulting ONNX is a whole-utterance encoder.
    """
    encoder = model.encoder

    # Newer NeMo: explicit attention context size
    if hasattr(encoder, "set_default_att_context_size"):
        encoder.set_default_att_context_size([cfg.left, cfg.right])

    # Cache-aware streaming params (chunk size + history)
    if hasattr(encoder, "setup_streaming_params"):
        encoder.setup_streaming_params(
            chunk_size=cfg.chunk,
            shift_size=cfg.chunk,
            left_chunks=cfg.left,
        )

    # Enable cache I/O on the exported graph. The flag has moved between
    # NeMo versions; try every known location and report which one stuck.
    enabled_via = _enable_export_cache_support(encoder)
    if enabled_via is None:
        print(
            "[export]   WARNING: could not find an `export_cache_support` "
            "knob on this NeMo build — exported ONNX will be whole-utterance, "
            "not cache-aware streaming. Inspect the encoder class:\n"
            f"           {type(encoder).__module__}.{type(encoder).__name__}"
        )
    else:
        print(f"[export]   cache-aware streaming export enabled via {enabled_via}")


def _probe_initial_cache_shapes(encoder) -> dict:
    """Best-effort probe of the encoder's initial cache tensor shapes.

    The ONNX export leaves cache dims (`cache_T`, `conv_T`) symbolic so the
    Rust runtime can't size its zero-init buffers correctly without help.
    NeMo's ConformerEncoder exposes `get_initial_cache_state(batch_size,
    dtype, device, max_dim)` on recent builds — we call it with batch=1
    and unpack the resulting tensor shapes by name. If anything is missing
    we return {} and the runtime falls back to its old heuristic.
    """
    try:
        import torch  # type: ignore
    except Exception:
        return {}
    if not hasattr(encoder, "get_initial_cache_state"):
        return {}
    try:
        # NeMo signatures vary; try the common one first.
        try:
            init = encoder.get_initial_cache_state(
                batch_size=1, dtype=torch.float32, device="cpu", max_dim=0
            )
        except TypeError:
            init = encoder.get_initial_cache_state(batch_size=1)
    except Exception as exc:  # noqa: BLE001
        print(f"[export]   note: get_initial_cache_state() failed ({exc}); "
              "runtime will heuristic-size cache.")
        return {}

    # init is typically a tuple (cache_last_channel, cache_last_time,
    # cache_last_channel_len) or similar. Map by tensor rank/dtype.
    #
    # IMPORTANT: NeMo's get_initial_cache_state returns rank-4 tensors in
    # `[layers, B, ...]` order, but the ONNX export uses `[B, layers, ...]`
    # (batch-first) for the cache I/O. We swap the first two axes when
    # recording the shape so the Rust runtime can feed it directly without
    # transposing.
    shapes: dict = {}
    if not isinstance(init, (list, tuple)):
        init = (init,)
    for t in init:
        if not hasattr(t, "shape"):
            continue
        shape = list(t.shape)
        if len(shape) == 4:
            # Swap [layers, B, ...] -> [B, layers, ...] to match ONNX I/O.
            shape = [shape[1], shape[0], shape[2], shape[3]]
            # cache_last_channel: [B, layers, cache_T, D]   (D usually largest)
            # cache_last_time:    [B, layers, D, conv_T]    (conv_T usually smallest)
            # Distinguish by which trailing axis is larger: D=1024 typically.
            if shape[2] <= shape[3]:
                shapes["cache_last_channel"] = shape
            else:
                shapes["cache_last_time"] = shape
        elif len(shape) == 1:
            shapes["cache_last_channel_len"] = shape
    if shapes:
        print(f"[export]   cache_init_shapes (B=1): {shapes}")
    return shapes


def _enable_export_cache_support(encoder) -> str | None:
    """Try every known way to flip cache-streaming export on. Return the
    location that worked, or None if nothing was found."""

    # 1. Direct attribute (NeMo >= 1.21 ConformerEncoder)
    if hasattr(encoder, "export_cache_support"):
        try:
            encoder.export_cache_support = True
            return "encoder.export_cache_support = True"
        except Exception:
            pass

    # 2. Exportable mixin: set_export_config (NeMo Exportable interface)
    if hasattr(encoder, "set_export_config"):
        try:
            encoder.set_export_config({"cache_support": "True"})
            return 'encoder.set_export_config({"cache_support": "True"})'
        except Exception:
            pass

    # 3. Streaming-config sub-object (some NeMo branches)
    if hasattr(encoder, "streaming_cfg"):
        cfg_obj = encoder.streaming_cfg
        if hasattr(cfg_obj, "export_cache_support"):
            try:
                cfg_obj.export_cache_support = True
                return "encoder.streaming_cfg.export_cache_support = True"
            except Exception:
                pass

    return None


def dump_io_contract(onnx_path: Path) -> dict:
    """Read back the ONNX file and dump its input/output names + shapes.

    The acceptance criteria require logging cache tensor
    shapes; this is the canonical place to capture them.
    """
    import onnx  # type: ignore

    model = onnx.load(str(onnx_path))
    contract = {"inputs": [], "outputs": []}
    for tensor_list, key in ((model.graph.input, "inputs"), (model.graph.output, "outputs")):
        for tensor in tensor_list:
            shape = []
            for dim in tensor.type.tensor_type.shape.dim:
                shape.append(dim.dim_param if dim.dim_param else dim.dim_value)
            contract[key].append({"name": tensor.name, "shape": shape})
    return contract


def _extract_tokenizer_from_nemo(nemo_file: Path, out_dir: Path) -> None:
    """Extract SentencePiece tokenizer artifacts from the .nemo tar archive.

    NeMo's `.nemo` is a tar (sometimes gzipped) containing the model
    weights, config YAML, and the original SentencePiece artifacts under
    hash-prefixed names like `<32-hex>_tokenizer.model`. NeMo's
    `model.tokenizer.model_path` is unreliable post-`restore_from()`
    (the temp extraction dir is cleaned up), so we crack the archive
    ourselves and rename to stable `tokenizer.model` / `tokenizer.vocab`
    for the Rust runtime to pick up.
    """
    import tarfile

    wants = ("tokenizer.model", "tokenizer.vocab")
    found: dict[str, "tarfile.TarInfo"] = {}
    try:
        with tarfile.open(str(nemo_file), "r:*") as tar:
            for member in tar.getmembers():
                base = Path(member.name).name
                for want in wants:
                    if want in found:
                        continue
                    if base == want or base.endswith("_" + want):
                        found[want] = member
                        break

            for want in wants:
                member = found.get(want)
                if member is None:
                    print(f"[export]   WARNING: {want} not found in .nemo archive")
                    continue
                src = tar.extractfile(member)
                if src is None:
                    print(f"[export]   WARNING: could not read {member.name} from .nemo")
                    continue
                (out_dir / want).write_bytes(src.read())
                print(f"[export]   tokenizer → {want}")
    except Exception as exc:  # noqa: BLE001
        print(f"[export]   WARNING: tokenizer extraction failed: {exc!r}")


def export_one(
    EncDecRNNTBPEModel,
    nemo_file: Path,
    out_root: Path,
    cfg: StreamingConfig,
    opset: int,
    skip_mel: bool,
) -> None:
    out_dir = out_root / cfg.label
    out_dir.mkdir(parents=True, exist_ok=True)
    print(f"[export] config={cfg.label} → {out_dir}")

    model = EncDecRNNTBPEModel.restore_from(str(nemo_file), map_location="cpu")
    model.eval()
    configure_streaming(model, cfg)

    # 1. Encoder
    encoder_path = out_dir / "encoder.onnx"
    print(f"[export]   encoder → {encoder_path.name}")
    model.encoder.export(str(encoder_path), onnx_opset_version=opset)

    # 2. Decoder + joint network
    decoder_joint_path = out_dir / "decoder_joint.onnx"
    print(f"[export]   decoder+joint → {decoder_joint_path.name}")
    # NeMo exposes a fused decoder+joint export helper on the model itself.
    if hasattr(model, "decoder_joint_export"):
        model.decoder_joint_export(str(decoder_joint_path), onnx_opset_version=opset)
    else:
        # Fallback: export decoder and joint separately.
        model.decoder.export(str(out_dir / "decoder.onnx"), onnx_opset_version=opset)
        model.joint.export(str(out_dir / "joint.onnx"), onnx_opset_version=opset)

    # 3. Mel preprocessor (optional — Parakeet's nemo128.onnx is reusable
    # iff the mel filterbank shape matches; we still export by default to
    # avoid surprise drift).
    #
    # Known issue: PyTorch's ONNX exporter raises
    #   `STFT does not currently support complex types`
    # for NeMo's FilterbankFeatures at every opset >= 14. We treat this
    # as non-fatal so the encoder/decoder/joint exports (which already
    # succeeded by this point) are not lost. The Rust side falls back
    # to its own mel implementation in that case.
    mel_exported = False
    if not skip_mel:
        mel_path = out_dir / "nemo_mel.onnx"
        print(f"[export]   mel preprocessor → {mel_path.name}")
        if hasattr(model, "preprocessor") and hasattr(model.preprocessor, "export"):
            try:
                model.preprocessor.export(str(mel_path), onnx_opset_version=opset)
                mel_exported = True
            except Exception as exc:  # noqa: BLE001 — known torch.onnx STFT bug
                print(
                    f"[export]   WARNING: mel preprocessor export failed "
                    f"({type(exc).__name__}: {exc.__class__.__module__}); "
                    f"continuing without nemo_mel.onnx. "
                    f"The Rust runtime will compute mel features itself."
                )
        else:
            print("[export]   WARNING: preprocessor.export() not available — skipping mel.")

    # 4. SentencePiece tokenizer.
    #
    # NeMo doesn't reliably expose `model.tokenizer.model_path` after
    # `restore_from()` (the temp extraction dir is cleaned up). Instead we
    # crack the `.nemo` archive (a plain tar) ourselves and pull out the
    # SentencePiece artifacts. They appear as hash-prefixed entries:
    #     <hash>_tokenizer.model   — SentencePiece BPE model (binary)
    #     <hash>_tokenizer.vocab   — id<TAB>piece text vocab
    # We rename them on extraction to the names the Rust runtime expects.
    _extract_tokenizer_from_nemo(nemo_file, out_dir)

    # 5. Metadata: streaming config + I/O contract for downstream Rust code.
    #
    # We also try to capture the *initial cache shapes* the encoder expects.
    # The ONNX graph leaves these dimensions symbolic, so the Rust runtime
    # would otherwise have to guess. NeMo's encoder exposes
    # `get_initial_cache_state()` (or equivalents); when present we use it
    # to record concrete cache_last_channel / cache_last_time shapes.
    cache_init_shapes = _probe_initial_cache_shapes(model.encoder)

    metadata = {
        "model": "nvidia/nemotron-speech-streaming-en-0.6b",
        "precision": "fp32",
        "opset": opset,
        "streaming_config": {
            "left": cfg.left,
            "chunk": cfg.chunk,
            "right": cfg.right,
        },
        "io_contract": {
            "encoder": dump_io_contract(encoder_path),
        },
        "mel_exported": mel_exported,
        "cache_init_shapes": cache_init_shapes,
    }
    if decoder_joint_path.exists():
        metadata["io_contract"]["decoder_joint"] = dump_io_contract(decoder_joint_path)
    decoder_split_path = out_dir / "decoder.onnx"
    joint_split_path = out_dir / "joint.onnx"
    if decoder_split_path.exists():
        metadata["io_contract"]["decoder"] = dump_io_contract(decoder_split_path)
    if joint_split_path.exists():
        metadata["io_contract"]["joint"] = dump_io_contract(joint_split_path)
    (out_dir / "export_metadata.json").write_text(json.dumps(metadata, indent=2))
    print(f"[export]   metadata → export_metadata.json")


def main() -> int:
    args = parse_args()
    if not args.nemo_file.is_file():
        sys.stderr.write(f"ERROR: --nemo-file does not exist: {args.nemo_file}\n")
        return 2

    configs = args.config or DEFAULT_CONFIGS
    args.out_dir.mkdir(parents=True, exist_ok=True)

    EncDecRNNTBPEModel = lazy_import_nemo()

    for cfg in configs:
        export_one(
            EncDecRNNTBPEModel=EncDecRNNTBPEModel,
            nemo_file=args.nemo_file,
            out_root=args.out_dir,
            cfg=cfg,
            opset=args.opset,
            skip_mel=args.skip_mel,
        )

    print("[export] done.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
