#!/usr/bin/env python3
"""
Build a standalone mel-preprocessor ONNX graph that matches NeMo's
``AudioToMelSpectrogramPreprocessor`` defaults used by Nemotron-Speech-Streaming
(and FastConformer ASR more generally).

This avoids the long-standing torch.onnx STFT export bug
(``STFT does not currently support complex types``) by hand-building the
graph from primitives:

    waveform (1, N) float32
        → Sub (pre-emphasis 0.97)
        → Pad (reflect, n_fft//2 on each side)
        → STFT (opset-17 op, real-valued)
        → ReduceSumSquare on the (real, imag) axis  →  power spectrum
        → MatMul with precomputed slaney mel filterbank
        → Add(2^-24) + Log
        → ReduceMean / ReduceStd along time → per-feature normalize

Output graph signature (matches the encoder's `audio_signal` input):

    inputs:
        signal : float32 [1, N]              raw 16-kHz mono PCM
        length : int64   [1]                 valid sample count
    outputs:
        features        : float32 [1, n_mels, T]   log-mel
        feature_length  : int64   [1]              T per item

Only ``numpy``, ``librosa`` and ``onnx`` are required.

Usage:
    python scripts/build_nemo_mel_onnx.py \\
        --out models/nemotron-streaming/7-10-7/mel.onnx
"""

from __future__ import annotations

import argparse
from pathlib import Path

import librosa
import numpy as np
import onnx
from onnx import TensorProto, helper, numpy_helper

DEFAULT_SR = 16_000
DEFAULT_N_FFT = 512
DEFAULT_WIN = 400
DEFAULT_HOP = 160
DEFAULT_N_MELS = 128
DEFAULT_PREEMPH = 0.97
LOG_ZERO_GUARD = 2.0 ** -24


