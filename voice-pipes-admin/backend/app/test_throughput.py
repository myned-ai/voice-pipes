"""Throughput observability — backend.

Pins the three throughput anomaly kinds (`output.slower_than_realtime`,
`audio_post.over_budget`, `client.starvation` / `client.jitter`), the annotator
kinds (`annotator.lagging` / `.degraded` / `.unavailable`, metadata-only, capped
at `fair`), the `audio_post:<name>` culprit surviving the s2s collapse, the
"delivers audio slower than real time" provider headline, the terminated-turn
headline fix, the reader's session throughput rollup, and the report's
`avg_rtf`-from-`emit_rtf` for s2s pipelines.

Run:: python -m unittest app.test_throughput
"""

import unittest

from .captures.reader import (
    _compute_session_metrics,
    arrival_rtf,
    project_throughput_metrics,
)
from .report.analysis import (
    _anomaly_severity,
    _compute_verdict,
    _detect_anomalies,
    _flatten_turn,
    _resolve_targets,
)
from .report.render import _build_report_for_llm, _render_report_markdown

TARGETS = _resolve_targets({}, None)
S2S_PROVIDERS = {"s2s": True, "llm": {"provider": "openai_realtime", "model": "gpt-realtime"}}

# The measured incident: 36.285 s of audio emitted in ~60 s of wall time
# with the lip-sync post-processor at 58 ms per 41 ms chunk.
INCIDENT_AUDIO_MS = 36285


def _turn(
    utt: int,
    *,
    emit_rtf: float | None = None,
    source_rtf: float | None = None,
    chain_ms: int | None = None,
    audio_ms: int = INCIDENT_AUDIO_MS,
    posts: dict | None = None,
    playback: dict | None = None,
    annotators: dict | None = None,
    terminated: bool = False,
    interrupted: bool = False,
    ttfb_ms: float = 200.0,
) -> dict:
    """A flattened turn row (the shape `_flatten_turn` produces) with an
    otherwise-clean latency profile so only the throughput rules fire."""
    row: dict = {
        "utt": utt,
        "initiator": "user",
        "interrupted": interrupted,
        "terminated": terminated,
        "total_ms": 3000,
        "stt": {"mode": "streaming", "endpointing_ms": 10, "processing_ms": None},
        "llm": {"ttft_ms": 300, "generation_ms": 800, "completion_tokens": 50,
                "tokens_per_sec": 60.0, "cold_start": None},
        "tts": {"ttfb_ms": ttfb_ms, "synthesis_ms": 500, "rtf": None, "chars": 80,
                "audio_duration_ms": audio_ms, "cold_start": None},
        "critical_path": {"total_ms": 310.0 + ttfb_ms, "dominant_stage": "llm_ttft_ms"},
    }
    if emit_rtf is not None:
        row["output"] = {
            "emit_rtf": emit_rtf,
            "source_rtf": source_rtf,
            "chain_overhead_ms": chain_ms,
            "audio_output_ms": audio_ms,
        }
    if posts:
        row["audio_post"] = posts
    if playback is not None:
        row["playback"] = playback
    if annotators:
        row["annotators"] = annotators
    return row


def _post(ratio: float | None, *, p50: float | None = None, timeouts: int = 0, calls: int = 880) -> dict:
    return {"calls": calls, "latency_p50_ms": p50, "latency_max_ms": (p50 or 0) * 3,
            "timeouts": timeouts, "failures": 0, "budget_ratio": ratio}


def _playback(*, underruns: int, arrival_wall_ms: float | None, audio_ms: float = 36000,
              silence_ms: float = 0, late_n: int = 0, late_max: float | None = None) -> dict:
    ph = {"underrun_count": underruns, "total_silence_ms": silence_ms, "audio_ms": audio_ms,
          "wall_ms": audio_ms + silence_ms, "arrival_wall_ms": arrival_wall_ms,
          "annotation_late_count": late_n, "annotation_late_ms_max": late_max}
    return {
        "underruns": underruns, "total_silence_ms": silence_ms, "audio_ms": audio_ms,
        "wall_ms": audio_ms + silence_ms, "arrival_wall_ms": arrival_wall_ms,
        "arrival_rtf": arrival_rtf(ph), "interrupted": None,
        "annotation_late_count": late_n, "annotation_late_ms_max": late_max,
    }


