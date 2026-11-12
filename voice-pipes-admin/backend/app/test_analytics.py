"""Regression tests for the analytics additions:

* ``analytics.rollup`` — the durable per-session rollup store.
* the rollup-backed window: fresh == cached, and the cached run does no reparse.
* Bad-call auto-tagging.
* Business outcome / CSAT aggregation (incl. pipeline scoping).

Stdlib ``unittest`` so the suite runs without extra deps.  Run with::

    python -m unittest backend.app.test_analytics

Each test builds a throwaway captures dir and points the module-level capture
state + rollup DB at temp paths, so nothing touches real data.
"""

import json
import os
import unittest
from datetime import UTC, datetime, timedelta
from pathlib import Path
from tempfile import TemporaryDirectory
from unittest import mock

import app.analytics.compute as comp
import app.analytics.rollup as rollup
import app.core.config as config


def _recent_iso(days_ago: float = 1.0) -> str:
    return (datetime.now(UTC) - timedelta(days=days_ago)).isoformat()


def _write(path: Path, obj) -> None:
    path.write_text(json.dumps(obj), encoding="utf-8")


def _snapshot_for(uid: int, t: dict) -> dict:
    """Build a canonical ``TurnMetricsSnapshot`` from a turn spec written with
    canonical metric keys (total_ms, ttft_ms, ttfb_ms, endpointing_ms,
    processing_ms, generation_ms, synthesis_ms, audio_duration_ms). This is the
    single source of truth the rollup reads — flat turn_resolved.json carries
    only lifecycle fields now.

    Throughput components: an ``output``
    dict becomes the ``output`` component's extras; ``audio_post`` maps
    processor name → extras (``audio_post:<name>`` components)."""
    stt = {k: t[k] for k in ("processing_ms", "endpointing_ms") if k in t}
    llm = {k: t[k] for k in ("ttft_ms", "generation_ms") if k in t}
    tts = {k: t[k] for k in ("ttfb_ms", "synthesis_ms", "audio_duration_ms") if k in t}
    components = []
    if stt:
        components.append({"component_id": "stt", "kind": "stt", "extras": stt})
    if llm:
        components.append({"component_id": "llm", "kind": "llm", "extras": llm})
    if tts:
        components.append({"component_id": "tts", "kind": "tts", "extras": tts})
    if t.get("output"):
        components.append({"component_id": "output", "kind": "output", "extras": dict(t["output"])})
    for name, extras in (t.get("audio_post") or {}).items():
        components.append({"component_id": f"audio_post:{name}", "kind": "audio_post",
                           "provider": name, "extras": dict(extras)})
    return {
        "utterance_id": uid,
        "total_ms": t.get("total_ms"),
        "interrupted": bool(t.get("interrupted")),
        "components": components,
    }


def _make_session(
    caps: Path,
    sid: str,
    *,
    pipeline: str = "alpha",
    turns: list[dict] | None = None,
    events: list[dict] | None = None,
    outcome: str | None = None,
    csat: int | None = None,
    meta_extra: dict | None = None,
) -> None:
    """Write one synthetic session (metadata + turn files + events.jsonl +
    pipeline_events.jsonl with canonical per-turn snapshots)."""
    d = caps / sid
    d.mkdir()
    meta: dict = {
        "id": sid,
        "start_time": _recent_iso(),
        "started_at": _recent_iso(),
        "pipeline_name": pipeline,
        "stt_provider": "deepgram",
        "tts_provider": "cartesia",
    }
    if meta_extra:
        meta.update(meta_extra)
    if outcome is not None:
        meta["outcome"] = outcome
    if csat is not None:
        meta["csat"] = csat
    _write(d / "metadata.json", meta)
    turn_specs = turns or [{"total_ms": 1000}]
    pe_lines: list[str] = []
    for i, t in enumerate(turn_specs, start=1):
        # turn_resolved.json: lifecycle only (utterance_id + any flags).
        lifecycle = {k: v for k, v in t.items() if k in ("interrupted", "terminated", "interrupt_trigger")}
        _write(d / f"{i:03d}_turn_resolved.json", {"utterance_id": i, **lifecycle})
        pe_lines.append(json.dumps(
            {"kind": "turn_metrics_ready", "utterance_id": i, "metrics": _snapshot_for(i, t)}
        ))
        # NNN_playback.json: the client's persisted playback_quality frame.
        if t.get("playback"):
            _write(d / f"{i:03d}_playback.json", t["playback"])
    (d / "pipeline_events.jsonl").write_text(
        "".join(line + "\n" for line in pe_lines), encoding="utf-8",
    )
    (d / "events.jsonl").write_text(
        "".join(json.dumps(e) + "\n" for e in (events or [])), encoding="utf-8",
    )


