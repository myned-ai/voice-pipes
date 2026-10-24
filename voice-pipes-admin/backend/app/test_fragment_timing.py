"""Per-fragment timing on session-detail utterances (`utt.fragments`).

The fragment timeline in the admin UI lays fragments out by these offsets
and seeks `tts_concat.wav` to them, so they must match how that track is
concatenated: cumulative durations, in numeric fragment order.

Run:: python -m unittest backend.app.test_fragment_timing
"""

import json

from .captures import reader
from .test_playback_health import _CapturesTest, _write_frag


class FragmentTimingTest(_CapturesTest):
    def _seed_transcript(self) -> None:
        (self.sdir / "001_stt.json").write_text(
            json.dumps({"text": "hello", "latency_ms": 100}), encoding="utf-8"
        )

    def test_offsets_are_cumulative_in_numeric_fragment_order(self) -> None:
        self._seed_transcript()
        _write_frag(self.sdir / "001_tts_frag_000.wav", 8000)  # 500 ms
        _write_frag(self.sdir / "001_tts_frag_001.wav", 4000)  # 250 ms
        # Index 10 sorts after 1 numerically but before it lexically ("010" < "1"
        # is false, but "tts_frag_010" vs "tts_frag_001" would mis-order under
        # a naive string sort of the *kind*). Guards the numeric key.
        _write_frag(self.sdir / "001_tts_frag_010.wav", 1600)  # 100 ms

        utt = reader._read_capture_artifacts(self.session)["utterances"][0]
        self.assertEqual(
            utt["fragments"],
            [
                {"kind": "tts_frag_000", "offset_ms": 0.0, "duration_ms": 500.0},
                {"kind": "tts_frag_001", "offset_ms": 500.0, "duration_ms": 250.0},
                {"kind": "tts_frag_010", "offset_ms": 750.0, "duration_ms": 100.0},
            ],
        )

    def test_turn_without_tts_fragments_has_no_fragments_key(self) -> None:
        self._seed_transcript()
        _write_frag(self.sdir / "001_mic_raw.wav", 800)

        utt = reader._read_capture_artifacts(self.session)["utterances"][0]
        self.assertNotIn("fragments", utt)
        self.assertIn("mic_raw", utt.get("audio_files", {}))