# The canonical fixtures from the acceptance checklist.
CHAIN_CAUSE = {"emit_rtf": 1.67, "source_rtf": 0.53, "chain_ms": 41365,
               "posts": {"grpc": _post(1.4, p50=58)}}
PROVIDER_CAUSE = {"emit_rtf": 1.3, "source_rtf": 1.25, "chain_ms": 1800}


def _by_kind(anoms: list[dict]) -> dict[str, dict]:
    out: dict[str, dict] = {}
    for a in anoms:
        out.setdefault(a["kind"], a)
    return out


class ThroughputAnomalyFixtureTests(unittest.TestCase):
    def test_chain_caused_slow_emit_flags_output_and_post_processor(self):
        anoms = _by_kind(_detect_anomalies([_turn(1, **CHAIN_CAUSE)], TARGETS))
        self.assertIn("output.slower_than_realtime", anoms)
        self.assertIn("audio_post.over_budget", anoms)
        out = anoms["output.slower_than_realtime"]
        self.assertEqual(out["level"], "error")           # 1.67 >= 1.2
        self.assertEqual(out["observed"], 1.67)
        self.assertEqual(out["target"], 0.9)
        self.assertEqual(out["severity"], "severe")       # absolute band >= 1.5
        self.assertIs(out["on_critical_path"], True)
        self.assertEqual(out["component"], "audio_post:grpc")  # chain gap 1.14 > 0.3
        self.assertIn("36.3s of audio in 60.6s", out["detail"])
        self.assertIn("emit_rtf 1.67", out["detail"])
        self.assertIn("starve after the prebuffer", out["detail"])
        post = anoms["audio_post.over_budget"]
        self.assertEqual(post["level"], "error")
        self.assertEqual(post["component"], "audio_post:grpc")
        self.assertEqual(post["severity"], "severe")      # the chain IS the cause
        self.assertEqual(post["observed"], 1.4)
        self.assertEqual(post["target"], 0.8)
        self.assertIn("post-processor grpc: p50 58ms per 41ms chunk (ratio 1.40); 0 timeouts", post["detail"])
        self.assertIn("matches emit_rtf − source_rtf = +1.14", post["detail"])

    def test_provider_caused_slow_emit_blames_provider_only(self):
        anoms = _by_kind(_detect_anomalies([_turn(1, **PROVIDER_CAUSE)], TARGETS))
        self.assertIn("output.slower_than_realtime", anoms)
        self.assertNotIn("audio_post.over_budget", anoms)
        out = anoms["output.slower_than_realtime"]
        self.assertEqual(out["component"], "tts")         # chain gap 0.05 <= 0.3
        self.assertEqual(out["severity"], "moderate")     # 1.2 <= 1.3 < 1.5
        self.assertEqual(out["level"], "error")
        self.assertIn("provider/TTS delivered slower than real time", out["detail"])

    def test_severity_bands_are_absolute_rtf(self):
        # warn 1.0 / moderate 1.2 / severe 1.5 on the rtf itself, not × target.
        for rtf, want in ((1.05, "minor"), (1.2, "moderate"), (1.5, "severe"), (3.0, "severe")):
            a = _by_kind(_detect_anomalies([_turn(1, emit_rtf=rtf, source_rtf=rtf)], TARGETS))
            self.assertEqual(a["output.slower_than_realtime"]["severity"], want, rtf)
        self.assertEqual(
            _by_kind(_detect_anomalies([_turn(1, emit_rtf=1.05, source_rtf=1.05)], TARGETS))
            ["output.slower_than_realtime"]["level"], "warn")
        self.assertEqual(_anomaly_severity(
            {"kind": "output.slower_than_realtime", "observed": 1.67, "target": 0.9}), "severe")

    def test_post_timeouts_fire_without_a_budget_ratio(self):
        # avatar-sync stopped: every call times out, no latency sample.
        t = _turn(1, emit_rtf=0.55, source_rtf=0.55, chain_ms=0,
                  posts={"grpc": _post(None, timeouts=12, calls=12)})
        anoms = _by_kind(_detect_anomalies([t], TARGETS))
        post = anoms["audio_post.over_budget"]
        self.assertEqual(post["observed"], 12)
        self.assertEqual(post["target"], 0)
        self.assertEqual(post["severity"], "moderate")
        self.assertIn("no latency sample; 12 timeouts", post["detail"])
        self.assertNotIn("output.slower_than_realtime", anoms)

    def test_within_budget_is_silent(self):
        t = _turn(1, emit_rtf=0.5, source_rtf=0.48, chain_ms=60,
                  posts={"grpc": _post(0.6, p50=25)},
                  playback=_playback(underruns=0, arrival_wall_ms=18000))
        kinds = {a["kind"] for a in _detect_anomalies([t], TARGETS)}
        self.assertFalse(kinds & {"output.slower_than_realtime", "audio_post.over_budget",
                                  "client.starvation", "client.jitter"}, kinds)

    def test_old_capture_without_output_block_is_silent(self):
        kinds = {a["kind"] for a in _detect_anomalies([_turn(1)], TARGETS)}
        self.assertFalse(kinds & {"output.slower_than_realtime", "audio_post.over_budget"})

    def test_underruns_with_slow_arrival_are_starvation(self):
        t = _turn(1, **CHAIN_CAUSE,
                  playback=_playback(underruns=9, arrival_wall_ms=54000, audio_ms=36000, silence_ms=14000))
        anoms = _by_kind(_detect_anomalies([t], TARGETS))
        self.assertIn("client.starvation", anoms)
        self.assertNotIn("client.jitter", anoms)
        st = anoms["client.starvation"]
        self.assertEqual(st["observed"], 1.5)             # 54000 / 36000
        self.assertEqual(st["target"], 1.0)
        self.assertEqual(st["level"], "warn")
        self.assertEqual(st["severity"], "severe")
        self.assertEqual(st["component"], "audio_post:grpc")  # follows the emit blame
        self.assertIn("server output starvation", st["detail"])

    def test_starvation_without_output_block_blames_provider(self):
        t = _turn(1, playback=_playback(underruns=3, arrival_wall_ms=40000, audio_ms=36000))
        st = _by_kind(_detect_anomalies([t], TARGETS))["client.starvation"]
        self.assertEqual(st["component"], "tts")

    def test_underruns_with_on_time_arrival_are_jitter(self):
        t = _turn(1, emit_rtf=0.5, source_rtf=0.5, chain_ms=0,
                  playback=_playback(underruns=2, arrival_wall_ms=35000, audio_ms=36000, silence_ms=300))
        anoms = _by_kind(_detect_anomalies([t], TARGETS))
        self.assertIn("client.jitter", anoms)
        self.assertNotIn("client.starvation", anoms)
        j = anoms["client.jitter"]
        self.assertEqual(j["component"], "transport")
        self.assertEqual(j["level"], "warn")
        self.assertEqual(j["severity"], "negligible")     # 300 / 36000 < 2 % silence
        self.assertIn("network / prebuffer", j["detail"])

    def test_jitter_severity_follows_silence_fraction(self):
        t = _turn(1, playback=_playback(underruns=6, arrival_wall_ms=35000, audio_ms=36000, silence_ms=5000))
        j = _by_kind(_detect_anomalies([t], TARGETS))["client.jitter"]
        self.assertEqual(j["severity"], "moderate")       # 13.9 %

    def test_underruns_without_arrival_rtf_fall_back_to_emit_rtf(self):
        # Old client (no arrival_wall_ms) but the server knows it was slow.
        t = _turn(1, **PROVIDER_CAUSE, playback=_playback(underruns=3, arrival_wall_ms=None))
        anoms = _by_kind(_detect_anomalies([t], TARGETS))
        self.assertIn("client.starvation", anoms)
        self.assertEqual(anoms["client.starvation"]["observed"], 1.3)
        # Neither known → undifferentiated, info only.
        t2 = _turn(1, playback=_playback(underruns=3, arrival_wall_ms=None))
        j = _by_kind(_detect_anomalies([t2], TARGETS))["client.jitter"]
        self.assertEqual(j["level"], "info")


