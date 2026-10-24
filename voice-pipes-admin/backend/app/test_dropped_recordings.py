"""The recordings kept for speech that made no turn are listed on the
session, paired with the `utterance_dropped` events, so the console can
play what the recogniser heard nothing in. Before this a session the
recogniser never answered looked empty in the console even with its audio
on disk, because the capture reader's filename pattern skipped the files.

Run:: python -m unittest backend.app.test_dropped_recordings
"""

import json

from .captures import reader
from .test_playback_health import _CapturesTest
from .test_user_speech import _clip, _write


def _dropped_event(utterance_id: int, audio_ms: int, secs: int) -> str:
    return json.dumps({
        "kind": "utterance_dropped",
        "utterance_id": utterance_id,
        "reason": "empty_transcript",
        "text": "",
        "had_interim_text": False,
        "audio_ms": audio_ms,
        "timestamp": {"secs_since_epoch": secs, "nanos_since_epoch": 0},
    })


class DroppedRecordingsTest(_CapturesTest):
    def test_each_drop_under_an_id_is_paired_with_its_recording_in_order(self) -> None:
        # Session 491bfc0f: two sounds dropped under id 2, then the turn.
        _write(self.sdir / "002_dropped_empty_transcript_mic_raw.wav", _clip(200, 400, 800))
        _write(self.sdir / "002_dropped_empty_transcript_2_mic_raw.wav", _clip(200, 600, 800))
        _write(self.sdir / "002_mic_raw.wav", _clip(200, 500, 900))
        (self.sdir / "002_stt.json").write_text(json.dumps({"text": "it is good", "latency_ms": 1}), encoding="utf-8")
        (self.sdir / "pipeline_events.jsonl").write_text(
            _dropped_event(2, 1377, 100) + "\n" + _dropped_event(2, 1597, 108) + "\n", encoding="utf-8"
        )

        utts = {u["utterance_id"]: u for u in reader._read_capture_artifacts(self.session)["utterances"]}
        dropped = utts[2]["dropped_before"]
        self.assertEqual([d["file"] for d in dropped], [
            "002_dropped_empty_transcript_mic_raw.wav",
            "002_dropped_empty_transcript_2_mic_raw.wav",
        ])
        self.assertEqual([d["audio_ms"] for d in dropped], [1377, 1597])
        self.assertAlmostEqual(dropped[0]["duration_ms"], 1400, delta=5)
        self.assertAlmostEqual(dropped[1]["duration_ms"], 1600, delta=5)
        # The turn's own files are untouched by the dropped ones.
        self.assertEqual(utts[2]["audio_files"], {"mic_raw": "002_mic_raw.wav"})
        self.assertNotIn("dropped_recordings", utts[2])

    def test_a_recording_without_its_event_is_still_listed_from_its_name(self) -> None:
        # A session with no turns at all: only the recording survives.
        _write(self.sdir / "001_dropped_stt_timeout_mic_raw.wav", _clip(100, 300, 100))

        utts = reader._read_capture_artifacts(self.session)["utterances"]
        self.assertEqual(len(utts), 1)
        d = utts[0]["dropped_before"][0]
        self.assertEqual(d["reason"], "stt_timeout")
        self.assertEqual(d["file"], "001_dropped_stt_timeout_mic_raw.wav")
        self.assertFalse(d["had_interim_text"])
        self.assertAlmostEqual(d["audio_ms"], 500, delta=5)
        self.assertIsNone(d["at_ms"])

    def test_a_session_without_drops_has_no_dropped_fields(self) -> None:
        _write(self.sdir / "001_mic_raw.wav", _clip(200, 500, 900))
        utts = reader._read_capture_artifacts(self.session)["utterances"]
        self.assertNotIn("dropped_before", utts[0])
        self.assertNotIn("dropped_recordings", utts[0])
