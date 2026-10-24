"""Locate the speech region inside a captured mic clip.

Realtime (S2S) sessions keep the microphone open for the whole call, so an
utterance's ``NNN_mic_raw.wav`` spans everything between two turn boundaries —
including the entire time the agent was speaking. Measured on a real session:
19.5 s of clip for 0.5 s of speech; a typed turn produced 59 s of pure
silence. Played back-to-back with the agent audio, that silence is what made
the Conversation Playback "pauses" feel far longer than the real call.

This module finds the first and last window that carries speech energy so the
playback can play just that region. It is an energy gate, not a VAD: it is
meant to cut leading and trailing silence, not to segment speech. The noise
floor is taken from the clip itself (20th percentile of window RMS) so a
noisy mic still trims; an absolute floor keeps a silent clip from promoting
its own hiss to "speech".
"""

from __future__ import annotations

import array
import math
import wave
from dataclasses import dataclass
from pathlib import Path

WINDOW_MS = 20
PRE_PAD_MS = 200
POST_PAD_MS = 300
#: RMS (int16 scale) below which a window is never speech, whatever the clip's
#: floor. Speech on a normal mic peaks in the thousands.
ABS_FLOOR = 150.0
#: Speech must clear the clip's own noise floor by this factor...
FLOOR_RATIO = 4.0
#: ...and be at least this fraction of the clip's loudest window.
PEAK_RATIO = 0.06


@dataclass(frozen=True)
class SpeechBounds:
    start_ms: int
    end_ms: int

    @property
    def duration_ms(self) -> int:
        return self.end_ms - self.start_ms


def _window_rms(samples: array.array, window: int) -> list[float]:
    out: list[float] = []
    n = len(samples)
    for i in range(0, n - window + 1, window):
        acc = 0
        for j in range(i, i + window):
            v = samples[j]
            acc += v * v
        out.append(math.sqrt(acc / window))
    return out


def speech_bounds_from_samples(samples: array.array, rate: int) -> SpeechBounds | None:
    """Bounds of the speech region in mono int16 ``samples``, or ``None`` when
    the clip carries no speech energy at all."""
    window = max(1, rate * WINDOW_MS // 1000)
    if len(samples) < window:
        return None
    rms = _window_rms(samples, window)
    if not rms:
        return None
    ordered = sorted(rms)
    floor = ordered[len(ordered) // 5]
    peak = ordered[-1]
    # The floor estimate assumes the clip is mostly silence (true for S2S
    # captures). When speech fills the clip the 20th percentile *is* speech,
    # so never let the floor-derived threshold climb past half the peak.
    threshold = max(ABS_FLOOR, min(floor * FLOOR_RATIO, peak * 0.5), peak * PEAK_RATIO)
    speech = [i for i, v in enumerate(rms) if v > threshold]
    if not speech:
        return None
    total_ms = len(samples) * 1000 // rate
    start_ms = max(0, speech[0] * WINDOW_MS - PRE_PAD_MS)
    end_ms = min(total_ms, (speech[-1] + 1) * WINDOW_MS + POST_PAD_MS)
    return SpeechBounds(start_ms=start_ms, end_ms=end_ms)


def read_mono_int16(path: Path) -> tuple[array.array, int] | None:
    """Load a 16-bit WAV as mono samples (first channel), with its rate."""
    try:
        with wave.open(str(path), "rb") as w:
            if w.getsampwidth() != 2:
                return None
            nch = w.getnchannels()
            rate = w.getframerate()
            raw = w.readframes(w.getnframes())
    except (wave.Error, EOFError, OSError):
        return None
    samples = array.array("h")
    samples.frombytes(raw)
    if nch > 1:
        samples = samples[::nch]
    return samples, rate


def speech_bounds(path: Path) -> SpeechBounds | None:
    loaded = read_mono_int16(path)
    if loaded is None:
        return None
    samples, rate = loaded
    return speech_bounds_from_samples(samples, rate)
