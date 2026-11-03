"""
Mel parity test: compare our Rust log-mel against a librosa reference using
NeMo `AudioToMelSpectrogramPreprocessor` defaults.

Workflow:
  1. Build the Rust dump tool:
       cargo build --release -p voice-pipes-stt-nemotron --example dump_mel_features
  2. Run it on a 16 kHz mono WAV:
       cargo run --release -p voice-pipes-stt-nemotron --example dump_mel_features -- \
           debug-dumps/utterance.wav debug-dumps/utterance.melbin
  3. Run this script:
       python scripts/mel_parity.py debug-dumps/utterance.wav debug-dumps/utterance.melbin

It will print:
  - shape, min/max/mean of each
  - max absolute and relative diff
  - which mel bin and time frame have the largest divergence
  - a small sample of values around the divergence

NeMo `AudioToMelSpectrogramPreprocessor` defaults used here (verified against
NeMo source `nemo/collections/asr/parts/preprocessing/features.py`):

  sample_rate=16000
  n_fft=512
  win_length=400 (= 25 ms @ 16 kHz)
  hop_length=160 (= 10 ms @ 16 kHz)
  n_mels=128
  fmin=0, fmax=sample_rate/2 (i.e. 8000)
  window=hann (periodic=False, i.e. symmetric)
  preemph=0.97   (applied as y[n] = x[n] - 0.97*x[n-1], y[0]=x[0])
  mag_power=2.0  (power spectrum)
  mel_norm='slaney'  (librosa norm='slaney')
  log_zero_guard='add', value=2**-24
  normalize='per_feature'  (per mel bin, mean/std across time, unbiased std + 1e-5)
  pad_to=16  (pad time dim to multiple of 16, with zero AFTER normalization)
  center=True, pad_mode='reflect'
  dither=0  (deterministic inference path; NeMo default at inference is also 0)
"""

import struct
import sys
from pathlib import Path

import numpy as np
import librosa
import scipy.signal

PREEMPH = 0.97
LOG_GUARD = 2.0 ** -24
NORM_EPS = 1e-5
PAD_TO = 16


def load_mono_16k(path: Path) -> np.ndarray:
    audio, sr = librosa.load(str(path), sr=16000, mono=True)
    return audio.astype(np.float32), sr


