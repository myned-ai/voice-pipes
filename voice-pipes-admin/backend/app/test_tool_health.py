"""Tests for client-tool-aware health reporting.

Pins the canonical mis-attribution case (session 8b8958d9): a speech-to-speech
turn following a slow `speak_on_result` tool call carried the whole client
round-trip in its commit-anchored `tts.ttfb_ms` (7777ms of which 7410ms was the
`play_media` client tool), so the verdict graded a healthy pipeline as
"broken — TTS unstable".

Covers:
  - turns._apply_tool_context   — leg classification + TTFB re-anchoring
  - anomalies (tool.*)          — gated vs fire-and-forget attribution
  - aggregates `client_tools`   — round-trips as a first-class component
  - providers s2s detection     — one realtime model = no separate TTS
  - verdict                     — `client_tools` culprit + s2s collapse

Run:: python -m unittest backend.app.test_tool_health
"""

import unittest

from .report.aggregates import _component_aggregates
from .report.anomalies import _detect_anomalies
from .report.providers import _extract_provider_identity
from .report.turns import _apply_tool_context, tool_roundtrip_ms
from .report.verdict import _compute_verdict


def _call(name="play_media", *, after_utt, called=10_000.0, resolved=17_410.0,
          status="completed", speak_on_result=True, spoken=None, acked=None):
    c = {
        "call_id": f"c-{name}-{after_utt}",
        "name": name,
        "status": status,
        "speak_on_result": speak_on_result,
        "spoken": speak_on_result if spoken is None else spoken,
        "called_at_ms": called,
        "after_utterance_id": after_utt,
    }
    if status in ("completed", "failed"):
        c["resolved_at_ms"] = resolved
    if acked is not None:
        c["acked_at_ms"] = acked
    return c


def _flat(utt, *, ttfb=None, audio_ms=None, agent=None, synthesis_ms=500.0):
    """Minimal flattened turn row (mirrors turns._flatten_turn output shape)."""
    return {
        "utt": utt,
        "initiator": "user",
        "interrupted": False,
        "terminated": False,
        "stt": {"mode": "batch", "endpointing_ms": None},
        "llm": {"ttft_ms": None, "cold_start": None},
        "tts": {
            "ttfb_ms": ttfb,
            "internal_ttfb_ms": None,
            "synthesis_ms": synthesis_ms if ttfb is not None else None,
            "audio_duration_ms": audio_ms,
        },
        "agent_transcript": agent,
        "critical_path": {
            "endpointing_ms": 0.0, "stt_critical_ms": 0.0, "llm_ttft_ms": 0.0,
            "tts_ttfb_ms": ttfb or 0.0,
            "total_ms": ttfb, "dominant_stage": "tts_ttfb_ms" if ttfb else None,
        },
    }


def _utt(uid, first_audio=None):
    u = {"utterance_id": uid}
    if first_audio is not None:
        u["_wallclock_ts_ms"] = {"first_audio_out_ms": first_audio}
    return u


class ToolRoundtripTests(unittest.TestCase):
    def test_roundtrip_from_timestamps(self):
        self.assertEqual(tool_roundtrip_ms(_call(after_utt=1)), 7410.0)

    def test_pending_has_no_roundtrip(self):
        self.assertIsNone(tool_roundtrip_ms(_call(after_utt=1, status="pending")))


