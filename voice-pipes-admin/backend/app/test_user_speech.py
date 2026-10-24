"""Speech-region trimming of user mic captures (`utt.user_speech` and the
`/audio/turn/{id}/user_speech.wav` route).

Run:: python -m unittest backend.app.test_user_speech
"""

import array
import asyncio
import io
import json
import math
import unittest
import wave

from fastapi import HTTPException

from .captures import reader
from .captures.speech_bounds import speech_bounds_from_samples
from .routers import audio as audio_router
from .test_playback_health import _CapturesTest

RATE = 16000


def _clip(silence_before_ms: int, speech_ms: int, silence_after_ms: int, amplitude: int = 6000) -> array.array:
    """Silence, a 220 Hz tone (speech stand-in), silence — with faint hiss so
    the noise floor is realistic rather than digital zero."""
    out = array.array("h")
    def hiss(n):
        return [((i * 7919) % 41) - 20 for i in range(n)]
    out.extend(hiss(RATE * silence_before_ms // 1000))
    n = RATE * speech_ms // 1000
    out.extend(int(amplitude * math.sin(2 * math.pi * 220 * i / RATE)) for i in range(n))
    out.extend(hiss(RATE * silence_after_ms // 1000))
    return out


def _write(path, samples: array.array) -> None:
    with wave.open(str(path), "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(RATE)
        w.writeframes(samples.tobytes())


class SpeechBoundsTest(unittest.TestCase):
    def test_finds_the_speech_region_with_padding(self) -> None:
        b = speech_bounds_from_samples(_clip(3000, 600, 15000), RATE)
        assert b is not None
        # 200 ms pre-pad / 300 ms post-pad around the 3000–3600 ms tone,
        # quantised to 20 ms windows.
        self.assertAlmostEqual(b.start_ms, 2800, delta=40)
        self.assertAlmostEqual(b.end_ms, 3900, delta=40)

    def test_silence_only_is_none(self) -> None:
        self.assertIsNone(speech_bounds_from_samples(_clip(2000, 0, 2000), RATE))

    def test_pads_are_clamped_to_the_clip(self) -> None:
        b = speech_bounds_from_samples(_clip(50, 400, 50), RATE)
        assert b is not None
        self.assertEqual(b.start_ms, 0)
        self.assertEqual(b.end_ms, 500)


class UserSpeechReaderTest(_CapturesTest):
    def test_reader_attaches_bounds_or_none(self) -> None:
        (self.sdir / "001_stt.json").write_text(json.dumps({"text": "hi", "latency_ms": 1}), encoding="utf-8")
        _write(self.sdir / "001_mic_raw.wav", _clip(3000, 600, 15000))
        (self.sdir / "002_stt.json").write_text(json.dumps({"text": "typed", "latency_ms": 1}), encoding="utf-8")
        _write(self.sdir / "002_mic_raw.wav", _clip(4000, 0, 0))

        utts = {u["utterance_id"]: u for u in reader._read_capture_artifacts(self.session)["utterances"]}
        spoken = utts[1]["user_speech"]
        self.assertEqual(spoken["kind"], "mic_raw")
        self.assertAlmostEqual(spoken["start_ms"], 2800, delta=40)
        self.assertAlmostEqual(spoken["end_ms"], 3900, delta=40)
        self.assertIsNone(utts[2]["user_speech"])


class UserSpeechRouteTest(_CapturesTest):
    def test_route_returns_only_the_speech_region(self) -> None:
        _write(self.sdir / "001_mic_raw.wav", _clip(3000, 600, 15000))
        resp = asyncio.run(audio_router.get_turn_user_speech(self.session, 1))
        with wave.open(io.BytesIO(resp.body), "rb") as r:
            self.assertEqual(r.getframerate(), RATE)
            # ~1.1 s (600 ms tone + 500 ms of padding), not 18.6 s.
            self.assertAlmostEqual(r.getnframes() / RATE, 1.1, delta=0.05)

    def test_route_404s_when_the_clip_has_no_speech(self) -> None:
        _write(self.sdir / "001_mic_raw.wav", _clip(4000, 0, 0))
        with self.assertRaises(HTTPException) as ctx:
            asyncio.run(audio_router.get_turn_user_speech(self.session, 1))
        self.assertEqual(ctx.exception.status_code, 404)

    def test_route_404s_without_a_mic_capture(self) -> None:
        with self.assertRaises(HTTPException):
            asyncio.run(audio_router.get_turn_user_speech(self.session, 1))