def reference_log_mel(
    waveform: np.ndarray,
    sample_rate: int = 16000,
    n_fft: int = 512,
    win_length: int = 400,
    hop_length: int = 160,
    n_mels: int = 128,
    fmin: float = 0.0,
    fmax: float = 8000.0,
    normalize: str = "per_feature",  # 'per_feature' or 'none' (model_config.yaml says normalize=NA)
    pad_to: int = 16,                  # set 0 to disable (model_config.yaml says pad_to=0)
    center: bool = True,               # NeMo FilterbankFeatures default is center=True
) -> np.ndarray:
    """Return mel of shape (n_mels, n_frames) matching NeMo defaults."""
    if waveform.size == 0:
        return np.zeros((n_mels, 0), dtype=np.float32)

    # 1) Pre-emphasis: y[0]=x[0]; y[n]=x[n]-0.97*x[n-1]
    y = np.empty_like(waveform)
    y[0] = waveform[0]
    y[1:] = waveform[1:] - PREEMPH * waveform[:-1]

    # 2) Window: torch.hann_window(win_length, periodic=False) — symmetric.
    #    librosa hann() is symmetric by default ('sym=True' equivalent).
    window = scipy.signal.windows.hann(win_length, sym=True).astype(np.float32)

    # 3) Optionally pad with reflect to length (n_fft//2) on each side, then
    #    STFT with the window padded to n_fft (centered inside the n_fft window).
    if center:
        pad = n_fft // 2
        y_padded = np.pad(y, (pad, pad), mode="reflect")
    else:
        y_padded = y

    padded_window = np.zeros(n_fft, dtype=np.float32)
    win_offset = (n_fft - win_length) // 2
    padded_window[win_offset : win_offset + win_length] = window

    n_frames = 0 if y_padded.size < n_fft else 1 + (y_padded.size - n_fft) // hop_length
    if n_frames == 0:
        return np.zeros((n_mels, 0), dtype=np.float32)

    # Manual framing to mirror our Rust implementation exactly.
    frames = np.lib.stride_tricks.as_strided(
        y_padded,
        shape=(n_frames, n_fft),
        strides=(y_padded.strides[0] * hop_length, y_padded.strides[0]),
    ).copy()
    frames *= padded_window  # broadcast win across frames

    spec = np.fft.rfft(frames, n=n_fft, axis=1)  # (n_frames, n_fft//2+1)
    power = (spec.real ** 2 + spec.imag ** 2).astype(np.float32)

    # 4) Slaney mel filterbank (librosa default norm='slaney')
    mel_fb = librosa.filters.mel(
        sr=sample_rate,
        n_fft=n_fft,
        n_mels=n_mels,
        fmin=fmin,
        fmax=fmax,
        norm="slaney",
        htk=False,
    ).astype(np.float32)  # (n_mels, n_freqs)

    mel = mel_fb @ power.T  # (n_mels, n_frames)

    # 5) log(x + 2^-24)
    mel = np.log(mel + LOG_GUARD)

    # 6) Per-feature normalization (per mel bin, across time, unbiased std)
    if normalize == "per_feature" and n_frames > 1:
        mean = mel.mean(axis=1, keepdims=True)
        var = ((mel - mean) ** 2).sum(axis=1, keepdims=True) / (n_frames - 1)
        std = np.sqrt(var) + NORM_EPS
        mel = (mel - mean) / std

    # 7) pad_to with zero AFTER normalization
    if pad_to and pad_to > 0:
        pad_frames = ((n_frames + pad_to - 1) // pad_to) * pad_to - n_frames
        if pad_frames > 0:
            mel = np.pad(mel, ((0, 0), (0, pad_frames)), mode="constant", constant_values=0.0)

    return mel.astype(np.float32)


def load_rust_dump(path: Path) -> tuple[np.ndarray, dict]:
    with open(path, "rb") as f:
        n_mels = struct.unpack("<I", f.read(4))[0]
        n_frames = struct.unpack("<I", f.read(4))[0]
        sr = struct.unpack("<I", f.read(4))[0]
        n_fft = struct.unpack("<I", f.read(4))[0]
        win_length = struct.unpack("<I", f.read(4))[0]
        hop_length = struct.unpack("<I", f.read(4))[0]
        data = np.frombuffer(f.read(n_mels * n_frames * 4), dtype="<f4").reshape(
            n_mels, n_frames
        )
    meta = dict(
        sample_rate=sr,
        n_fft=n_fft,
        win_length=win_length,
        hop_length=hop_length,
        n_mels=n_mels,
        n_frames=n_frames,
    )
    return data.copy(), meta


def stats(name: str, a: np.ndarray) -> None:
    print(
        f"  {name:<10} shape={a.shape}  min={a.min():+.4f}  max={a.max():+.4f}  "
        f"mean={a.mean():+.4f}  std={a.std():.4f}"
    )


def main() -> int:
    if len(sys.argv) != 3:
        print("usage: python mel_parity.py <input.wav> <rust_dump.melbin>")
        return 2
    wav_path = Path(sys.argv[1])
    bin_path = Path(sys.argv[2])

    print(f"loading WAV: {wav_path}")
    audio, sr = load_mono_16k(wav_path)
    print(f"  samples={audio.size}  sr={sr}")

    print("computing librosa reference (NeMo defaults)")
    # Variant A: previous reference (per_feature norm + pad_to=16) — what our native does
    ref_normed = reference_log_mel(audio, normalize="per_feature", pad_to=16, center=True)
    # Variant B: NeMo-trained-true (model_config.yaml: normalize=NA, pad_to=0)
    ref_raw_center = reference_log_mel(audio, normalize="none", pad_to=0, center=True)
    # Variant C: same as B but center=False (just in case the export used that)
    ref_raw_nocenter = reference_log_mel(audio, normalize="none", pad_to=0, center=False)

    print(f"loading rust dump: {bin_path}")
    ours, meta = load_rust_dump(bin_path)
    print(f"  meta={meta}")

    print("\n=== shape comparison ===")
    print(f"  ref_normed       : {ref_normed.shape}")
    print(f"  ref_raw_center   : {ref_raw_center.shape}")
    print(f"  ref_raw_nocenter : {ref_raw_nocenter.shape}")
    print(f"  ours             : {ours.shape}")

    print("\n=== feature stats ===")
    stats("ref_normed", ref_normed)
    stats("ref_raw_C",  ref_raw_center)
    stats("ref_raw_NC", ref_raw_nocenter)
    stats("ours",       ours)

    def compare(name: str, ref: np.ndarray, ours: np.ndarray) -> None:
        n_mels = min(ref.shape[0], ours.shape[0])
        n_frames = min(ref.shape[1], ours.shape[1])
        r = ref[:n_mels, :n_frames]
        o = ours[:n_mels, :n_frames]
        d = np.abs(o - r)
        print(f"  vs {name:<14} ({r.shape[0]}x{r.shape[1]}):  "
              f"max|d|={d.max():.5f}  mean|d|={d.mean():.5f}  RMSE={np.sqrt(((o-r)**2).mean()):.5f}")

    print("\n=== divergence (ours = the file you passed) ===")
    compare("normed+pad",   ref_normed,        ours)
    compare("raw center",   ref_raw_center,    ours)
    compare("raw no-center", ref_raw_nocenter, ours)

    # Also print which one is closest, to give a clear verdict
    deltas = {
        "normed+pad (current code path)": np.abs(ours[:, :ref_normed.shape[1]] - ref_normed[:, :ours.shape[1]]).max() if ref_normed.shape == ours.shape else float("inf"),
        "raw + center (model_config.yaml-true)": np.abs(ours[:, :ref_raw_center.shape[1]] - ref_raw_center[:, :ours.shape[1]]).max() if ref_raw_center.shape == ours.shape else float("inf"),
        "raw + no-center": np.abs(ours[:, :ref_raw_nocenter.shape[1]] - ref_raw_nocenter[:, :ours.shape[1]]).max() if ref_raw_nocenter.shape == ours.shape else float("inf"),
    }
    print("\n=== closeness ranking ===")
    for k, v in sorted(deltas.items(), key=lambda kv: kv[1]):
        print(f"  {k:<48s} max|diff| = {v:.5f}")

    return 0


if __name__ == "__main__":
    sys.exit(main())
