#!/usr/bin/env python3
"""Regenerate the synthetic probe fixtures.

The fixtures are the known inputs the component probes and the regression
tests run against: short utterances at 16 kHz mono, each paired with the text
that was spoken. Synthesising them with the repo's own Piper voice keeps them
regenerable and free of licensing questions, and means a fixture can be added
by editing one table here rather than by recording anything.

Two fixtures are deliberately NOT synthesised and are committed as captures:

  room-tone-only  needs real room noise -- a synthesiser emits digital silence,
                  which no recogniser hallucinates against, so a synthetic
                  version would pass a test that the real one fails.
  hello-gated     a real capture whose speech was gated to a 0.55 s burst by
                  the browser's automatic gain. Nothing reproduces that.

Usage (from the voice-pipes directory, after `cargo build --release`):

    python fixtures/build-fixtures.py

Writes each WAV plus `manifest.json`, which is what the probes and tests read.
"""

from __future__ import annotations

import array
import json
import math
import pathlib
import shutil
import subprocess
import sys
import tempfile
import wave

HERE = pathlib.Path(__file__).resolve().parent
REPO_VP = HERE.parent                 # voice-pipes/
TARGET_RATE = 16_000

# name -> (text spoken, note explaining what the fixture is for)
SPOKEN: dict[str, tuple[str, str]] = {
    "hello-how-are-you": (
        "Hello, how are you?",
        "The regression that prompted the probes: the leading word must survive "
        "segmentation. A transcript starting at 'o,' means the pre-roll is too short.",
    ),
    "short-yes": (
        "Yes.",
        "An utterance shorter than most silence timeouts; must still commit as a turn.",
    ),
    "short-about-three": (
        "About three.",
        "A short answer with a number, the shape most likely to be clipped to nothing.",
    ),
    "mid-sentence-pause": (
        "I was thinking that we could, maybe, try the other one.",
        "Contains pauses a speaker makes while thinking. A fixed silence timeout ends "
        "the turn early here; that is the failure this fixture exists to show.",
    ),
}

# Spoken quietly, to sit near the VAD's floor without going under it.
QUIET = (
    "quiet-speech",
    "Can you hear me now?",
    "Near the detection floor. Over-aggressive gating drops this one first, which is "
    "what a caller sitting back from the microphone sounds like.",
)
QUIET_GAIN = 0.12


def synth(text: str, out: pathlib.Path) -> None:
    """Synthesise `text` with the repo's Piper voice."""
    cfg = tempfile.NamedTemporaryFile("w", suffix=".toml", delete=False)
    cfg.write(
        '[pipeline]\nname = "fixture-build"\n\n'
        '[tts]\nprovider = "piper"\nmodel = "models/en_US-lessac-medium.onnx"\n'
    )
    cfg.close()
    exe = REPO_VP.parent / "target" / "release" / "voice-pipes"
    if not exe.exists() and not exe.with_suffix(".exe").exists():
        sys.exit(f"build the release binary first: {exe} not found")
    subprocess.run(
        [str(exe), "--config", cfg.name, "tts", "--text", text, "--output", str(out)],
        cwd=REPO_VP, check=True, capture_output=True,
    )
    pathlib.Path(cfg.name).unlink(missing_ok=True)


def read(path: pathlib.Path) -> tuple[array.array, int]:
    with wave.open(str(path), "rb") as w:
        rate = w.getframerate()
        data = array.array("h")
        data.frombytes(w.readframes(w.getnframes()))
        if w.getnchannels() == 2:
            data = array.array("h", data[0::2])
    return data, rate