class ApplyToolContextTests(unittest.TestCase):
    """The 8b8958d9 shape: invocation turn 4 (no speech) → `play_media`
    resolves 7410ms later → followup turn 5's commit-anchored ttfb (7777ms)
    contains the whole wait."""

    def _session(self, *, first_audio=17_925.5, followup_ttfb=7777.0):
        turns = [
            _flat(4, audio_ms=0.0),                              # invocation leg
            _flat(5, ttfb=followup_ttfb, audio_ms=3600.0,
                  agent="Estoy reproduciendo jazz ahora."),       # followup leg
        ]
        utts = [_utt(4), _utt(5, first_audio=first_audio)]
        calls = [_call(after_utt=4)]  # called 10_000 → resolved 17_410
        return turns, utts, calls

    def test_invocation_and_followup_classified(self):
        turns, utts, calls = self._session()
        _apply_tool_context(turns, utts, calls)
        self.assertEqual(turns[0]["kind"], "tool_invocation")
        self.assertEqual(turns[0]["tool_calls"][0]["name"], "play_media")
        self.assertEqual(turns[0]["tool_calls"][0]["roundtrip_ms"], 7410.0)
        self.assertEqual(turns[1]["kind"], "tool_followup")
        self.assertEqual(turns[1]["tool_context"]["invocation_utt"], 4)

    def test_followup_ttfb_reanchored_at_tool_result(self):
        turns, utts, calls = self._session()
        _apply_tool_context(turns, utts, calls)
        tts = turns[1]["tts"]
        # first_audio 17_925.5 − resolved 17_410 = 515.5ms of real model latency.
        self.assertEqual(tts["ttfb_ms"], 515.5)
        self.assertEqual(tts["ttfb_pre_tool_ms"], 7777.0)
        self.assertEqual(tts["ttfb_anchor"], "tool_result")
        # Critical path recomputed off the re-anchored value.
        self.assertEqual(turns[1]["critical_path"]["total_ms"], 515.5)
        self.assertEqual(turns[1]["tool_context"]["excluded_wait_ms"], 7261.5)

    def test_no_reanchor_when_resolved_before_commit(self):
        # Tool resolved long before the followup's commit (user spoke again
        # later): the ttfb window is clean — must stay untouched.
        turns, utts, calls = self._session(first_audio=40_000.0, followup_ttfb=300.0)
        _apply_tool_context(turns, utts, calls)  # commit 39_700 > resolved 17_410
        self.assertEqual(turns[1]["tts"]["ttfb_ms"], 300.0)
        self.assertNotIn("ttfb_anchor", turns[1]["tts"])
        self.assertNotIn("kind", turns[1])

    def test_no_reanchor_when_resolved_after_first_audio(self):
        # Model spoke without waiting for the tool: response wasn't gated.
        turns, utts, calls = self._session(first_audio=15_000.0, followup_ttfb=6000.0)
        _apply_tool_context(turns, utts, calls)  # resolved 17_410 > first_audio
        self.assertEqual(turns[1]["tts"]["ttfb_ms"], 6000.0)
        self.assertNotIn("ttfb_anchor", turns[1]["tts"])

    def test_estimated_reanchor_without_wallclock(self):
        # Legacy/headless capture: no first-audio wallclock — conservatively
        # subtract the round-trip instead of dropping the sample.
        turns, utts, calls = self._session()
        utts[1] = _utt(5)  # strip wallclock
        _apply_tool_context(turns, utts, calls)
        tts = turns[1]["tts"]
        self.assertEqual(tts["ttfb_ms"], 7777.0 - 7410.0)
        self.assertEqual(tts["ttfb_anchor"], "tool_result_estimated")

    def test_speechless_echo_leg_marked_followup(self):
        # Fire-and-forget navigate: realtime providers emit a duplicated user
        # item with no output after the invocation (8b8958d9 turns 7/8).
        turns = [_flat(7, audio_ms=0.0), _flat(8)]
        utts = [_utt(7), _utt(8)]
        calls = [_call("navigate", after_utt=7, resolved=12_800.0,
                       speak_on_result=False, spoken=False)]
        _apply_tool_context(turns, utts, calls)
        self.assertEqual(turns[0]["kind"], "tool_invocation")
        self.assertEqual(turns[1]["kind"], "tool_followup")
        self.assertIsNone(turns[1]["tts"]["ttfb_ms"])

    def test_next_silent_leg_with_its_own_call_is_not_an_echo(self):
        # "Navigate to settings", then "play some jazz": two silent legs in a
        # row, each with its own call. The second is a new invocation, not
        # the first one's echo.
        turns = [_flat(1, audio_ms=0.0), _flat(2, audio_ms=0.0)]
        utts = [_utt(1), _utt(2)]
        calls = [
            _call("navigate", after_utt=1, resolved=12_800.0, speak_on_result=False, spoken=False),
            _call("play_media", after_utt=2, called=20_000.0, resolved=23_000.0),
        ]
        _apply_tool_context(turns, utts, calls)
        self.assertEqual(turns[0]["kind"], "tool_invocation")
        self.assertEqual(turns[1]["kind"], "tool_invocation")
        self.assertNotIn("tool_context", turns[1])

    def test_speaking_turn_with_calls_keeps_metrics_and_kind(self):
        # A turn that speaks AND dispatches a tool is a speech turn; its own
        # latency metrics are valid.
        turns = [_flat(1, ttfb=280.0, audio_ms=2000.0, agent="Sure, navigating.")]
        utts = [_utt(1, first_audio=11_000.0)]
        _apply_tool_context(turns, utts, [_call("navigate", after_utt=1)])
        self.assertNotIn("kind", turns[0])
        self.assertEqual(turns[0]["tts"]["ttfb_ms"], 280.0)
        self.assertEqual(len(turns[0]["tool_calls"]), 1)

    def test_noop_without_calls(self):
        turns = [_flat(1, ttfb=280.0, audio_ms=2000.0, agent="hi")]
        before = [dict(t) for t in turns]
        _apply_tool_context(turns, [_utt(1)], [])
        _apply_tool_context(turns, [_utt(1)], None)
        self.assertEqual(turns, before)