class AnnotatorAnomalyTests(unittest.TestCase):
    def _ann(self, **kw) -> dict:
        base = {"segments": 40, "annotations": 40, "latency_p50_ms": 30.0, "latency_max_ms": 80.0,
                "lag_max_ms": 120.0, "dropped_chunks": 0, "timeouts": 0, "skipped_segments": 0,
                "breaker_open": False}
        base.update(kw)
        return {"grpc": base}

    def test_lagging_from_server_lag(self):
        t = _turn(1, emit_rtf=0.5, source_rtf=0.5, chain_ms=0, annotators=self._ann(lag_max_ms=350.0))
        a = _by_kind(_detect_anomalies([t], TARGETS))["annotator.lagging"]
        self.assertEqual(a["level"], "warn")
        self.assertEqual(a["component"], "annotator:grpc")
        self.assertEqual(a["observed"], 350.0)
        self.assertEqual(a["target"], 200)
        self.assertIn("audio unaffected", a["detail"])

    def test_lagging_from_client_late_annotations(self):
        t = _turn(1, annotators=self._ann(),
                  playback=_playback(underruns=0, arrival_wall_ms=18000, late_n=7, late_max=640.0))
        a = _by_kind(_detect_anomalies([t], TARGETS))["annotator.lagging"]
        self.assertEqual(a["component"], "annotator:grpc")
        self.assertIn("7 annotation(s) arrive after their audio started (max 640ms late)", a["detail"])

    def test_degraded_and_unavailable(self):
        t = _turn(1, annotators=self._ann(timeouts=2, dropped_chunks=5, breaker_open=True))
        anoms = _by_kind(_detect_anomalies([t], TARGETS))
        self.assertEqual(anoms["annotator.degraded"]["level"], "warn")
        self.assertEqual(anoms["annotator.degraded"]["observed"], 7)
        self.assertEqual(anoms["annotator.unavailable"]["level"], "error")
        self.assertEqual(anoms["annotator.unavailable"]["severity"], "severe")
        self.assertEqual(anoms["annotator.unavailable"]["component"], "annotator:grpc")

    def test_healthy_annotator_is_silent(self):
        t = _turn(1, annotators=self._ann())
        kinds = {a["kind"] for a in _detect_anomalies([t], TARGETS)}
        self.assertFalse({k for k in kinds if k.startswith("annotator.")})