def write(path: pathlib.Path, samples: array.array, rate: int = TARGET_RATE) -> None:
    with wave.open(str(path), "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(rate)
        w.writeframes(samples.tobytes())


def resample(src: array.array, src_rate: int, dst_rate: int) -> array.array:
    """Rate conversion with a low-pass ahead of the decimation.

    Piper runs at 22050 Hz and the fixtures are 16 kHz, so this downsamples.
    Skipping the filter would alias, and a fixture that aliases would bake the
    exact fault these probes exist to detect into the reference data.
    """
    if src_rate == dst_rate:
        return src
    if dst_rate < src_rate:
        src = biquad_lowpass(src, src_rate, fc=dst_rate * 0.45)
    ratio = src_rate / dst_rate
    n = int(len(src) / ratio)
    out = array.array("h")
    for i in range(n):
        x = i * ratio
        j = int(x)
        frac = x - j
        a = src[j]
        b = src[min(j + 1, len(src) - 1)]
        out.append(int(a + (b - a) * frac))
    return out


def biquad_lowpass(d: array.array, rate: int, fc: float, q: float = 0.707) -> array.array:
    w0 = 2 * math.pi * fc / rate
    alpha = math.sin(w0) / (2 * q)
    c = math.cos(w0)
    b0, b1, b2 = (1 - c) / 2, 1 - c, (1 - c) / 2
    a0, a1, a2 = 1 + alpha, -2 * c, 1 - alpha
    b0, b1, b2, a1, a2 = b0 / a0, b1 / a0, b2 / a0, a1 / a0, a2 / a0
    x1 = x2 = y1 = y2 = 0.0
    out = array.array("h")
    for s in d:
        x = float(s)
        y = b0 * x + b1 * x1 + b2 * x2 - a1 * y1 - a2 * y2
        x2, x1 = x1, x
        y2, y1 = y1, y
        out.append(max(-32768, min(32767, int(y))))
    return out


def pad(d: array.array, rate: int, lead_ms: int = 400, tail_ms: int = 400) -> array.array:
    """Silence either side.

    Every fixture needs room before the speech, because a probe that starts at
    the first sample cannot show whether the pre-roll would have kept anything:
    there is nothing before the onset to keep.
    """
    lead = array.array("h", [0] * int(rate * lead_ms / 1000))
    tail = array.array("h", [0] * int(rate * tail_ms / 1000))
    return lead + d + tail


def measure(d: array.array, rate: int) -> dict:
    n = len(d)
    peak = max(max(d), -min(d)) / 32768 if n else 0.0
    rms = (sum(float(s) * s for s in d) / n) ** 0.5 / 32768 if n else 0.0
    return {
        "duration_ms": round(n / rate * 1000),
        "peak": round(peak, 4),
        "rms": round(rms, 5),
    }


def main() -> None:
    entries: list[dict] = []
    tmp = pathlib.Path(tempfile.mkdtemp())

    for name, (text, note) in SPOKEN.items():
        raw = tmp / f"{name}-raw.wav"
        synth(text, raw)
        d, rate = read(raw)
        d = pad(resample(d, rate, TARGET_RATE), TARGET_RATE)
        out = HERE / f"{name}.wav"
        write(out, d)
        entries.append({"name": name, "file": out.name, "text": text,
                        "note": note, **measure(d, TARGET_RATE)})
        print(f"  {out.name}")

    # Quiet speech: the same synthesis, attenuated.
    name, text, note = QUIET
    raw = tmp / f"{name}-raw.wav"
    synth(text, raw)
    d, rate = read(raw)
    d = resample(d, rate, TARGET_RATE)
    d = array.array("h", [int(s * QUIET_GAIN) for s in d])
    d = pad(d, TARGET_RATE)
    out = HERE / f"{name}.wav"
    write(out, d)
    entries.append({"name": name, "file": out.name, "text": text,
                    "note": note, **measure(d, TARGET_RATE)})
    print(f"  {out.name}")

    # Committed captures, described here but never regenerated.
    for name, text, note in [
        ("room-tone-only", "",
         "Real room noise and no speech. Must yield NO utterance and an empty "
         "transcript: Parakeet returned 'a little bit of the three years ago' from "
         "two seconds of near-silence during the investigation that prompted this."),
        ("hello-gated", "Hello, how are you?",
         "A real capture of the phrase after the browser's automatic gain gated it to "
         "a 0.55 s burst. Shows what capture damage looks like downstream; no "
         "synthesiser reproduces it."),
    ]:
        f = HERE / f"{name}.wav"
        if not f.exists():
            print(f"  {f.name} MISSING — capture it; see the module docstring")
            continue
        d, rate = read(f)
        entries.append({"name": name, "file": f.name, "text": text, "note": note,
                        "synthetic": False, **measure(d, rate)})
        print(f"  {f.name} (committed capture)")

    for e in entries:
        e.setdefault("synthetic", True)
    manifest = HERE / "manifest.json"
    manifest.write_text(json.dumps({"sample_rate": TARGET_RATE, "fixtures": entries},
                                   indent=2) + "\n", encoding="utf-8")
    print(f"\nwrote {manifest.name} with {len(entries)} fixtures")
    shutil.rmtree(tmp, ignore_errors=True)


if __name__ == "__main__":
    main()
