"""Tests for playback-fidelity auditing.

Covers the as-heard reconstruction route and the capture reader projecting
`NNN_playback.json` onto turn summaries and the session rollup.

Run:: python -m unittest backend.app.test_playback_health
"""

import asyncio
import io
import json
import unittest
import wave
from pathlib import Path
from tempfile import TemporaryDirectory

from .captures import reader
from .core import config
from .routers import audio as audio_router

RATE = 16000
SAMPWIDTH = 2


def _write_frag(path: Path, n_frames: int, value: int = 1000) -> None:
    with wave.open(str(path), "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(SAMPWIDTH)
        w.setframerate(RATE)
        w.writeframes(value.to_bytes(2, "little", signed=True) * n_frames)


def _read_wav(body: bytes):
    with wave.open(io.BytesIO(body), "rb") as r:
        return r.getframerate(), r.getnframes(), r.readframes(r.getnframes())


class _CapturesTest(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = TemporaryDirectory()
        self.caps = Path(self._tmp.name) / "captures"
        self.caps.mkdir()
        self._old_caps = config.CAPTURES_DIR
        config.CAPTURES_DIR = self.caps
        self.session = "test-session-1"
        self.sdir = self.caps / self.session
        self.sdir.mkdir()

    def tearDown(self) -> None:
        config.CAPTURES_DIR = self._old_caps
        self._tmp.cleanup()


class AsHeardRouteTest(_CapturesTest):
    def _seed_frags(self) -> None:
        # Two fragments: 8000 + 4000 frames = 500 ms + 250 ms of ideal audio.
        _write_frag(self.sdir / "001_tts_frag_000.wav", 8000)
        _write_frag(self.sdir / "001_tts_frag_001.wav", 4000)

    def test_splices_reported_silences_into_ideal_audio(self) -> None:
        self._seed_frags()
        (self.sdir / "001_playback.json").write_text(
            json.dumps(
                {
                    "underrun_count": 1,
                    "total_silence_ms": 250,
                    "audio_ms": 750,
                    "heard_ms": 750,
                    "wall_ms": 1000,
                    "gaps": [{"at_audio_ms": 500, "silence_ms": 250}],
                }
            ),
            encoding="utf-8",
        )
        resp = asyncio.run(audio_router.get_turn_as_heard(self.session, 1))
        rate, n_frames, frames = _read_wav(resp.body)
        self.assertEqual(rate, RATE)
        # 750 ms ideal + 250 ms silence = 1000 ms = 16000 frames.
        self.assertEqual(n_frames, 16000)
        # The spliced region [500ms, 750ms) must be all zero bytes...
        gap_start = round(0.5 * RATE) * SAMPWIDTH
        gap_end = gap_start + round(0.25 * RATE) * SAMPWIDTH
        self.assertEqual(frames[gap_start:gap_end], b"\x00" * (gap_end - gap_start))
        # ...and the audio around it must be untouched (non-zero).
        self.assertNotEqual(frames[gap_start - 2 : gap_start], b"\x00\x00")
        self.assertNotEqual(frames[gap_end : gap_end + 2], b"\x00\x00")

    def test_without_playback_json_equals_tts_concat(self) -> None:
        self._seed_frags()
        as_heard = asyncio.run(audio_router.get_turn_as_heard(self.session, 1))
        concat = asyncio.run(audio_router.get_turn_tts_concat(self.session, 1))
        self.assertEqual(_read_wav(as_heard.body), _read_wav(concat.body))

    def test_interrupted_turn_clamps_to_heard_ms(self) -> None:
        self._seed_frags()
        (self.sdir / "001_playback.json").write_text(
            json.dumps(
                {
                    "underrun_count": 0,
                    "total_silence_ms": 0,
                    "audio_ms": 750,
                    "heard_ms": 500,
                    "wall_ms": 500,
                    "gaps": [],
                }
            ),
            encoding="utf-8",
        )
        (self.sdir / "001_turn_resolved.json").write_text(
            json.dumps({"utterance_id": 1, "interrupted": True}), encoding="utf-8"
        )
        resp = asyncio.run(audio_router.get_turn_as_heard(self.session, 1))
        _, n_frames, _ = _read_wav(resp.body)
        # Clamped to heard_ms=500ms → 8000 frames; no audio past the barge-in.
        self.assertEqual(n_frames, 8000)


class ReaderProjectionTest(_CapturesTest):
    def test_playback_json_projects_onto_summary_and_rollup(self) -> None:
        _write_frag(self.sdir / "001_tts_frag_000.wav", 8000)
        # A transcript makes the turn non-empty (is_empty_turn) so it counts
        # toward the session rollup, as every real turn would.
        (self.sdir / "001_stt.json").write_text(
            json.dumps({"text": "hello", "latency_ms": 100}), encoding="utf-8"
        )
        (self.sdir / "001_playback.json").write_text(
            json.dumps(
                {
                    "underrun_count": 3,
                    "total_silence_ms": 900,
                    "audio_ms": 5000,
                    "heard_ms": 5000,
                    "wall_ms": 5900,
                    "gaps": [{"at_audio_ms": 480, "silence_ms": 300}],
                }
            ),
            encoding="utf-8",
        )
        result = reader._read_capture_artifacts(self.session)
        utts = result.get("utterances") or []
        self.assertEqual(len(utts), 1)
        utt = utts[0]
        self.assertEqual(utt["playback_health"]["underrun_count"], 3)
        summary = utt.get("summary") or {}
        self.assertEqual(summary.get("audio_underruns"), 3)
        self.assertEqual(summary.get("total_silence_ms"), 900)
        self.assertEqual(summary.get("wall_clock_ms"), 5900)
        self.assertEqual(summary.get("playback_gaps"), [{"at_audio_ms": 480, "silence_ms": 300}])

        metrics = reader._compute_session_metrics(utts, [])
        self.assertEqual(metrics["turnsWithUnderruns"], 1)
        self.assertEqual(metrics["totalUnderruns"], 3)
        self.assertEqual(metrics["totalSilenceMs"], 900)


if __name__ == "__main__":
    unittest.main()