class ToolAnomalyTests(unittest.TestCase):
    TARGETS = {
        "ttft_ms": 800, "ttfb_ms": 400, "endpointing_ms": 500,
        "stt_processing_ms": 800, "rtf_max": 0.6, "tokens_per_sec_min": 20,
        "barge_in_rtt_ms": 400, "self_barge_played_ms_max": 200,
        "outlier_total_ms_factor": 2.0, "tool_roundtrip_ms": 2000,
    }

    def test_slow_roundtrip_is_informational_never_warn(self):
        # Round-trip is APP execution time (media start, navigation, a human
        # resolving the playground card) — a legitimately long-running tool
        # must not grade the session degraded, gated or not.
        anoms = _detect_anomalies([], self.TARGETS, [_call(after_utt=4)])
        (a,) = anoms
        self.assertEqual(a["kind"], "tool.roundtrip_high")
        self.assertEqual(a["level"], "info")
        self.assertTrue(a["on_critical_path"])       # user did audibly wait
        self.assertEqual(a["severity"], "negligible")  # info → zero verdict weight
        self.assertEqual(a["utt"], 4)
        self.assertIn("app-side execution time", a["detail"])

    def test_ack_surfaces_in_detail(self):
        # Client acked at +180ms then executed for 7.4s: responsive
        # integration, long-running action — the detail says so.
        (a,) = _detect_anomalies([], self.TARGETS, [_call(after_utt=4, acked=10_180.0)])
        self.assertIn("client acked after 180ms", a["detail"])

    def test_missing_ack_noted_in_detail(self):
        (a,) = _detect_anomalies([], self.TARGETS, [_call(after_utt=4)])
        self.assertIn("no started/progress ack", a["detail"])

    def test_fire_and_forget_slow_call_is_info(self):
        calls = [_call("navigate", after_utt=7, resolved=12_801.0,
                       speak_on_result=False, spoken=False)]
        (a,) = _detect_anomalies([], self.TARGETS, calls)
        self.assertEqual(a["kind"], "tool.roundtrip_high")
        self.assertEqual(a["level"], "info")
        self.assertFalse(a["on_critical_path"])
        self.assertEqual(a["severity"], "negligible")  # info → negligible

    def test_fast_call_is_clean(self):
        calls = [_call(after_utt=1, resolved=10_500.0)]
        self.assertEqual(_detect_anomalies([], self.TARGETS, calls), [])

    def test_pending_gated_call_is_unresolved_warn(self):
        (a,) = _detect_anomalies([], self.TARGETS, [_call(after_utt=3, status="pending")])
        self.assertEqual(a["kind"], "tool.unresolved")
        self.assertEqual(a["level"], "warn")
        self.assertTrue(a["on_critical_path"])

    def test_failed_call_is_info(self):
        calls = [_call(after_utt=2, resolved=10_100.0, status="failed")]
        kinds = {a["kind"]: a for a in _detect_anomalies([], self.TARGETS, calls)}
        self.assertIn("tool.failed", kinds)
        self.assertEqual(kinds["tool.failed"]["level"], "info")


class ToolAggregateTests(unittest.TestCase):
    def test_client_tools_component(self):
        calls = [
            _call(after_utt=4),                                    # 7410ms
            _call("navigate", after_utt=7, resolved=12_800.8,
                  speak_on_result=False, spoken=False),            # 2800.8ms
            _call(after_utt=9, status="pending"),
            _call(after_utt=10, status="failed", resolved=11_000.0),
        ]
        aggs = {a["kind"]: a for a in _component_aggregates([], client_tool_calls=calls)}
        tools = aggs["client_tools"]
        self.assertEqual(tools["primary_metric"], "roundtrip_ms")
        self.assertEqual(tools["count"], 4)
        self.assertEqual(tools["pending"], 1)
        self.assertEqual(tools["failed"], 1)
        self.assertEqual(tools["max"], 7410.0)

    def test_absent_without_calls(self):
        kinds = {a["kind"] for a in _component_aggregates([], client_tool_calls=[])}
        self.assertNotIn("client_tools", kinds)