class VerdictTests(unittest.TestCase):
    def _verdict(self, turns, providers, model="v2"):
        anoms = _detect_anomalies(turns, TARGETS)
        return _compute_verdict(turns, [], anoms, providers, model=model)

    def test_chain_cause_names_the_post_processor_in_s2s(self):
        turns = [_turn(1, emit_rtf=0.5, source_rtf=0.5, chain_ms=10),
                 _turn(2, **CHAIN_CAUSE),
                 _turn(3, **CHAIN_CAUSE)]
        for model in ("v1", "v2"):
            v = self._verdict(turns, S2S_PROVIDERS, model=model)
            self.assertEqual(v["primary_culprit"], "audio_post:grpc", model)   # survives the s2s collapse
            self.assertEqual(v["level"], "degraded", model)
            self.assertIn("POST-PROCESSOR (grpc) is the primary bottleneck", v["headline"])
            self.assertNotIn("speech-to-speech model", v["headline"])
            self.assertEqual(v["supporting_anomaly_kinds"],
                             ["audio_post.over_budget", "output.slower_than_realtime"])

    def test_provider_cause_collapses_to_s2s_with_throughput_headline(self):
        turns = [_turn(1, **PROVIDER_CAUSE), _turn(2, **PROVIDER_CAUSE)]
        v = self._verdict(turns, S2S_PROVIDERS)
        self.assertEqual(v["primary_culprit"], "s2s")
        self.assertIn("the gpt-realtime speech-to-speech model delivers audio slower than real time",
                      v["headline"])

    def test_provider_cause_in_cascade_names_tts(self):
        turns = [_turn(1, **PROVIDER_CAUSE)]
        v = self._verdict(turns, {"tts": {"provider": "cartesia"}})
        self.assertEqual(v["primary_culprit"], "tts")
        self.assertIn("TTS (cartesia) delivers audio slower than real time", v["headline"])

    def test_starvation_alone_follows_emit_blame(self):
        turns = [_turn(1, **CHAIN_CAUSE,
                       playback=_playback(underruns=9, arrival_wall_ms=54000, audio_ms=36000))]
        v = self._verdict(turns, S2S_PROVIDERS)
        self.assertEqual(v["primary_culprit"], "audio_post:grpc")

    def test_jitter_blames_transport_not_server(self):
        turns = [_turn(1, emit_rtf=0.5, source_rtf=0.5, chain_ms=0,
                       playback=_playback(underruns=6, arrival_wall_ms=35000, audio_ms=36000,
                                          silence_ms=5000))]
        v = self._verdict(turns, S2S_PROVIDERS)
        self.assertEqual(v["primary_culprit"], "transport")
        self.assertEqual(v["level"], "fair")

    def test_terminated_turn_without_anomalies_has_honest_headline(self):
        turns = [_turn(3, terminated=True)]
        for model in ("v1", "v2"):
            v = self._verdict(turns, S2S_PROVIDERS, model=model)
            self.assertEqual(v["level"], "broken", model)
            self.assertEqual(v["primary_culprit"], "none", model)
            self.assertEqual(
                v["headline"],
                "Broken session — turn 3 terminated mid-response (client disconnect / "
                "interruption); no metric anomalies across 1 turn(s).",
            )
            self.assertNotIn("NONE", v["headline"])
            self.assertNotIn("bottleneck", v["headline"])

    def test_terminated_with_real_anomalies_keeps_culprit(self):
        turns = [_turn(1, terminated=True), _turn(2, **CHAIN_CAUSE, terminated=True)]
        v = self._verdict(turns, S2S_PROVIDERS)
        self.assertEqual(v["level"], "broken")
        self.assertEqual(v["primary_culprit"], "audio_post:grpc")

    def test_annotator_only_session_is_capped_at_fair(self):
        ann = {"grpc": {"lag_max_ms": 900.0, "timeouts": 0, "dropped_chunks": 0, "breaker_open": True}}
        turns = [_turn(1, emit_rtf=0.5, source_rtf=0.5, chain_ms=0, annotators=ann) for _ in range(3)]
        for model in ("v1", "v2"):
            v = self._verdict(turns, S2S_PROVIDERS, model=model)
            self.assertEqual(v["level"], "fair", model)          # never degraded
            self.assertEqual(v["primary_culprit"], "annotator:grpc", model)  # not collapsed to s2s
            self.assertIn("ANNOTATOR (grpc) — metadata only, audio unaffected", v["headline"])
            self.assertIn("audio unaffected", v["headline"])
            self.assertEqual(sorted(v["supporting_anomaly_kinds"]),
                             ["annotator.lagging", "annotator.unavailable"])

    def test_annotator_never_outranks_an_audio_culprit(self):
        ann = {"grpc": {"lag_max_ms": 900.0, "timeouts": 3, "dropped_chunks": 0, "breaker_open": True}}
        turns = [_turn(1, ttfb_ms=560.0, annotators=ann)]  # tts.ttfb_high moderate
        v = self._verdict(turns, {})
        self.assertEqual(v["primary_culprit"], "tts")
        self.assertEqual(v["level"], "fair")
        self.assertNotIn("audio unaffected", v["headline"])

    def test_clean_throughput_is_healthy(self):
        turns = [_turn(1, emit_rtf=0.5, source_rtf=0.5, chain_ms=0,
                       posts={"grpc": _post(0.6, p50=25)}) for _ in range(3)]
        v = self._verdict(turns, S2S_PROVIDERS)
        self.assertEqual(v["level"], "healthy")
        self.assertEqual(v["primary_culprit"], "none")


