"""Tests for metric plausibility rules (`measurement.*`) and unusable-TTFT attribution (P1).

Covers:
- P1.1: `measurement.zero_with_work` when `ttft_ms == 0` alongside LLM work
  (`prompt_ingest_ms`, `generate_ms`, `generation_ms`, or `completion_tokens` > 0);
  verdict culprit is `llm`, never `tts`.
- P1.2: When TTFT is unusable, commit-anchored wait is attributed using
  `llm_detail` (`prompt_ingest_ms` / `perceived_ttft_ms`) when present, else left
  unattributed — never charged to TTS by subtracting 0.
- P1.3: `measurement.breakdown_exceeds_stage` for `tts.internal_ttfb_ms > tts.ttfb_ms`
  and `llm.ttft_ms > tts.ttfb_ms` on commit-anchored turns.
- P1.7: Shared `resolve_turn_ttft` helper used by `turns.py` and `routers/sessions.py`.
"""

import unittest

from .report.analysis import (
    _component_aggregates,
    _compute_verdict,
    _detect_anomalies,
    critical_path_latency,
)
from .report.turns import resolve_turn_ttft

TARGETS = {
    "ttft_ms": 300.0,
    "ttfb_ms": 400.0,
    "endpointing_ms": 500.0,
    "stt_processing_ms": 800.0,
    "draining_handshake_ms": 1500.0,
    "rtf_max": 0.6,
    "tokens_per_sec_min": 20.0,
    "barge_in_rtt_ms": 400.0,
    "self_barge_played_ms_max": 200.0,
    "outlier_total_ms_factor": 2.0,
}


def _session_73d30ef8_turns() -> list[dict]:
    """The three turns from incident session 73d30ef8 (SmolLM2-360M GGUF + Piper)."""
    rows = [
        {
            "utt": 1,
            "initiator": "user",
            "interrupted": False,
            "terminated": False,
            "total_ms": 2227.0,
            "stt": {
                "mode": "batch",
                "endpointing_ms": 1039.0,
                "processing_ms": 1575.0,
                "rtfx": 9.78,
                "cold_start": True,
            },
            "llm": {
                "ttft_ms": 0.0,
                "perceived_ttft_ms": 2026.0,
                "prompt_ingest_ms": 1379.5,
                "first_token_ms": 0.5,
                "generate_ms": 638.5,
                "generation_ms": 2026.0,
                "prompt_tokens": 32,
                "completion_tokens": 12,
                "tokens_per_sec": 18.8,
                "cold_start": True,
            },
            "tts": {
                "ttfb_ms": 2221.0,
                "internal_ttfb_ms": 192.0,
                "synthesis_ms": 198.0,
                "audio_duration_ms": 2438.0,
                "rtf": 0.08,
                "chars": 48,
                "cold_start": True,
            },
        },
        {
            "utt": 2,
            "initiator": "user",
            "interrupted": False,
            "terminated": False,
            "total_ms": 3758.0,
            "stt": {
                "mode": "batch",
                "endpointing_ms": 1048.0,
                "processing_ms": 1668.0,
                "rtfx": 10.62,
                "cold_start": None,
            },
            "llm": {
                "ttft_ms": 0.0,
                "perceived_ttft_ms": 3183.0,
                "prompt_ingest_ms": 2656.6,
                "first_token_ms": 0.4,
                "generate_ms": 912.5,
                "generation_ms": 3570.0,
                "prompt_tokens": 60,
                "completion_tokens": 17,
                "tokens_per_sec": 18.6,
                "cold_start": None,
            },
            "tts": {
                "ttfb_ms": 3389.0,
                "internal_ttfb_ms": 205.0,
                "synthesis_ms": 574.0,
                "audio_duration_ms": 5468.0,
                "rtf": 0.11,
                "chars": 100,
                "cold_start": None,
            },
        },
        {
            "utt": 3,
            "initiator": "user",
            "interrupted": False,
            "terminated": False,
            "total_ms": 5539.0,
            "stt": {
                "mode": "batch",
                "endpointing_ms": 1064.0,
                "processing_ms": 2256.0,
                "rtfx": 7.48,
                "cold_start": None,
            },
            "llm": {
                "ttft_ms": 0.0,
                "perceived_ttft_ms": 4867.0,
                "prompt_ingest_ms": 4262.5,
                "first_token_ms": 0.4,
                "generate_ms": 1103.0,
                "generation_ms": 5366.0,
                "prompt_tokens": 94,
                "completion_tokens": 17,
                "tokens_per_sec": 15.4,
                "cold_start": None,
            },
            "tts": {
                "ttfb_ms": 5066.0,
                "internal_ttfb_ms": 199.0,
                "synthesis_ms": 671.0,
                "audio_duration_ms": 3900.0,
                "rtf": 0.17,
                "chars": 66,
                "cold_start": None,
            },
        },
    ]
    for r in rows:
        r["critical_path"] = critical_path_latency(r)
    return rows


