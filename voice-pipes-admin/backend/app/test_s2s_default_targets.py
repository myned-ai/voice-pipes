"""Speech-to-speech sessions are graded against cloud-S2S targets by default.

A healthy Gemini Live session (warm TTFB p50 ~1.25 s, the provider noticing
the end of speech ~1.1-1.3 s late, bounded by the 1 s silence timeout) graded
`degraded`: the engine's generic realtime descriptor budget (ttfb 600 ms) and
the cascade default endpointing (500 ms) flagged seven of its turns. The
fixture below is that session's turn profile. Only pipelines with a hand-written `[grading.targets]` escaped it.
The report now resolves ttfb_ms and endpointing_ms to 1200 ms for an S2S
session; the console's ADMIN_GRADING_* and the pipeline's `[grading]` still
win over that.
"""

import json
import os
import unittest
from pathlib import Path
from tempfile import TemporaryDirectory
from unittest import mock

from .captures.reader import _read_capture_artifacts
from .core import config
from .report.analysis import _extract_provider_identity, resolve_grading
from .report.render import _build_report_for_llm
from .test_s2s_e2e import T0_MS, _ts

# The engine's realtime descriptor as a capture records it: ttfb budget 600.
REALTIME_DESCRIPTOR = {
    "component_id": "realtime", "kind": "realtime", "label": "Realtime S2S",
    "fields": [
        {"key": "ttfb_ms", "label": "Time to first audio", "unit": "ms",
         "health": {"role": "latency", "budget": 600.0, "on_critical_path": True}},
        {"key": "provider_endpointing_ms", "label": "Provider endpointing", "unit": "ms",
         "health": {"role": "latency", "budget": 500.0, "on_critical_path": True}},
    ],
}

# The turns of a healthy Gemini Live session as captured on 2026-10-05 (it
# graded `degraded`): (ttfb_ms, anchor, provider_endpointing_ms, interrupted,
# audio_output_ms). Turn 1 is the cold start; warm TTFB p50 ~1.25 s.
VP, PROXY = "vp_vad", "input_transcript_proxy"
TURNS = [
    (2113, VP, None, False, 2210),
    (1285, VP, None, False, 6410),
    (1258, VP, None, True, 7049),
    (1250, VP, None, False, 4260),
    (1174, VP, None, True, 6469),
    (1122, VP, None, False, 5920),
    (1097, VP, None, True, 5149),
    (510, PROXY, 1128, False, 5640),
    (568, PROXY, 1275, True, 6059),
    (1204, VP, None, False, 10020),
    (1485, VP, None, False, 8720),
]


def _snapshot(uid: int, ttfb: float, anchor: str, endpt: float | None,
              interrupted: bool, audio_ms: float) -> dict:
    vp_end = T0_MS + uid * 20_000
    extras = {
        "ttfb_ms": ttfb,
        "ttfb_anchor_source": anchor,
        "vp_speech_end_epoch_ms": vp_end,
        "user_speech_end_epoch_ms": vp_end + (endpt or 0),
        "first_text_ms": ttfb,
        "audio_output_ms": audio_ms,
        "input_audio_ms": 6000,
        "sample_rate": 16000,
    }
    if endpt is not None:
        extras["provider_endpointing_ms"] = endpt
    return {
        "utterance_id": uid,
        "total_ms": audio_ms + 4,
        "interrupted": interrupted,
        "components": [{
            "component_id": "realtime",
            "kind": "realtime",
            "provider": "gemini-live",
            "latency_ms": audio_ms + 4,
            "started_at_ms": 0,
            "extras": extras,
        }],
    }