def _snapshot(uid: int, *, output: dict | None = None, posts: dict | None = None,
              annotators: dict | None = None) -> dict:
    comps = [{"component_id": "realtime", "kind": "realtime", "provider": "openai_realtime",
              "extras": {"ttfb_ms": 700, "audio_output_ms": (output or {}).get("audio_output_ms", 4000)}}]
    if output is not None:
        comps.append({"component_id": "output", "kind": "output", "extras": output})
    for name, ex in (posts or {}).items():
        comps.append({"component_id": f"audio_post:{name}", "kind": "audio_post",
                      "provider": name, "extras": ex})
    for name, ex in (annotators or {}).items():
        comps.append({"component_id": f"annotator:{name}", "kind": "annotator",
                      "provider": name, "extras": ex})
    return {"utterance_id": uid, "total_ms": 3000, "interrupted": False, "components": comps}


class ReaderProjectionTests(unittest.TestCase):
    def test_project_throughput_metrics(self):
        snap = _snapshot(1, output={"emit_rtf": 1.67, "source_rtf": 0.53,
                                    "chain_overhead_ms": 41365, "audio_output_ms": 36285},
                         posts={"grpc": {"calls": 880, "latency_max_ms": 190, "timeouts": 0,
                                         "failures": 0}},   # no p50 / ratio (optional on the wire)
                         annotators={"grpc": {"segments": 4, "annotations": 4, "lag_max_ms": 120,
                                              "dropped_chunks": 0, "timeouts": 0,
                                              "skipped_segments": 0, "breaker_open": False}})
        thr = project_throughput_metrics(snap)
        # The processor carries no call-time total here, so the difference
        # cannot be apportioned and stays the chain's.
        self.assertEqual(thr["output"], {"emit_rtf": 1.67, "source_rtf": 0.53,
                                         "chain_overhead_ms": 41365, "pacer_wait_ms": None,
                                         "audio_output_ms": 36285})
        self.assertEqual(thr["audio_post"]["grpc"]["calls"], 880)
        self.assertIsNone(thr["audio_post"]["grpc"]["budget_ratio"])
        self.assertIsNone(thr["audio_post"]["grpc"]["latency_p50_ms"])
        self.assertEqual(thr["annotators"]["grpc"]["lag_max_ms"], 120.0)
        self.assertIs(thr["annotators"]["grpc"]["breaker_open"], False)
        # Old capture / no snapshot.
        self.assertEqual(project_throughput_metrics(None), {"output": None, "audio_post": {}, "annotators": {}})
        self.assertIsNone(project_throughput_metrics(_snapshot(1))["output"])

    def test_arrival_rtf_guards(self):
        self.assertEqual(arrival_rtf({"arrival_wall_ms": 54000, "audio_ms": 36000}), 1.5)
        self.assertIsNone(arrival_rtf({"arrival_wall_ms": 54000, "audio_ms": 0}))
        self.assertIsNone(arrival_rtf({"audio_ms": 36000}))       # old client
        self.assertIsNone(arrival_rtf(None))

    def test_session_metrics_rollup(self):
        utts = [
            {"utterance_id": 1, "metric_snapshot": _snapshot(1, output={"emit_rtf": 0.5, "source_rtf": 0.5,
                                                                        "chain_overhead_ms": 0, "audio_output_ms": 4000})},
            {"utterance_id": 2, "metric_snapshot": _snapshot(2, output={"emit_rtf": 1.67, "source_rtf": 0.53,
                                                                        "chain_overhead_ms": 41365, "audio_output_ms": 36285}),
             "summary": {"interrupted": True}},   # interrupted turns count too
            {"utterance_id": 3, "metric_snapshot": _snapshot(3)},  # no audio → no output component
        ]
        m = _compute_session_metrics(utts, [])
        self.assertAlmostEqual(m["avgEmitRtf"], 1.085, places=2)   # (0.5 + 1.67) / 2, 2-dp rounded
        self.assertEqual(m["maxEmitRtf"], 1.67)
        self.assertEqual(m["turnsOverRealtime"], 1)
        self.assertIsNone(m["avgRtf"])                   # cascade TTS meaning untouched
        empty = _compute_session_metrics([{"utterance_id": 1, "metric_snapshot": _snapshot(1)}], [])
        self.assertIsNone(empty["avgEmitRtf"])
        self.assertIsNone(empty["maxEmitRtf"])
        self.assertEqual(empty["turnsOverRealtime"], 0)

    def test_flatten_turn_carries_throughput_blocks(self):
        utt = {
            "utterance_id": 2,
            "summary": {},
            "metric_snapshot": _snapshot(2, output={"emit_rtf": 1.67, "source_rtf": 0.53,
                                                    "chain_overhead_ms": 41365, "audio_output_ms": 36285},
                                         posts={"grpc": {"calls": 880, "latency_p50_ms": 58, "latency_max_ms": 190,
                                                         "timeouts": 0, "failures": 0, "budget_ratio": 1.4}},
                                         annotators={"grpc": {"segments": 4, "annotations": 4, "lag_max_ms": 120,
                                                              "dropped_chunks": 0, "timeouts": 0,
                                                              "skipped_segments": 0, "breaker_open": False}}),
            "playback_health": {"underrun_count": 9, "total_silence_ms": 14000, "audio_ms": 36000,
                                "wall_ms": 50000, "arrival_wall_ms": 54000, "interrupted": True,
                                "annotation_late_count": 0, "annotation_late_ms_max": 0},
        }
        row = _flatten_turn(utt, None, 1, S2S_PROVIDERS)
        self.assertEqual(row["output"]["emit_rtf"], 1.67)
        self.assertEqual(row["audio_post"]["grpc"]["budget_ratio"], 1.4)
        self.assertEqual(row["annotators"]["grpc"]["lag_max_ms"], 120.0)
        self.assertEqual(row["playback"]["arrival_rtf"], 1.5)
        self.assertEqual(row["playback"]["underruns"], 9)
        self.assertIs(row["playback"]["interrupted"], True)
        # Old capture: none of the blocks appear.
        bare = _flatten_turn({"utterance_id": 1, "summary": {}, "metric_snapshot": _snapshot(1)}, None, 1, {})
        for k in ("output", "audio_post", "annotators", "playback"):
            self.assertNotIn(k, bare)