class RollupStoreTest(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = TemporaryDirectory()
        # The rollup store follows ADMIN_STATE_DIR, read per call.
        env = mock.patch.dict(os.environ, {"ADMIN_STATE_DIR": self._tmp.name})
        env.start()
        self.addCleanup(env.stop)

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def test_put_get_roundtrip(self) -> None:
        payload = {"utterances": [{"a": 1}], "cost_events": [["p", "llm", 0.01]]}
        rollup.put("s1", "alpha", 1.0, 10.0, 20.0, 1, payload)
        self.assertEqual(rollup.get_fresh("s1", 10.0, 20.0, 1), payload)

    def test_changed_mtime_invalidates(self) -> None:
        rollup.put("s1", "alpha", 1.0, 10.0, 20.0, 1, {"x": 1})
        self.assertIsNone(rollup.get_fresh("s1", 11.0, 20.0, 1))  # events.jsonl changed
        self.assertIsNone(rollup.get_fresh("s1", 10.0, 21.0, 1))  # a turn file added (dir mtime)

    def test_schema_bump_invalidates(self) -> None:
        rollup.put("s1", "alpha", 1.0, 10.0, 20.0, 1, {"x": 1})
        self.assertIsNone(rollup.get_fresh("s1", 10.0, 20.0, 2))

    def test_unknown_session_is_none(self) -> None:
        self.assertIsNone(rollup.get_fresh("missing", 1.0, 1.0, 1))

    def test_upsert_overwrites(self) -> None:
        rollup.put("s1", "alpha", 1.0, 10.0, 20.0, 1, {"v": 1})
        rollup.put("s1", "alpha", 1.0, 11.0, 21.0, 1, {"v": 2})
        self.assertIsNone(rollup.get_fresh("s1", 10.0, 20.0, 1))   # old freshness gone
        self.assertEqual(rollup.get_fresh("s1", 11.0, 21.0, 1), {"v": 2})


class _WindowTest(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = TemporaryDirectory()
        self.caps = Path(self._tmp.name) / "captures"
        self.caps.mkdir()
        self._old_caps = config.CAPTURES_DIR
        config.CAPTURES_DIR = self.caps
        # The rollup store follows ADMIN_STATE_DIR, read per call.
        env = mock.patch.dict(os.environ, {"ADMIN_STATE_DIR": self._tmp.name})
        env.start()
        self.addCleanup(env.stop)

    def tearDown(self) -> None:
        config.CAPTURES_DIR = self._old_caps
        self._tmp.cleanup()

    def window(self, pipeline: str | None = None) -> dict:
        return comp._compute_analytics_window(3650, 0, pipeline)["response"]


class RollupWindowTest(_WindowTest):
    def test_fresh_equals_cached_and_no_reparse(self) -> None:
        _make_session(self.caps, "s1", turns=[{"total_ms": 1200, "ttft_ms": 300}])
        _make_session(self.caps, "s2", pipeline="beta", turns=[{"total_ms": 900}])

        calls = {"n": 0}
        orig = comp._compute_session_rollup
        comp._compute_session_rollup = lambda *a, **k: (calls.__setitem__("n", calls["n"] + 1) or orig(*a, **k))
        try:
            first = self.window()
            n_first = calls["n"]
            second = self.window()
            n_second = calls["n"] - n_first
        finally:
            comp._compute_session_rollup = orig

        self.assertEqual(json.dumps(first, sort_keys=True), json.dumps(second, sort_keys=True))
        self.assertEqual(n_first, 2)    # both sessions parsed once
        self.assertEqual(n_second, 0)   # second scan fully served from the rollup


class BadCallTaggingTest(_WindowTest):
    def _tags_by_session(self, resp: dict) -> dict[str, list[str]]:
        return {f["session_id"]: f["tags"] for f in resp["flagged_sessions"]}

    def test_each_symptom_produces_its_tag(self) -> None:
        _make_session(self.caps, "slow", turns=[{"total_ms": 5000}])
        _make_session(self.caps, "under", turns=[{"total_ms": 1000}],
                      events=[{"event_type": "client_timing",
                               "payload": {"utterance_id": 1, "event": "underrun", "client_timestamp_ms": 1}}])
        _make_session(self.caps, "barge",
                      turns=[{"total_ms": 1000}, {"total_ms": 1000}],
                      events=[{"event_type": "barge_in_rtt", "payload": {"utterance_id": 1, "rtt_ms": 200}},
                              {"event_type": "barge_in_rtt", "payload": {"utterance_id": 2, "rtt_ms": 200}}])
        _make_session(self.caps, "err", turns=[{"total_ms": 1000}],
                      events=[{"event_type": "llm_error", "payload": {"utterance_id": 1, "message": "boom"}}])
        _make_session(self.caps, "sad", turns=[{"total_ms": 1000}], csat=1)
        _make_session(self.caps, "clean", turns=[{"total_ms": 1000}], csat=5)

        tags = self._tags_by_session(self.window())
        self.assertIn("high_latency", tags["slow"])
        self.assertIn("underrun", tags["under"])
        self.assertIn("barge_storm", tags["barge"])
        self.assertIn("errors", tags["err"])
        self.assertIn("low_csat", tags["sad"])
        self.assertNotIn("clean", tags)  # a healthy session isn't flagged

    def test_bad_calls_are_emitted_for_issue_persistence(self) -> None:
        _make_session(self.caps, "slow", turns=[{"total_ms": 9000}])
        out = comp._compute_analytics_window(3650, 0, None)
        bad = out["issue_inputs"]["bad_calls"]
        self.assertEqual(len(bad), 1)
        self.assertEqual(bad[0]["session_id"], "slow")
        self.assertIn("high_latency", bad[0]["tags"])


class ClientTimingTest(_WindowTest):
    """`client_ttfa_ms` / `client_receive_to_play_ms` derive from the client's
    `speech_end` / `audio_receive` / `playback_start` timing reports."""

    @staticmethod
    def _timing(uid: int, event: str, ts: int) -> dict:
        return {"event_type": "client_timing",
                "payload": {"utterance_id": uid, "event": event, "client_timestamp_ms": ts}}

    def test_receive_to_play_and_ttfa_from_timing_reports(self) -> None:
        _make_session(self.caps, "s1", turns=[{"total_ms": 1000}, {"total_ms": 1000}],
                      events=[self._timing(1, "speech_end", 1000),
                              self._timing(1, "audio_receive", 1800),
                              self._timing(1, "playback_start", 2050),
                              # turn 2: no audio_receive → only TTFA derivable
                              self._timing(2, "speech_end", 5000),
                              self._timing(2, "playback_start", 5900)])
        utts = {u["utterance_id"]: u for u in self.window()["utterances"]}
        self.assertEqual(utts[1]["client_ttfa_ms"], 1050)
        self.assertEqual(utts[1]["client_receive_to_play_ms"], 250)
        self.assertEqual(utts[2]["client_ttfa_ms"], 900)
        self.assertIsNone(utts[2]["client_receive_to_play_ms"])

    def test_receive_to_play_is_none_when_clocks_disagree(self) -> None:
        _make_session(self.caps, "s1", turns=[{"total_ms": 1000}],
                      events=[self._timing(1, "audio_receive", 2000),
                              self._timing(1, "playback_start", 1900)])
        utts = self.window()["utterances"]
        self.assertIsNone(utts[0]["client_receive_to_play_ms"])
        self.assertIsNone(utts[0]["client_ttfa_ms"])


class ThroughputAnalyticsTest(_WindowTest):
    """Per-utterance `emit_rtf` / `source_rtf` / `chain_overhead_ms` /
    `post_budget_ratio_max` / `post_timeouts` / `arrival_rtf` plus the session
    and window rollups."""

    SLOW = {
        "total_ms": 3000,
        "output": {"emit_rtf": 1.67, "source_rtf": 0.53, "chain_overhead_ms": 41365, "audio_output_ms": 36285},
        "audio_post": {
            "grpc": {"calls": 880, "latency_p50_ms": 58, "latency_max_ms": 190, "timeouts": 2,
                     "failures": 0, "budget_ratio": 1.4},
            "gain": {"calls": 880, "latency_p50_ms": 1, "latency_max_ms": 3, "timeouts": 0,
                     "failures": 0, "budget_ratio": 0.02},
        },
        "playback": {"underrun_count": 9, "total_silence_ms": 14000, "audio_ms": 36000,
                     "wall_ms": 50000, "arrival_wall_ms": 54000, "interrupted": True},
    }
    FAST = {
        "total_ms": 1000,
        "output": {"emit_rtf": 0.5, "source_rtf": 0.5, "chain_overhead_ms": 0, "audio_output_ms": 4000},
        "audio_post": {"grpc": {"calls": 100, "latency_max_ms": 40, "timeouts": 0, "failures": 0}},
    }

    def test_per_utterance_fields_and_rollups(self) -> None:
        _make_session(self.caps, "s1", turns=[self.SLOW, self.FAST, {"total_ms": 900}])
        resp = self.window()
        utts = {u["utterance_id"]: u for u in resp["utterances"]}

        slow = utts[1]
        self.assertEqual(slow["emit_rtf"], 1.67)
        self.assertEqual(slow["source_rtf"], 0.53)
        self.assertEqual(slow["chain_overhead_ms"], 41365)
        self.assertEqual(slow["post_budget_ratio_max"], 1.4)   # worst processor
        self.assertEqual(slow["post_timeouts"], 2)
        self.assertEqual(slow["arrival_rtf"], 1.5)             # 54000 / 36000

        fast = utts[2]
        self.assertEqual(fast["emit_rtf"], 0.5)
        self.assertIsNone(fast["post_budget_ratio_max"])       # no ratio on the wire (no calls timed)
        self.assertEqual(fast["post_timeouts"], 0)
        self.assertIsNone(fast["arrival_rtf"])                 # no playback frame

        old = utts[3]                                          # capture without an `output` component
        for k in ("emit_rtf", "source_rtf", "chain_overhead_ms", "post_budget_ratio_max",
                  "post_timeouts", "arrival_rtf"):
            self.assertIsNone(old[k], k)

        kpi = resp["kpi"]
        self.assertAlmostEqual(kpi["avg_emit_rtf"], (1.67 + 0.5) / 2, places=3)
        self.assertEqual(kpi["max_emit_rtf"], 1.67)
        self.assertEqual(kpi["p90_emit_rtf"], 1.67)
        self.assertEqual(kpi["turns_over_realtime"], 1)
        # The new anomaly kinds reach the window feed through the same rubric.
        self.assertIn("output.slower_than_realtime", kpi["anomalies_by_kind"])
        self.assertIn("audio_post.over_budget", kpi["anomalies_by_kind"])
        self.assertIn("client.starvation", kpi["anomalies_by_kind"])

    def test_session_rollup_contribution(self) -> None:
        _make_session(self.caps, "s1", turns=[self.SLOW, self.FAST])
        meta = json.loads((self.caps / "s1" / "metadata.json").read_text(encoding="utf-8"))
        contrib = comp._compute_session_rollup(self.caps / "s1", meta)
        self.assertEqual(contrib["throughput"],
                         {"avg_emit_rtf": 1.085, "max_emit_rtf": 1.67, "turns_over_realtime": 1})
        self.assertGreaterEqual(comp._ROLLUP_SCHEMA, 4)  # shape changed → old rollups invalidated

    def test_window_without_output_components(self) -> None:
        _make_session(self.caps, "s1", turns=[{"total_ms": 1000}])
        kpi = self.window()["kpi"]
        self.assertIsNone(kpi["avg_emit_rtf"])
        self.assertIsNone(kpi["p90_emit_rtf"])
        self.assertIsNone(kpi["max_emit_rtf"])
        self.assertEqual(kpi["turns_over_realtime"], 0)


class OutcomeAggregationTest(_WindowTest):
    def test_containment_escalation_and_csat(self) -> None:
        _make_session(self.caps, "c1", outcome="contained", csat=5)
        _make_session(self.caps, "c2", outcome="contained", csat=4)
        _make_session(self.caps, "e1", outcome="escalated", csat=2)
        _make_session(self.caps, "a1", outcome="abandoned")  # no csat

        kpi = self.window()["kpi"]
        self.assertEqual(kpi["outcome_known"], 4)
        self.assertEqual(kpi["contained"], 2)
        self.assertEqual(kpi["containment_rate_pct"], 50.0)
        self.assertEqual(kpi["escalation_rate_pct"], 25.0)
        self.assertEqual(kpi["csat_samples"], 3)
        self.assertAlmostEqual(kpi["avg_csat"], (5 + 4 + 2) / 3, places=2)

    def test_outcome_fields_are_none_without_tags(self) -> None:
        _make_session(self.caps, "s1", turns=[{"total_ms": 1000}])  # no outcome/csat
        kpi = self.window()["kpi"]
        self.assertEqual(kpi["outcome_known"], 0)
        self.assertIsNone(kpi["containment_rate_pct"])
        self.assertIsNone(kpi["avg_csat"])

    def test_outcomes_are_pipeline_scoped(self) -> None:
        _make_session(self.caps, "a1", pipeline="alpha", outcome="contained", csat=5)
        _make_session(self.caps, "a2", pipeline="alpha", outcome="escalated", csat=3)
        _make_session(self.caps, "b1", pipeline="beta", outcome="contained", csat=4)

        kpi = self.window(pipeline="alpha")["kpi"]
        self.assertEqual(kpi["outcome_known"], 2)        # only alpha's sessions
        self.assertEqual(kpi["containment_rate_pct"], 50.0)
        self.assertEqual(self.window()["available_pipelines"], ["alpha", "beta"])


class ClearAndLiteAnalyticsTest(_WindowTest):
    def test_lite_mode_skips_anomalies_and_preserves_full_rollup_freshness(self) -> None:
        _make_session(self.caps, "s1", pipeline="alpha", turns=[ThroughputAnalyticsTest.SLOW])
        lite_resp = comp._compute_analytics_window(3650, 0, None, lite=True)["response"]
        self.assertEqual(lite_resp["kpi"]["total_sessions"], 1)
        self.assertEqual(lite_resp["anomaly_sessions"], [])
        self.assertEqual(lite_resp["kpi"]["anomalies_total"], 0)

        # Full mode should NOT reuse the lite rollup as a full rollup — it must compute anomalies.
        full_resp = comp._compute_analytics_window(3650, 0, None, lite=False)["response"]
        self.assertGreater(full_resp["kpi"]["anomalies_total"], 0)
        self.assertEqual(len(full_resp["anomaly_sessions"]), 1)

    def test_clear_session_dismisses_single_session(self) -> None:
        _make_session(self.caps, "s1", pipeline="alpha", turns=[{"total_ms": 1000}])
        _make_session(self.caps, "s2", pipeline="beta", turns=[{"total_ms": 1200}])
        self.assertEqual(self.window()["kpi"]["total_sessions"], 2)

        res = comp.clear_analytics_data(session_id="s1", delete_captures=False)
        self.assertIn("s1", res["cleared"]["sessions"])
        self.assertEqual(res["deleted_captures"], 0)
        self.assertTrue((self.caps / "s1").exists())  # soft clear keeps capture on disk

        after = self.window()
        self.assertEqual(after["kpi"]["total_sessions"], 1)
        self.assertEqual([u["session_id"] for u in after["utterances"]], ["s2"])

    def test_clear_pipeline_resets_only_target_pipeline(self) -> None:
        _make_session(self.caps, "s1", pipeline="alpha", turns=[{"total_ms": 1000}])
        _make_session(self.caps, "s2", pipeline="beta", turns=[{"total_ms": 1200}])

        res = comp.clear_analytics_data(pipeline="alpha", delete_captures=False)
        self.assertIn("alpha", res["cleared"]["pipelines"])

        after = self.window()
        self.assertEqual(after["kpi"]["total_sessions"], 1)
        self.assertEqual(after["available_pipelines"], ["beta"])

    def test_clear_all_with_delete_captures_removes_directories(self) -> None:
        _make_session(self.caps, "s1", pipeline="alpha", turns=[{"total_ms": 1000}])
        _make_session(self.caps, "s2", pipeline="beta", turns=[{"total_ms": 1200}])

        res = comp.clear_analytics_data(delete_captures=True)
        self.assertGreater(res["cleared"]["global_cleared_at"], 0)
        self.assertEqual(res["deleted_captures"], 2)
        self.assertFalse((self.caps / "s1").exists())
        self.assertFalse((self.caps / "s2").exists())
        self.assertEqual(self.window()["kpi"]["total_sessions"], 0)

    def test_resource_metrics_aggregated_in_kpi(self) -> None:
        _make_session(
            self.caps,
            "s1",
            pipeline="alpha",
            turns=[{"total_ms": 1000}],
            meta_extra={
                "peak_rss_bytes": 64 * 1024 * 1024,
                "container_peak_memory_bytes": 78 * 1024 * 1024,
                "avg_turn_cpu_ms": 35.5,
            },
        )
        _make_session(
            self.caps,
            "s2",
            pipeline="beta",
            turns=[{"total_ms": 1200}],
            meta_extra={
                "peak_rss_bytes": 72 * 1024 * 1024,
                "container_peak_memory_bytes": 82 * 1024 * 1024,
                "avg_turn_cpu_ms": 42.5,
            },
        )
        res = self.window()
        kpi = res["kpi"]
        self.assertEqual(kpi["avg_peak_rss_bytes"], 68 * 1024 * 1024)
        self.assertEqual(kpi["p95_peak_rss_bytes"], 72 * 1024 * 1024)
        self.assertEqual(kpi["max_container_peak_bytes"], 82 * 1024 * 1024)
        self.assertEqual(kpi["avg_turn_cpu_ms"], 39.0)


if __name__ == "__main__":
    unittest.main()