class MeasurementIntegrityTests(unittest.TestCase):
    def test_zero_ttft_with_llm_work_is_a_measurement_fault_not_tts(self):
        turns = _session_73d30ef8_turns()
        # P1.2: dominant stage on every turn is the LLM stage, never TTS.
        for t in turns:
            cp = t["critical_path"]
            self.assertEqual(cp["dominant_stage"], "llm_ttft_ms")
            self.assertLess(cp["tts_ttfb_ms"], 300.0)

        anoms = _detect_anomalies(turns, TARGETS)
        kinds = [a["kind"] for a in anoms]
        self.assertIn("measurement.zero_with_work", kinds)
        # TTS must not have an on-critical-path ttfb_high breach when Piper spent ~200 ms.
        tts_on_path = [
            a for a in anoms
            if a["kind"] == "tts.ttfb_high" and a.get("on_critical_path") is not False
        ]
        self.assertEqual(tts_on_path, [])

        aggs = _component_aggregates(turns)
        providers = {
            "stt": {"provider": "parakeet-tdt", "mode": "batch"},
            "llm": {"provider": "local", "model": "SmolLM2-360M-Instruct-Q4_K_M"},
            "tts": {"provider": "piper", "voice": "en_US-lessac-medium"},
        }
        v2 = _compute_verdict(turns, aggs, anoms, providers, model="v2")
        self.assertNotEqual(v2["primary_culprit"], "tts")
        self.assertEqual(v2["primary_culprit"], "llm")
        self.assertIn("measurement.zero_with_work", v2["supporting_anomaly_kinds"])

        v1 = _compute_verdict(turns, aggs, anoms, providers, model="v1")
        self.assertNotEqual(v1["primary_culprit"], "tts")
        self.assertEqual(v1["primary_culprit"], "llm")

    def test_unusable_ttft_without_breakdown_leaves_wait_unattributed_not_tts(self):
        turn = {
            "utt": 2,
            "stt": {"mode": "streaming", "endpointing_ms": 200.0},
            "llm": {"ttft_ms": 0.0, "generation_ms": 2800.0, "completion_tokens": 25},
            "tts": {"ttfb_ms": 3000.0, "internal_ttfb_ms": 180.0, "synthesis_ms": 300.0},
        }
        cp = critical_path_latency(turn)
        # Never subtract 0 and charge 3000 ms to TTS!
        self.assertEqual(cp["tts_ttfb_ms"], 180.0)
        self.assertNotEqual(cp["dominant_stage"], "tts_ttfb_ms")
        self.assertIsNone(cp["dominant_stage"])

    def test_breakdown_exceeds_stage_tts_internal_gt_ttfb(self):
        turn = {
            "utt": 2,
            "stt": {"mode": "streaming", "endpointing_ms": 150.0},
            "llm": {"ttft_ms": 200.0, "generation_ms": 600.0},
            "tts": {"ttfb_ms": 250.0, "internal_ttfb_ms": 420.0, "synthesis_ms": 500.0},
        }
        turn["critical_path"] = critical_path_latency(turn)
        anoms = _detect_anomalies([turn], TARGETS)
        kinds = [a["kind"] for a in anoms]
        self.assertIn("measurement.breakdown_exceeds_stage", kinds)

    def test_breakdown_exceeds_stage_llm_ttft_gt_commit_anchored_ttfb(self):
        turn = {
            "utt": 2,
            "stt": {"mode": "streaming", "endpointing_ms": 150.0},
            "llm": {"ttft_ms": 950.0, "generation_ms": 1400.0},
            "tts": {"ttfb_ms": 400.0, "internal_ttfb_ms": 180.0, "synthesis_ms": 500.0},
        }
        turn["critical_path"] = critical_path_latency(turn)
        anoms = _detect_anomalies([turn], TARGETS)
        kinds = [a["kind"] for a in anoms]
        self.assertIn("measurement.breakdown_exceeds_stage", kinds)

    def test_shared_resolve_turn_ttft(self):
        # Valid snapshot TTFT is returned directly.
        val, est = resolve_turn_ttft(240.0, {"first_token_ms": 230.0})
        self.assertEqual(val, 240.0)
        self.assertFalse(est)

        # Missing snapshot TTFT falls back to valid llm_detail.first_token_ms.
        val, est = resolve_turn_ttft(None, {"first_token_ms": 310.0, "prompt_ingest_ms": 200.0})
        self.assertEqual(val, 310.0)
        self.assertTrue(est)

        # Snapshot TTFT = 0 with LLM work is rejected; broken first_token_ms < prompt_ingest_ms is also rejected.
        val, est = resolve_turn_ttft(
            0.0,
            {"first_token_ms": 0.4, "prompt_ingest_ms": 2656.6, "generate_ms": 912.5},
            generation_ms=3570.0,
        )
        self.assertEqual(val, 0.0)
        self.assertFalse(est)

    def test_critical_path_uses_engine_owned_telescoping_stages(self):
        turn = {
            "utt": 2,
            "stt": {"mode": "streaming", "endpointing_ms": 220.0},
            "llm": {"ttft_ms": 310.0, "llm_wait_ms": 315.4, "generation_ms": 800.0},
            "tts": {
                "ttfb_ms": 180.0,
                "tts_wait_ms": 182.6,
                "output_wait_ms": 12.0,
                "commit_to_first_audio_ms": 510.0,
                "synthesis_ms": 400.0,
                "audio_duration_ms": 2100.0,
            },
        }
        cp = critical_path_latency(turn)
        self.assertEqual(cp["endpointing_ms"], 220.0)
        self.assertEqual(cp["llm_ttft_ms"], 315.4)
        self.assertEqual(cp["llm_wait_ms"], 315.4)
        self.assertEqual(cp["tts_ttfb_ms"], 182.6)
        self.assertEqual(cp["tts_wait_ms"], 182.6)
        self.assertEqual(cp["output_wait_ms"], 12.0)
        self.assertEqual(cp["commit_to_first_audio_ms"], 510.0)
        self.assertEqual(cp["total_ms"], 730.0)
        self.assertEqual(cp["dominant_stage"], "llm_ttft_ms")
        self.assertNotIn("stages_disagree_ms", cp)

    def test_stages_disagree_emits_measurement_anomaly(self):
        turn = {
            "utt": 2,
            "stt": {"mode": "streaming", "endpointing_ms": 200.0},
            "llm": {"ttft_ms": 250.0, "llm_wait_ms": 250.0, "generation_ms": 600.0},
            "tts": {
                "ttfb_ms": 150.0,
                "tts_wait_ms": 150.0,
                "output_wait_ms": 10.0,
                "commit_to_first_audio_ms": 600.0,
                "synthesis_ms": 300.0,
                "audio_duration_ms": 1800.0,
            },
        }
        cp = critical_path_latency(turn)
        turn["critical_path"] = cp
        self.assertEqual(cp["total_ms"], 800.0)
        self.assertEqual(cp["stages_disagree_ms"], -190.0)

        anoms = _detect_anomalies([turn], TARGETS)
        kinds = [a["kind"] for a in anoms]
        self.assertIn("measurement.stages_disagree", kinds)

    def test_tool_context_reanchors_commit_to_first_audio_without_corrupting_tts_ttfb(self):
        from .report.turns import _apply_tool_context

        flat_turns = [
            {
                "utt": 1,
                "agent_transcript": "",
                "stt": {"mode": "streaming", "endpointing_ms": 200.0},
                "llm": {},
                "tts": {"audio_duration_ms": 0},
            },
            {
                "utt": 2,
                "agent_transcript": "Playing your track now.",
                "stt": {"mode": "streaming", "endpointing_ms": 150.0},
                "llm": {"ttft_ms": 220.0, "llm_wait_ms": 1220.0, "generation_ms": 500.0},
                "tts": {
                    "ttfb_ms": 180.0,
                    "tts_wait_ms": 180.0,
                    "output_wait_ms": 10.0,
                    "commit_to_first_audio_ms": 1410.0,
                    "synthesis_ms": 300.0,
                    "audio_duration_ms": 1900.0,
                },
            },
        ]
        utterances = [
            {"utterance_id": 1},
            {"utterance_id": 2, "_wallclock_ts_ms": {"first_audio_out_ms": 2410.0}},
        ]
        tool_calls = [
            {
                "call_id": "c1",
                "name": "play_media",
                "status": "completed",
                "after_utterance_id": 1,
                "called_at_ms": 1000.0,
                "resolved_at_ms": 2000.0,
                "speak_on_result": True,
            }
        ]
        _apply_tool_context(flat_turns, utterances, tool_calls)
        followup = flat_turns[1]
        self.assertEqual(followup["kind"], "tool_followup")
        # TTS-own ttfb_ms is untouched (180 ms), while commit_to_first_audio_ms
        # and llm_wait_ms are re-anchored past the 1000 ms tool wait.
        self.assertEqual(followup["tts"]["ttfb_ms"], 180.0)
        self.assertEqual(followup["tts"]["commit_to_first_audio_pre_tool_ms"], 1410.0)
        self.assertEqual(followup["tts"]["commit_to_first_audio_ms"], 410.0)
        self.assertEqual(followup["llm"]["llm_wait_pre_tool_ms"], 1220.0)
        self.assertEqual(followup["llm"]["llm_wait_ms"], 220.0)
        self.assertEqual(followup["critical_path"]["total_ms"], 560.0)


if __name__ == "__main__":
    unittest.main()