class PacerSplitTests(unittest.TestCase):
    """`chain_overhead_ms` is the post-processors' share only. The engine's
    value is emit − source wall time, which on a pipeline with no processor
    is the realtime pacer holding a fast provider to playback speed (the
    measured case: emit_rtf 1.0 against source_rtf 0.23, 4920 ms on a 6.4 s
    reply)."""

    PACED = {"emit_rtf": 1.0, "source_rtf": 0.23, "chain_overhead_ms": 4920,
             "audio_output_ms": 6400}

    def test_no_post_processor_reports_zero_chain_and_the_pacer_apart(self):
        out = project_throughput_metrics(_snapshot(1, output=dict(self.PACED)))["output"]
        self.assertEqual(out["chain_overhead_ms"], 0)
        self.assertEqual(out["pacer_wait_ms"], 4920)

    def test_post_processor_share_is_its_summed_call_time(self):
        snap = _snapshot(1, output=dict(self.PACED),
                         posts={"gain": {"calls": 160, "latency_max_ms": 2}})
        for c in snap["components"]:
            if c["kind"] == "audio_post":
                c["latency_ms"] = 180
        out = project_throughput_metrics(snap)["output"]
        self.assertEqual(out["chain_overhead_ms"], 180)
        self.assertEqual(out["pacer_wait_ms"], 4740)

    def test_processor_time_above_the_difference_is_capped(self):
        snap = _snapshot(1, output={**self.PACED, "chain_overhead_ms": 100},
                         posts={"gain": {"calls": 160}})
        for c in snap["components"]:
            if c["kind"] == "audio_post":
                c["latency_ms"] = 180
        out = project_throughput_metrics(snap)["output"]
        self.assertEqual(out["chain_overhead_ms"], 100)
        self.assertEqual(out["pacer_wait_ms"], 0)

    def test_report_row_and_markdown_say_so(self):
        utt = {"utterance_id": 1, "summary": {},
               "metric_snapshot": _snapshot(1, output=dict(self.PACED))}
        row = _flatten_turn(utt, None, 1, S2S_PROVIDERS)
        self.assertEqual(row["output"]["chain_overhead_ms"], 0)
        self.assertEqual(row["output"]["pacer_wait_ms"], 4920)
        md = _render_report_markdown(_build_report_for_llm(
            session_id="t", metadata={}, utterances=[], session_metrics=None,
            providers=S2S_PROVIDERS, targets=TARGETS, events=None, health_model="v2",
            generated_at="2026-08-27T00:00:00Z",
        ))
        self.assertIn("`output.pacer_wait_ms`", md)

    def test_slow_emit_without_processors_does_not_blame_a_chain(self):
        # Emit slower than realtime with a fast source and no processor: the
        # gap is not a post-processor's, so none is named.
        row = _turn(1, emit_rtf=1.3, source_rtf=0.5, chain_ms=0)
        row["output"]["pacer_wait_ms"] = 4000
        anoms = _by_kind(_detect_anomalies([row], TARGETS))
        slow = anoms["output.slower_than_realtime"]
        self.assertEqual(slow["component"], "tts")
        self.assertNotIn("post-audio chain is the cause", slow["detail"])
        self.assertNotIn("chain_overhead", slow["detail"])
        self.assertIn("no post-processor ran", slow["detail"])


