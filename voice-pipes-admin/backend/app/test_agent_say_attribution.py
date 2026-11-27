"""Regression tests for `agent_say` (skipped-LLM) telemetry attribution.

Verifies:
1. `agent_say` greeting turns (e.g. utt 10001) never steal `llm_detail` from
   subsequent user turns — preventing fake orchestration gaps on Turn 1,
   missing `llm_detail` on Turn 2, bogus `llm.ttft_high` on Turn 10001, and
   `stages_disagree_ms` on Turn 10001.
2. Both legacy captures (synthetic `llm_timing` on `agent_say`, no
   `utterance_id` on `llm_timing`) and new captures (`llm_timing` carries
   `utterance_id` and is skipped on `agent_say`) align accurately.
3. Engine-reported `tts.cold_start` is preserved on the greeting turn while
   subsequent warm user turns get `tts.cold_start = None` (and legacy captures
   without explicit `tts.cold_start` keep the `is_first_turn` fallback).
"""

import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from app.captures import reader as _reader
from app.report.analysis import _extract_provider_identity, resolve_grading
from app.report.render import _build_report_for_llm
from app.report.turns import _flatten_turn, critical_path_latency, has_explicit_tts_cold_start


def _write_jsonl(path: Path, rows: list[dict]) -> None:
    with open(path, "w", encoding="utf-8") as f:
        for row in rows:
            f.write(json.dumps(row) + "\n")


