"""Input audio quality — backend slice.

Pins: `project_snapshot_metrics` reads the `input_*` keys from the `realtime`
component (S2S) and the `stt` component (cascade); `_flatten_turn` exposes them
as `row["input_audio"]` with the `input_` prefix stripped, never on agent turns
or captures without the keys; the report Notes explain them only when present.
No anomalies yet — thresholds arrive with a later calibration pass.

Run:: python -m unittest app.test_input_audio
"""

import unittest

from .captures.reader import INPUT_AUDIO_KEYS, project_snapshot_metrics
from .report.analysis import _detect_anomalies, _flatten_turn, _resolve_targets
from .report.render import _build_report_for_llm, _render_report_markdown
from .test_s2s_e2e import _realtime_snapshot

GEMINI = {"s2s": True, "llm": {"provider": "gemini_live", "model": "gemini-2.5-flash-native-audio"}}

INPUT_EXTRAS = {
    "input_speech_dbfs": -22.4,
    "input_peak_dbfs": -6.1,
    "input_noise_dbfs": -58.0,
    "input_snr_db": 35.6,
    "input_clipped_ratio": 0.0,
    "input_speech_ratio": 0.72,
    "input_speech_ms": 1180,
    "input_level_source": "vp_energy",
}


def _cascade_snapshot(uid: int, stt_extras: dict, initiator: str | None = None) -> dict:
    snap = {
        "utterance_id": uid,
        "total_ms": 1500,
        "components": [
            {"kind": "stt", "extras": {"endpointing_ms": 300, **stt_extras}},
            {"kind": "tts", "extras": {"ttfb_ms": 400, "synthesis_ms": 500}},
        ],
    }
    if initiator:
        snap["initiator"] = initiator
    return snap


class ProjectionTests(unittest.TestCase):
    def test_keys_mirror_the_rust_meter(self):
        self.assertEqual(len(INPUT_AUDIO_KEYS), 8)
        self.assertTrue(all(k.startswith("input_") for k in INPUT_AUDIO_KEYS))

    def test_realtime_component(self):
        proj = project_snapshot_metrics(_realtime_snapshot(2, {"ttfb_ms": 900, **INPUT_EXTRAS}))
        for key, value in INPUT_EXTRAS.items():
            self.assertEqual(proj[key], value, key)
        self.assertIsInstance(proj["input_speech_ms"], float)

    def test_stt_component(self):
        proj = project_snapshot_metrics(_cascade_snapshot(2, INPUT_EXTRAS))
        self.assertEqual(proj["input_snr_db"], 35.6)
        self.assertEqual(proj["input_level_source"], "vp_energy")

    def test_absent_keys_are_none(self):
        proj = project_snapshot_metrics(_realtime_snapshot(2, {"ttfb_ms": 900}))
        for key in INPUT_AUDIO_KEYS:
            self.assertIsNone(proj[key], key)
        self.assertIsNone(project_snapshot_metrics(None)["input_speech_dbfs"])

    def test_noise_keys_optional(self):
        extras = {k: v for k, v in INPUT_EXTRAS.items() if k not in ("input_noise_dbfs", "input_snr_db")}
        proj = project_snapshot_metrics(_realtime_snapshot(2, extras))
        self.assertEqual(proj["input_speech_dbfs"], -22.4)
        self.assertIsNone(proj["input_noise_dbfs"])
        self.assertIsNone(proj["input_snr_db"])

    def test_non_string_source_dropped(self):
        proj = project_snapshot_metrics(_realtime_snapshot(2, {**INPUT_EXTRAS, "input_level_source": 3}))
        self.assertIsNone(proj["input_level_source"])
        self.assertEqual(proj["input_speech_dbfs"], -22.4)


class RowTests(unittest.TestCase):
    def test_s2s_row_block(self):
        row = _flatten_turn(
            {"utterance_id": 2, "summary": {}, "metric_snapshot": _realtime_snapshot(2, {"ttfb_ms": 900, **INPUT_EXTRAS})},
            None, 1, GEMINI,
        )
        self.assertEqual(row["input_audio"], {
            "speech_dbfs": -22.4,
            "peak_dbfs": -6.1,
            "noise_dbfs": -58.0,
            "snr_db": 35.6,
            "clipped_ratio": 0.0,
            "speech_ratio": 0.72,
            "speech_ms": 1180.0,
            "level_source": "vp_energy",
        })

    def test_cascade_row_block(self):
        row = _flatten_turn({"utterance_id": 2, "summary": {}, "metric_snapshot": _cascade_snapshot(2, INPUT_EXTRAS)},
                            None, 1, {})
        self.assertEqual(row["input_audio"]["speech_dbfs"], -22.4)

    def test_absent_without_keys_and_on_agent_turns(self):
        bare = _flatten_turn({"utterance_id": 2, "summary": {}, "metric_snapshot": _cascade_snapshot(2, {})},
                             None, 1, {})
        self.assertNotIn("input_audio", bare)
        agent = _flatten_turn(
            {"utterance_id": 3, "summary": {}, "metric_snapshot": _cascade_snapshot(3, INPUT_EXTRAS, "agent")},
            None, 1, {},
        )
        self.assertNotIn("input_audio", agent)

    def test_not_judged_yet(self):
        # Terrible input must not raise anything until thresholds are set.
        awful = {**INPUT_EXTRAS, "input_speech_dbfs": -60.0, "input_snr_db": 2.0, "input_clipped_ratio": 0.3}
        row = _flatten_turn(
            {"utterance_id": 2, "summary": {}, "metric_snapshot": _realtime_snapshot(2, {"ttfb_ms": 300, **awful})},
            None, 1, GEMINI,
        )
        kinds = {a["kind"] for a in _detect_anomalies([row], _resolve_targets({}, None))}
        self.assertFalse(any(k.startswith("input.") for k in kinds), kinds)


class NotesTests(unittest.TestCase):
    def _markdown(self, extras: dict) -> str:
        report = _build_report_for_llm(
            session_id="t", metadata={},
            utterances=[{"utterance_id": 2, "summary": {}, "metric_snapshot": _realtime_snapshot(2, extras)}],
            session_metrics=None, providers=GEMINI, targets=_resolve_targets({}, None),
            events=None, health_model="v2", generated_at="2026-09-11T00:00:00Z",
        )
        return _render_report_markdown(report)

    def test_bullet_only_when_a_turn_has_input_audio(self):
        md = self._markdown({"ttfb_ms": 900, "audio_output_ms": 2000, **INPUT_EXTRAS})
        self.assertIn("`turns[].input_audio`", md)
        self.assertIn("\"speech_dbfs\": -22.4", md)
        md = self._markdown({"ttfb_ms": 900, "audio_output_ms": 2000})
        self.assertNotIn("input_audio", md)


if __name__ == "__main__":
    unittest.main()