class S2sDefaultTargetsTest(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        old = config.CAPTURES_DIR
        config.CAPTURES_DIR = Path(self._tmp.name)
        self.addCleanup(setattr, config, "CAPTURES_DIR", old)
        env = mock.patch.dict(os.environ, {}, clear=False)
        env.start()
        self.addCleanup(env.stop)
        for k in ("ADMIN_GRADING_TARGETS", "ADMIN_GRADING_LEVELS"):
            os.environ.pop(k, None)

    def _write(self, sid: str, grading: dict | None = None) -> dict:
        d = Path(self._tmp.name) / sid
        d.mkdir()
        # As the worker records a custom realtime pipeline: provider names
        # unknown, one model id for all three stages.
        model = {"id": "gemini-3.8-live", "source": "config"}
        meta = {
            "id": sid,
            "pipeline_name": "custom-realtime",
            "start_time": "2026-10-05T09:13:05Z",
            "stt_provider": "unknown",
            "tts_provider": "unknown",
            "llm_model": model,
            "stt_model": model,
            "tts_model": model,
            "metric_descriptors": [REALTIME_DESCRIPTOR],
            "turn_detection": {"mode": "silence", "silence_timeout_ms": 1000},
            "grading": grading,
        }
        (d / "metadata.json").write_text(json.dumps(meta), encoding="utf-8")
        pe = []
        for uid, (ttfb, anchor, endpt, interrupted, audio_ms) in enumerate(TURNS, start=1):
            if uid > 1:
                pe.append({"kind": "utterance_received", "utterance_id": uid, "source": "realtime",
                           "text": "tell me something",
                           "timestamp": _ts(T0_MS + uid * 20_000 + (endpt or 300))})
            pe.append({"kind": "turn_metrics_ready", "utterance_id": uid,
                       "metrics": _snapshot(uid, ttfb, anchor, endpt, interrupted, audio_ms)})
            (d / f"{uid:03d}_turn_resolved.json").write_text(json.dumps({
                "utterance_id": uid, "interrupted": interrupted, "terminated": False,
                "stt_provider": "gemini-live", "tts_provider": "gemini-live",
            }), encoding="utf-8")
        (d / "pipeline_events.jsonl").write_text(
            "".join(json.dumps(e) + "\n" for e in pe), encoding="utf-8")
        (d / "events.jsonl").write_text("", encoding="utf-8")
        return meta

    def _grade(self, sid: str, meta: dict) -> tuple[dict, dict]:
        arts = _read_capture_artifacts(sid, events_offset=0, events_limit=5000)
        utts = arts["utterances"]
        providers = _extract_provider_identity(utts, meta)
        self.assertTrue(providers.get("s2s"), providers)
        grading = resolve_grading(providers, meta)
        report = _build_report_for_llm(
            session_id=sid, metadata=meta, utterances=utts,
            session_metrics=arts["session_metrics"], providers=providers,
            targets=grading["targets"], events=None, health_model="v2",
            grading=grading, generated_at="2026-10-05T00:00:00Z",
        )
        return grading, report

    def test_healthy_gemini_session_grades_fair_or_better_without_grading(self) -> None:
        meta = self._write("gemini-healthy")
        grading, report = self._grade("gemini-healthy", meta)
        self.assertEqual(grading["targets"]["ttfb_ms"], 1200)
        self.assertEqual(grading["targets"]["endpointing_ms"], 1200)
        # Defaults, not operator rules: nothing to disclose.
        self.assertEqual(grading["custom"], [])
        self.assertIn(report["verdict"]["level"], ("healthy", "fair"), report["verdict"])

    def test_pipeline_grading_still_wins(self) -> None:
        meta = self._write("gemini-strict", grading={"targets": {"ttfb_ms": 800}})
        grading, report = self._grade("gemini-strict", meta)
        self.assertEqual(grading["targets"]["ttfb_ms"], 800)
        self.assertEqual(grading["targets"]["endpointing_ms"], 1200)
        self.assertEqual(
            [(c["name"], c["value"], c["default"], c["source"]) for c in grading["custom"]],
            [("ttfb_ms", 800, 1200, "pipeline config")],
        )
        kinds = {a["kind"] for a in report["anomalies"]}
        self.assertIn("tts.ttfb_high", kinds)

    def test_console_rules_still_win(self) -> None:
        meta = self._write("gemini-console")
        with mock.patch.dict(os.environ, {"ADMIN_GRADING_TARGETS": "endpointing_ms:900"}):
            grading, _ = self._grade("gemini-console", meta)
        self.assertEqual(grading["targets"]["endpointing_ms"], 900)
        self.assertEqual(grading["targets"]["ttfb_ms"], 1200)

    def test_cascade_session_keeps_the_cascade_defaults(self) -> None:
        grading = resolve_grading({"llm": {"provider": "ollama"}, "stt": {}, "tts": {}}, {})
        self.assertEqual(grading["targets"]["ttfb_ms"], 400)
        self.assertEqual(grading["targets"]["endpointing_ms"], 500)


if __name__ == "__main__":
    unittest.main()