class S2SProviderTests(unittest.TestCase):
    def test_realtime_provider_pair_detected(self):
        providers = _extract_provider_identity(
            [], {"stt_provider": "openai-realtime", "tts_provider": "openai-realtime"}
        )
        self.assertTrue(providers.get("s2s"))

    def test_shared_model_fallback_detected(self):
        meta = {
            "stt_model": {"id": "gpt-realtime"},
            "llm_model": {"id": "gpt-realtime"},
            "tts_model": {"id": "gpt-realtime"},
        }
        self.assertTrue(_extract_provider_identity([], meta).get("s2s"))

    def test_component_pipeline_not_s2s(self):
        providers = _extract_provider_identity(
            [], {"stt_provider": "deepgram", "tts_provider": "kokoro"}
        )
        self.assertNotIn("s2s", providers)


class ToolVerdictTests(unittest.TestCase):
    def _turn(self, crit_total, dominant="tts_ttfb_ms"):
        return {
            "terminated": False,
            "llm": {"cold_start": None},
            "critical_path": {"total_ms": crit_total, "dominant_stage": dominant},
        }

    def test_slow_tools_alone_leave_session_healthy(self):
        # End-to-end policy check: a 10s gated tool round-trip produces only
        # info anomalies, so a session with fast pipeline turns stays healthy.
        turns = [self._turn(400) for _ in range(5)]
        anoms = _detect_anomalies(
            turns, ToolAnomalyTests.TARGETS,
            [_call(after_utt=2, resolved=20_000.0)],  # 10s round-trip, gated
        )
        v = _compute_verdict(turns, [], anoms, {"s2s": True}, model="v2")
        self.assertEqual(v["level"], "healthy")
        self.assertEqual(v["primary_culprit"], "none")

    def test_unresolved_gated_call_blames_client_tools(self):
        # The genuine client-tool defect: a spoken confirmation was gated on
        # a call that never resolved — the user waited for speech that never
        # came. This is the one tool anomaly allowed to move the verdict.
        turns = [self._turn(400) for _ in range(5)]
        anoms = _detect_anomalies(
            turns, ToolAnomalyTests.TARGETS,
            [_call(after_utt=3, status="pending")],
        )
        v = _compute_verdict(turns, [], anoms, {"s2s": True}, model="v2")
        self.assertEqual(v["level"], "fair")  # moderate on-path breach
        self.assertEqual(v["primary_culprit"], "client_tools")
        self.assertIn("CLIENT TOOLS", v["headline"])

    def test_s2s_collapses_stage_blame_to_model(self):
        # No anomalies, high critical path dominated by the ttfb stage: in an
        # s2s pipeline the culprit must be the model, not a phantom TTS.
        turns = [self._turn(3500) for _ in range(5)]
        providers = {"llm": {"model": "gpt-realtime"}, "stt": {}, "tts": {}, "s2s": True}
        v = _compute_verdict(turns, [], [], providers, model="v2")
        self.assertEqual(v["level"], "degraded")
        self.assertEqual(v["primary_culprit"], "s2s")
        self.assertIn("gpt-realtime speech-to-speech model", v["headline"])

    def test_non_s2s_stage_blame_unchanged(self):
        turns = [self._turn(3500) for _ in range(5)]
        providers = {"llm": {}, "stt": {}, "tts": {"model": "kokoro"}}
        v = _compute_verdict(turns, [], [], providers, model="v2")
        self.assertEqual(v["primary_culprit"], "tts")
        self.assertIn("TTS (kokoro)", v["headline"])

    def test_s2s_unstable_clause_names_response_latency(self):
        turns = [self._turn(3500) for _ in range(5)]
        aggs = [{"kind": "tts", "unstable": True}]
        providers = {"llm": {"model": "gpt-realtime"}, "stt": {}, "tts": {}, "s2s": True}
        v = _compute_verdict(turns, aggs, [], providers, model="v2")
        self.assertIn("Response latency is unstable", v["headline"])
        # Aggregate kinds stay raw — they name metric buckets, not blame.
        self.assertEqual(v["unstable_components"], ["tts"])


if __name__ == "__main__":
    unittest.main()