class ReportTests(unittest.TestCase):
    def _report(self, providers, session_metrics):
        return _build_report_for_llm(
            session_id="t", metadata={}, utterances=[], session_metrics=session_metrics,
            providers=providers, targets=TARGETS, events=None, health_model="v2",
            generated_at="2026-08-27T00:00:00Z",
        )

    def test_s2s_avg_rtf_is_populated_from_emit_rtf(self):
        totals = self._report(S2S_PROVIDERS, {"avgRtf": None, "avgEmitRtf": 1.12, "maxEmitRtf": 1.67,
                                              "turnsOverRealtime": 2})["totals"]
        self.assertEqual(totals["avg_rtf"], 1.12)
        self.assertEqual(totals["avg_rtf_source"], "emit_rtf")
        self.assertEqual(totals["avg_emit_rtf"], 1.12)
        self.assertEqual(totals["max_emit_rtf"], 1.67)
        self.assertEqual(totals["turns_over_realtime"], 2)

    def test_cascade_avg_rtf_keeps_tts_meaning(self):
        totals = self._report({"tts": {"provider": "cartesia"}},
                              {"avgRtf": 0.31, "avgEmitRtf": 0.9, "turnsOverRealtime": 0})["totals"]
        self.assertEqual(totals["avg_rtf"], 0.31)
        self.assertNotIn("avg_rtf_source", totals)
        # And an s2s session on an old capture stays null rather than inventing a value.
        totals = self._report(S2S_PROVIDERS, {"avgRtf": None, "avgEmitRtf": None})["totals"]
        self.assertIsNone(totals["avg_rtf"])
        self.assertNotIn("avg_rtf_source", totals)

    def test_markdown_targets_and_notes(self):
        md = _render_report_markdown(self._report(S2S_PROVIDERS, None))
        self.assertIn("- emit_rtf <= 0.9", md)
        self.assertIn("- post_budget_ratio <= 0.8", md)
        self.assertIn("- annotation_lag_ms <= 200", md)
        self.assertIn("For s2s pipelines `rtf` is the streaming-phase `emit_rtf`", md)
        self.assertIn("`source_rtf` isolates the model, `chain_overhead_ms` the", md)
        legend = self._report(S2S_PROVIDERS, None)["legend"]["metrics"]
        for k in ("emit_rtf_max", "post_budget_ratio_max", "annotation_lag_budget_ms"):
            self.assertEqual(legend[k]["direction"], "lower_better", k)


if __name__ == "__main__":
    unittest.main()