def build_mel_onnx(
    out_path: Path,
    sample_rate: int = DEFAULT_SR,
    n_fft: int = DEFAULT_N_FFT,
    win_length: int = DEFAULT_WIN,
    hop_length: int = DEFAULT_HOP,
    n_mels: int = DEFAULT_N_MELS,
    fmin: float = 0.0,
    fmax: float | None = None,
    preemph: float = DEFAULT_PREEMPH,
    pad_to: int = 16,
) -> None:
    if fmax is None:
        fmax = sample_rate / 2.0

    # 1. Pre-computed constants.
    # Hann window — NeMo uses torch.hann_window(win_length, periodic=False)
    # which is the SYMMETRIC Hann window (np.hanning(N)). Periodic and
    # symmetric variants differ at the edges and noticeably degrade ASR
    # accuracy if mismatched.
    raw_window = np.hanning(win_length).astype(np.float32)  # symmetric
    pad_left = (n_fft - win_length) // 2
    pad_right = n_fft - win_length - pad_left
    window = np.concatenate(
        [np.zeros(pad_left, dtype=np.float32), raw_window, np.zeros(pad_right, dtype=np.float32)]
    )

    # Slaney mel filterbank (librosa default: htk=False, norm='slaney').
    # Shape: (n_mels, n_fft//2 + 1)
    mel_fb = librosa.filters.mel(
        sr=sample_rate, n_fft=n_fft, n_mels=n_mels, fmin=fmin, fmax=fmax
    ).astype(np.float32)

    # Initializers
    inits = [
        numpy_helper.from_array(np.array([preemph], dtype=np.float32), "preemph_coef"),
        numpy_helper.from_array(window, "stft_window"),
        numpy_helper.from_array(np.array([n_fft], dtype=np.int64), "frame_length"),
        numpy_helper.from_array(np.array([hop_length], dtype=np.int64), "frame_step"),
        # mel filterbank as (n_freqs, n_mels) for MatMul on power[..., n_freqs]
        numpy_helper.from_array(mel_fb.T.copy(), "mel_filterbank"),
        numpy_helper.from_array(
            np.array([LOG_ZERO_GUARD], dtype=np.float32), "log_zero_guard"
        ),
        numpy_helper.from_array(np.array([1e-5], dtype=np.float32), "norm_eps"),
        numpy_helper.from_array(np.array([n_fft // 2, n_fft // 2], dtype=np.int64), "pad_amount"),
    ]

    nodes: list[helper.NodeProto] = []

    # ── Pre-emphasis: y[0]=x[0]; y[n]=x[n]-preemph*x[n-1]
    # Implemented as: y = concat( x[:, :1],  x[:, 1:] - preemph * x[:, :-1] )
    nodes += [
        helper.make_node("Slice", ["signal", "ones_int", "ones_int", "axis1"], ["x_first"]),
        helper.make_node("Slice", ["signal", "ones_int", "neg1_int", "axis1"], ["x_prev"]),
        helper.make_node("Slice", ["signal", "twos_int", "big_int", "axis1"], ["x_curr"]),
        helper.make_node("Mul", ["x_prev", "preemph_coef"], ["x_prev_sc"]),
        helper.make_node("Sub", ["x_curr", "x_prev_sc"], ["x_diff"]),
        helper.make_node("Concat", ["x_first", "x_diff"], ["x_pre"], axis=1),
    ]
    inits += [
        numpy_helper.from_array(np.array([0], dtype=np.int64), "zeros_int"),
        numpy_helper.from_array(np.array([1], dtype=np.int64), "ones_int"),
        numpy_helper.from_array(np.array([2], dtype=np.int64), "twos_int"),
        numpy_helper.from_array(np.array([-1], dtype=np.int64), "neg1_int"),
        numpy_helper.from_array(np.array([2**31 - 1], dtype=np.int64), "big_int"),
        numpy_helper.from_array(np.array([1], dtype=np.int64), "axis1"),
    ]

    # ── Reflect-pad signal by n_fft//2 on each side (pads the time/last axis).
    # ONNX Pad expects pads as [x1_begin, x2_begin, ..., x1_end, x2_end, ...].
    # For input [B=1, N], pad axis 1 by n_fft//2 each side; axis 0 by 0.
    pads = np.array([0, n_fft // 2, 0, n_fft // 2], dtype=np.int64)
    inits.append(numpy_helper.from_array(pads, "stft_pads"))
    nodes.append(helper.make_node("Pad", ["x_pre", "stft_pads"], ["x_padded"], mode="reflect"))

    # ── STFT (opset-17). Real-valued input, returns (..., n_frames, n_fft//2 + 1, 2).
    nodes.append(
        helper.make_node(
            "STFT",
            ["x_padded", "frame_step", "stft_window", "frame_length"],
            ["stft_out"],
            onesided=1,
        )
    )

    # ── Power spectrum: sum of squares over the last (real, imag) axis.
    # stft_out shape: [B, n_frames, n_freqs, 2]
    nodes.append(
        helper.make_node(
            "ReduceSumSquare",
            ["stft_out"],
            ["power"],
            axes=[-1],
            keepdims=0,
        )
    )
    # power shape: [B, n_frames, n_freqs]

    # ── Mel projection: power @ mel_filterbank → [B, n_frames, n_mels]
    nodes.append(helper.make_node("MatMul", ["power", "mel_filterbank"], ["mel_energy"]))

    # ── log(mel + 2^-24)
    nodes += [
        helper.make_node("Add", ["mel_energy", "log_zero_guard"], ["mel_guarded"]),
        helper.make_node("Log", ["mel_guarded"], ["log_mel_tnf"]),
    ]
    # log_mel_tnf shape: [B, n_frames, n_mels]

    # ── Transpose to NeMo layout [B, n_mels, n_frames]
    nodes.append(
        helper.make_node("Transpose", ["log_mel_tnf"], ["log_mel"], perm=[0, 2, 1])
    )

    # ── Per-feature normalization (mean/unbiased-std across time axis = 2)
    nodes += [
        helper.make_node(
            "ReduceMean", ["log_mel"], ["mel_mean"], axes=[2], keepdims=1
        ),
        helper.make_node("Sub", ["log_mel", "mel_mean"], ["mel_centred"]),
        helper.make_node("Mul", ["mel_centred", "mel_centred"], ["mel_sq"]),
        helper.make_node(
            "ReduceSum", ["mel_sq", "axis_t"], ["mel_sq_sum"], keepdims=1
        ),
        # divide by (T - 1) for unbiased variance (matches torch.std default)
        helper.make_node("Shape", ["log_mel"], ["mel_shape"]),
        helper.make_node(
            "Gather", ["mel_shape", "two_int"], ["t_dim"], axis=0
        ),
        helper.make_node("Cast", ["t_dim"], ["t_f"], to=TensorProto.FLOAT),
        helper.make_node("Sub", ["t_f", "one_f"], ["t_minus_1"]),
        helper.make_node("Div", ["mel_sq_sum", "t_minus_1"], ["mel_var"]),
        helper.make_node("Sqrt", ["mel_var"], ["mel_std_raw"]),
        helper.make_node("Add", ["mel_std_raw", "norm_eps"], ["mel_std"]),
        helper.make_node("Div", ["mel_centred", "mel_std"], ["features_unpad"]),
    ]
    inits += [
        numpy_helper.from_array(np.array([2], dtype=np.int64), "axis_t"),
        numpy_helper.from_array(np.array(2, dtype=np.int64), "two_int"),
        numpy_helper.from_array(np.array([1.0], dtype=np.float32), "one_f"),
    ]

    # ── pad_to=16: pad time dim of features to next multiple of 16.
    # Compute pad_n = (-T) mod 16 ; if 0 → no-op. We pad with 0 (already
    # normalised so 0 is the mean, which matches NeMo's behaviour where
    # the pad value passes through normalisation as ~0).
    nodes += [
        helper.make_node("Shape", ["features_unpad"], ["feat_shape"]),
        helper.make_node("Gather", ["feat_shape", "two_int"], ["t_now"], axis=0),
        helper.make_node("Mod", ["t_now", "pad_to_const"], ["t_mod"]),
        helper.make_node("Sub", ["pad_to_const", "t_mod"], ["pad_diff"]),
        helper.make_node("Mod", ["pad_diff", "pad_to_const"], ["pad_extra"]),
        # pad_extra is already 1-D shape [1] (broadcast of [1] inputs); use directly.
        helper.make_node(
            "Concat",
            ["zeros_pad_3", "zeros_pad_2", "pad_extra"],
            ["pads_features"],
            axis=0,
        ),
        helper.make_node("Pad", ["features_unpad", "pads_features"], ["features"]),
        # length output: t_now is scalar; unsqueeze to [1] then cast to int64.
        helper.make_node("Unsqueeze", ["t_now", "zeros_int"], ["feature_length_scalar"]),
        helper.make_node("Cast", ["feature_length_scalar"], ["feature_length"], to=TensorProto.INT64),
    ]
    inits += [
        numpy_helper.from_array(np.array([pad_to], dtype=np.int64), "pad_to_const"),
        numpy_helper.from_array(np.zeros(3, dtype=np.int64), "zeros_pad_3"),
        numpy_helper.from_array(np.zeros(2, dtype=np.int64), "zeros_pad_2"),
    ]

    # ── Build graph.
    graph = helper.make_graph(
        nodes=nodes,
        name="nemo_mel_preprocessor",
        inputs=[
            helper.make_tensor_value_info("signal", TensorProto.FLOAT, [1, "N"]),
            helper.make_tensor_value_info("length", TensorProto.INT64, [1]),
        ],
        outputs=[
            helper.make_tensor_value_info("features", TensorProto.FLOAT, [1, n_mels, "T"]),
            helper.make_tensor_value_info("feature_length", TensorProto.INT64, [1]),
        ],
        initializer=inits,
    )

    opset = [helper.make_opsetid("", 17)]
    model = helper.make_model(graph, opset_imports=opset, producer_name="voice-pipes-mel")
    model.ir_version = 8
    onnx.checker.check_model(model)

    out_path.parent.mkdir(parents=True, exist_ok=True)
    onnx.save(model, str(out_path))
    print(f"[mel-onnx] wrote {out_path}  ({out_path.stat().st_size:,} bytes)")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, required=True, help="output mel.onnx path")
    parser.add_argument("--sample-rate", type=int, default=DEFAULT_SR)
    parser.add_argument("--n-fft", type=int, default=DEFAULT_N_FFT)
    parser.add_argument("--win-length", type=int, default=DEFAULT_WIN)
    parser.add_argument("--hop-length", type=int, default=DEFAULT_HOP)
    parser.add_argument("--n-mels", type=int, default=DEFAULT_N_MELS)
    parser.add_argument("--fmin", type=float, default=0.0)
    parser.add_argument("--fmax", type=float, default=None)
    parser.add_argument("--preemph", type=float, default=DEFAULT_PREEMPH)
    parser.add_argument("--pad-to", type=int, default=16)
    args = parser.parse_args()

    build_mel_onnx(
        out_path=args.out,
        sample_rate=args.sample_rate,
        n_fft=args.n_fft,
        win_length=args.win_length,
        hop_length=args.hop_length,
        n_mels=args.n_mels,
        fmin=args.fmin,
        fmax=args.fmax,
        preemph=args.preemph,
        pad_to=args.pad_to,
    )


if __name__ == "__main__":
    main()