def _make_agent_say_capture(
    root: Path,
    session_id: str,
    *,
    legacy_agent_say_llm_timing: bool,
    explicit_tts_cold_start: bool = True,
) -> Path:
    sdir = root / session_id
    sdir.mkdir(parents=True, exist_ok=True)
    (sdir / "metadata.json").write_text(
        json.dumps(
            {
                "session_id": session_id,
                "pipeline_name": "custom-classic",
                "stt_provider": "moonshine",
                "tts_provider": "pocket-candle",
                "llm_provider": "openai-compat",
                "started_at": "2026-11-27T13:19:15.592Z",
                "ended_at": "2026-11-27T13:19:28.600Z",
                "status": "completed",
            }
        ),
        encoding="utf-8",
    )

    greeting_tts_comp: dict = {
        "kind": "tts",
        "provider": "pocket-candle",
        "extras": {
            "ttfb_ms": 2159,
            "tts_wait_ms": 2160.2,
            "output_wait_ms": 0.2,
            "commit_to_first_audio_ms": 2160.4,
            "synthesis_ms": 2796,
            "audio_duration_ms": 2240,
            "rtf": 1.25,
            "chars": 29,
        },
    }
    if explicit_tts_cold_start:
        greeting_tts_comp["cold_start"] = True

    pipeline_events = [
        {
            "kind": "llm_completed",
            "utterance_id": 10001,
            "text_len": 29,
            "elapsed_ms": 0,
            "is_partial": False,
        },
        {"kind": "tts_completed", "utterance_id": 10001},
        {
            "kind": "turn_metrics_ready",
            "utterance_id": 10001,
            "metrics": {
                "utterance_id": 10001,
                "initiator": "agent",
                "total_ms": 2798,
                "components": [
                    {
                        "kind": "llm",
                        "provider": "openai-compat",
                        "extras": {"stage_skipped_reason": "agent_say"},
                    },
                    greeting_tts_comp,
                    {
                        "kind": "output",
                        "extras": {
                            "emit_rtf": 0.28,
                            "source_rtf": 0.28,
                            "chain_overhead_ms": 0,
                            "audio_output_ms": 2240,
                        },
                    },
                ],
            },
        },
        {"kind": "utterance_received", "utterance_id": 1, "source": "Audio"},
        {
            "kind": "llm_completed",
            "utterance_id": 1,
            "text_len": 57,
            "elapsed_ms": 447,
            "is_partial": False,
        },
        {"kind": "tts_completed", "utterance_id": 1},
        {
            "kind": "turn_metrics_ready",
            "utterance_id": 1,
            "metrics": {
                "utterance_id": 1,
                "initiator": "user",
                "total_ms": 703,
                "components": [
                    {
                        "kind": "stt",
                        "provider": "moonshine",
                        "extras": {"endpointing_ms": 1556, "processing_ms": 1716},
                    },
                    {
                        "kind": "llm",
                        "provider": "openai-compat",
                        "extras": {
                            "ttft_ms": 315,
                            "llm_perceived_ttft_ms": 447,
                            "llm_wait_ms": 448.1,
                            "generation_ms": 447,
                        },
                    },
                    {
                        "kind": "tts",
                        "provider": "pocket-candle",
                        "extras": {
                            "ttfb_ms": 135,
                            "tts_wait_ms": 135.1,
                            "output_wait_ms": 0.0,
                            "commit_to_first_audio_ms": 583.2,
                            "audio_duration_ms": 350,
                            "rtf": 0.45,
                        },
                    },
                ],
            },
        },
        {"kind": "utterance_received", "utterance_id": 2, "source": "Audio"},
        {
            "kind": "llm_completed",
            "utterance_id": 2,
            "text_len": 34,
            "elapsed_ms": 224,
            "is_partial": False,
        },
        {"kind": "tts_completed", "utterance_id": 2},
        {
            "kind": "turn_metrics_ready",
            "utterance_id": 2,
            "metrics": {
                "utterance_id": 2,
                "initiator": "user",
                "total_ms": 755,
                "components": [
                    {
                        "kind": "stt",
                        "provider": "moonshine",
                        "extras": {"endpointing_ms": 1643, "processing_ms": 1924},
                    },
                    {
                        "kind": "llm",
                        "provider": "openai-compat",
                        "extras": {
                            "ttft_ms": 107,
                            "llm_perceived_ttft_ms": 155,
                            "llm_wait_ms": 156.1,
                            "generation_ms": 224,
                        },
                    },
                    {
                        "kind": "tts",
                        "provider": "pocket-candle",
                        "extras": {
                            "ttfb_ms": 87,
                            "tts_wait_ms": 87.3,
                            "output_wait_ms": 0.0,
                            "commit_to_first_audio_ms": 243.4,
                            "synthesis_ms": 898,
                            "audio_duration_ms": 2000,
                            "rtf": 0.45,
                        },
                    },
                ],
            },
        },
    ]
    _write_jsonl(sdir / "pipeline_events.jsonl", pipeline_events)

    events: list[dict] = []
    if legacy_agent_say_llm_timing:
        events.append(
            {
                "timestamp": "2026-11-27T13:19:15.691Z",
                "event_type": "llm_timing",
                "component": "llm:openai-compat",
                "payload": {"elapsed_ms": 0.1, "tokens": 7, "provider": "openai-compat"},
            }
        )
    events.extend(
        [
            {
                "timestamp": "2026-11-27T13:19:18.487Z",
                "event_type": "tts_timing",
                "component": "tts:pocket-candle",
                "payload": {
                    "elapsed_ms": 2796.4,
                    "audio_duration_s": 2.24,
                    "chars": 29,
                    "rtf": 1.25,
                    "provider": "pocket-candle",
                },
            },
            {
                "timestamp": "2026-11-27T13:19:20.489Z",
                "event_type": "stt_timing",
                "component": "stt:moonshine",
                "payload": {"elapsed_ms": 1716.4, "endpointing_ms": 1556.5, "chars": 3},
            },
            {
                "timestamp": "2026-11-27T13:19:20.937Z",
                "event_type": "llm_detail",
                "component": "llm:openai-compat",
                "payload": {
                    "connect_ms": 46.8,
                    "first_token_ms": 315.8,
                    "elapsed_ms": 447.5,
                    "first_sentence_chars": 57,
                    "model": "gemma-4-E2B-it",
                },
            },
            {
                "timestamp": "2026-11-27T13:19:20.937Z",
                "event_type": "llm_timing",
                "component": "llm:openai-compat",
                "payload": (
                    {"elapsed_ms": 447.8, "tokens": 11, "provider": "openai-compat"}
                    if legacy_agent_say_llm_timing
                    else {
                        "utterance_id": 1,
                        "elapsed_ms": 447.8,
                        "tokens": 11,
                        "provider": "openai-compat",
                    }
                ),
            },
            {
                "timestamp": "2026-11-27T13:19:23.116Z",
                "event_type": "stt_timing",
                "component": "stt:moonshine",
                "payload": {"elapsed_ms": 1924.4, "endpointing_ms": 1643.8, "chars": 6},
            },
            {
                "timestamp": "2026-11-27T13:19:23.341Z",
                "event_type": "llm_detail",
                "component": "llm:openai-compat",
                "payload": {
                    "connect_ms": 41.8,
                    "first_token_ms": 107.4,
                    "elapsed_ms": 224.5,
                    "first_sentence_chars": 12,
                    "model": "gemma-4-E2B-it",
                },
            },
            {
                "timestamp": "2026-11-27T13:19:23.341Z",
                "event_type": "llm_timing",
                "component": "llm:openai-compat",
                "payload": (
                    {"elapsed_ms": 224.7, "tokens": 7, "provider": "openai-compat"}
                    if legacy_agent_say_llm_timing
                    else {
                        "utterance_id": 2,
                        "elapsed_ms": 224.7,
                        "tokens": 7,
                        "provider": "openai-compat",
                    }
                ),
            },
            {
                "timestamp": "2026-11-27T13:19:23.872Z",
                "event_type": "tts_timing",
                "component": "tts:pocket-candle",
                "payload": {
                    "elapsed_ms": 898.2,
                    "audio_duration_s": 2.0,
                    "chars": 33,
                    "rtf": 0.45,
                    "provider": "pocket-candle",
                },
            },
        ]
    )
    _write_jsonl(sdir / "events.jsonl", events)
    return sdir


class TestAgentSayAttribution(unittest.TestCase):
    def _run_report(self, *, legacy_agent_say_llm_timing: bool, explicit_tts_cold_start: bool = True):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            sid = "sess-agent-say"
            sdir = _make_agent_say_capture(
                root,
                sid,
                legacy_agent_say_llm_timing=legacy_agent_say_llm_timing,
                explicit_tts_cold_start=explicit_tts_cold_start,
            )
            with patch.object(_reader, "_resolve_session_dir", lambda _sid, **_kw: sdir):
                meta = _reader._read_session_metadata(sid) or {}
                arts = _reader._read_capture_artifacts(sid)
            providers = _extract_provider_identity(arts["utterances"], meta)
            grading = resolve_grading(providers, meta)
            report = _build_report_for_llm(
                sid,
                meta,
                arts["utterances"],
                arts.get("session_metrics") or {},
                providers,
                grading["targets"],
                events=arts["events"],
                health_model="v2",
                grading=grading,
            )
            return arts, report

    def _assert_aligned(self, report: dict) -> None:
        by_utt = {t["utt"]: t for t in report["turns"]}
        t10001 = by_utt[10001]
        t1 = by_utt[1]
        t2 = by_utt[2]

        # Turn 10001 (agent_say): clean TTS metrics, no leaked LLM detail, no stages_disagree_ms.
        self.assertEqual(t10001["initiator"], "agent")
        self.assertEqual(t10001["llm"]["stage_skipped_reason"], "agent_say")
        self.assertIsNone(t10001["llm"]["ttft_ms"])
        self.assertIsNone(t10001["llm"]["connect_ms"])
        self.assertIsNone(t10001["llm"]["first_token_ms"])
        self.assertIsNone(t10001["llm"]["generation_ms"])
        self.assertIsNone(t10001["llm"]["cold_start"])
        self.assertIsNone(t10001["stt"]["cold_start"])
        self.assertTrue(t10001["tts"]["cold_start"])
        self.assertEqual(t10001["tts"]["ttfb_ms"], 2159.0)
        self.assertEqual(t10001["tts"]["synthesis_ms"], 2796.0)
        self.assertEqual(t10001["critical_path"]["llm_ttft_ms"], 0.0)
        self.assertNotIn("stages_disagree_ms", t10001["critical_path"])

        # Turn 1: gets its OWN llm_detail (315.8ms), eliminating the fake ~208ms orch gap.
        self.assertEqual(t1["llm"]["ttft_ms"], 315.0)
        self.assertEqual(t1["llm"]["connect_ms"], 46.8)
        self.assertEqual(t1["llm"]["first_token_ms"], 315.8)
        self.assertTrue(t1["stt"]["cold_start"])
        self.assertTrue(t1["llm"]["cold_start"])
        self.assertIsNone(t1["tts"]["cold_start"])

        # Turn 2: gets its OWN llm_detail (107.4ms) instead of None.
        self.assertEqual(t2["llm"]["ttft_ms"], 107.0)
        self.assertEqual(t2["llm"]["connect_ms"], 41.8)
        self.assertEqual(t2["llm"]["first_token_ms"], 107.4)
        self.assertIsNone(t2["tts"]["cold_start"])

        # No bogus llm.ttft_high anomaly on Turn 10001.
        llm_anoms_10001 = [
            a for a in report["anomalies"]
            if a.get("utt") == 10001 and (a.get("kind") or "").startswith("llm.")
        ]
        self.assertEqual(llm_anoms_10001, [])

    def test_legacy_capture_aligns_llm_detail_across_agent_say(self):
        _, report = self._run_report(legacy_agent_say_llm_timing=True)
        self._assert_aligned(report)

    def test_new_capture_with_utterance_id_on_llm_timing_aligns(self):
        arts, report = self._run_report(legacy_agent_say_llm_timing=False)
        self._assert_aligned(report)
        utts = {u["utterance_id"]: u for u in arts["utterances"]}
        self.assertNotIn("llm_timing", utts[10001])
        self.assertEqual(utts[1]["llm_timing"]["utterance_id"], 1)
        self.assertEqual(utts[2]["llm_timing"]["utterance_id"], 2)

    def test_legacy_capture_without_explicit_tts_cold_start_keeps_first_turn_fallback(self):
        arts, report = self._run_report(
            legacy_agent_say_llm_timing=True,
            explicit_tts_cold_start=False,
        )
        self.assertFalse(has_explicit_tts_cold_start(arts["utterances"]))
        by_utt = {t["utt"]: t for t in report["turns"]}
        # Fallback preserves existing behavior when snapshot lacks tts.cold_start.
        self.assertTrue(by_utt[1]["tts"]["cold_start"])
        self.assertIsNone(by_utt[2]["tts"]["cold_start"])

    def test_skipped_llm_defense_in_depth(self):
        # Even if a malformed utterance carries a stray llm_detail alongside
        # stage_skipped_reason="agent_say", _flatten_turn and critical_path_latency
        # must never fabricate LLM latency or stages_disagree_ms.
        stray_utt = {
            "utterance_id": 10001,
            "llm_detail": {"connect_ms": 46.8, "first_token_ms": 315.8, "elapsed_ms": 447.5},
            "metric_snapshot": {
                "utterance_id": 10001,
                "initiator": "agent",
                "total_ms": 2798,
                "components": [
                    {"kind": "llm", "extras": {"stage_skipped_reason": "agent_say"}},
                    {
                        "kind": "tts",
                        "cold_start": True,
                        "extras": {
                            "ttfb_ms": 2159,
                            "tts_wait_ms": 2160.2,
                            "output_wait_ms": 0.2,
                            "commit_to_first_audio_ms": 2160.4,
                        },
                    },
                ],
            },
        }
        flat = _flatten_turn(stray_utt, None, 1, {}, tts_cold_start_explicit=True)
        self.assertIsNone(flat["llm"]["ttft_ms"])
        self.assertIsNone(flat["llm"]["connect_ms"])
        self.assertIsNone(flat["llm"]["first_token_ms"])
        cp = critical_path_latency(flat)
        self.assertEqual(cp["llm_ttft_ms"], 0.0)
        self.assertNotIn("stages_disagree_ms", cp)

    def test_session_metrics_excludes_agent_turns_and_computes_p95_and_leading_silence(self):
        def _utt(uid: int, ttft: int, ttfb: int, lead: int, initiator: str = "user") -> dict:
            return {
                "utterance_id": uid,
                "summary": {},
                "metric_snapshot": {
                    "utterance_id": uid,
                    "initiator": initiator,
                    "total_ms": ttfb + 500,
                    "components": [
                        {"kind": "llm", "extras": {"ttft_ms": ttft}},
                        {
                            "kind": "tts",
                            "extras": {
                                "ttfb_ms": ttfb,
                                "tts_leading_silence_ms": lead,
                                "tts_audible_ttfb_ms": ttfb + lead,
                            },
                        },
                    ],
                },
            }

        utts = [
            _utt(1, 100, 200, 40),
            _utt(2, 120, 220, 60),
            _utt(3, 140, 240, 80),
            _utt(4, 500, 800, 100),
            # Agent-initiated turn with huge values must be excluded from user averages/p95.
            _utt(10001, 9000, 9000, 9000, initiator="agent"),
        ]
        m = _reader._compute_session_metrics(utts, [])
        self.assertEqual(m["avgTtftMs"], round((100 + 120 + 140 + 500) / 4))
        self.assertEqual(m["p95TtftMs"], 500)
        self.assertEqual(m["p95TtfbMs"], 800)
        self.assertEqual(m["avgLeadingSilenceMs"], 70)
        self.assertEqual(m["avgAudibleTtfbMs"], round((240 + 280 + 320 + 900) / 4))
        self.assertEqual(m["p95AudibleTtfbMs"], 900)


if __name__ == "__main__":
    unittest.main()
