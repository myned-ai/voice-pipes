"""Honest speech-to-speech latency — backend.

Pins: `tts.ttfb_high` now firing for an anchored S2S turn (1623 ms → error /
severe), `stt.endpointing_high` raised from `provider_endpointing_ms`, the
info-only `s2s.anchor_proxy`, the two S2S verdict verbs ("answers late" /
"is slow to notice the end of speech"), the per-turn `anchor` block +
`stt.endpointing_ms` mapping in `_flatten_turn`, the report notes, the
session/analytics rollups (`avg_provider_endpointing_ms`, `_ROLLUP_SCHEMA` 5)
and one end-to-end capture → report replay of the incident described below.

Run:: python -m unittest app.test_s2s_latency
"""

import json
import unittest
from pathlib import Path
from tempfile import TemporaryDirectory

from .analytics import compute as analytics
from .captures.reader import _compute_session_metrics, _read_capture_artifacts
from .core import config
from .report.analysis import (
    _anomaly_severity,
    _compute_verdict,
    _detect_anomalies,
    _flatten_turn,
    _resolve_targets,
    critical_path_latency,
)
from .report.render import _build_report_for_llm, _render_report_markdown
from .test_s2s_e2e import T0_MS, _client_timing, _realtime_snapshot, _ts

TARGETS = _resolve_targets({}, None)          # ttfb 400, endpointing 500
GEMINI = {"s2s": True, "llm": {"provider": "gemini_live", "model": "gemini-2.5-flash-native-audio"}}
PROXY = "input_transcript_proxy"


def _s2s_turn(
    utt: int,
    *,
    ttfb: float = 300.0,
    source: str | None = PROXY,
    endpt: float | None = None,
    vp: float | None = None,
    client: float | None = None,
    audio: float = 4000.0,
    interrupted: bool = False,
    cold: bool = False,
    first_text: float | None = None,
) -> dict:
    """A flattened realtime turn row (the shape `_flatten_turn` produces for a
    `kind: "realtime"` snapshot): no synthesis_ms / ttft, an `anchor` block,
    and `stt.endpointing_ms` = provider endpointing."""
    row: dict = {
        "utt": utt,
        "initiator": "user",
        "interrupted": interrupted,
        "terminated": False,
        "total_ms": 3000,
        "stt": {"mode": "unknown", "endpointing_ms": endpt, "processing_ms": None, "rtfx": None},
        "llm": {"ttft_ms": None, "generation_ms": None, "completion_tokens": None,
                "tokens_per_sec": None, "cold_start": cold or None},
        "tts": {"ttfb_ms": ttfb, "internal_ttfb_ms": None, "synthesis_ms": None, "rtf": None,
                "chars": 40, "audio_duration_ms": audio, "cold_start": cold or None},
        "e2e_perceived_ms": (endpt or 0) + ttfb,
        "e2e_estimated": True,
        "e2e_anchor_source": "vp_vad" if vp is not None else source,
    }
    if source is not None:
        row["anchor"] = {
            "ttfb_anchor_source": source,
            "first_text_ms": first_text,
            "provider_endpointing_ms": endpt,
            "vp_speech_end_epoch_ms": vp,
            "e2e_anchor_source": row["e2e_anchor_source"],
        }
        if client is not None:
            row["anchor"]["client_speech_end_epoch_ms"] = client
    row["critical_path"] = critical_path_latency(row)
    return row


def _by_kind(anoms: list[dict]) -> dict[str, dict]:
    out: dict[str, dict] = {}
    for a in anoms:
        out.setdefault(a["kind"], a)
    return out


class S2sTtfbAnomalyTests(unittest.TestCase):
    def test_incident_turn_fires_error_severe(self):
        # Gemini "Who are you?", ttfb 1623 from the transcript proxy,
        # audio produced, not interrupted — used to grade Fair with 0 flags.
        a = _by_kind(_detect_anomalies([_s2s_turn(2, ttfb=1623, vp=T0_MS, endpt=400)], TARGETS))
        self.assertIn("tts.ttfb_high", a)
        t = a["tts.ttfb_high"]
        self.assertEqual(t["level"], "error")                       # 1623 >= 2.5 × 400
        self.assertEqual(t["severity"], "severe")
        self.assertEqual(t["observed"], 1623)
        self.assertEqual(t["target"], 400)
        self.assertIs(t["on_critical_path"], True)
        self.assertEqual(t["anchor_source"], PROXY)
        self.assertEqual(t["detail"], "speech-to-speech model: end of speech (input_transcript_proxy) → first audio")
        self.assertNotIn("component", t)                            # default owner `tts` → s2s collapse

    def test_modest_overage_is_warn_minor(self):
        t = _by_kind(_detect_anomalies([_s2s_turn(2, ttfb=500, source="provider_speech_stopped")], TARGETS))["tts.ttfb_high"]
        self.assertEqual(t["level"], "warn")                        # below the 2.5 × severe band
        self.assertEqual(t["severity"], "minor")                    # 1.25 ×
        self.assertIn("(provider_speech_stopped)", t["detail"])
        # Within 15 % of target: still raised (v1 semantics) but negligible.
        t = _by_kind(_detect_anomalies([_s2s_turn(2, ttfb=450, source="vp_vad")], TARGETS))["tts.ttfb_high"]
        self.assertEqual(t["severity"], "negligible")

    def test_gates(self):
        # Interrupted, no audio, legacy `provider` label, `turn_start`, no
        # anchor block, cold turn: none may grade the S2S ttfb.
        cases = {
            "interrupted": _s2s_turn(2, ttfb=1623, interrupted=True),
            "no_audio": _s2s_turn(2, ttfb=1623, audio=0),
            "legacy_provider": _s2s_turn(2, ttfb=1623, source="provider"),
            "turn_start": _s2s_turn(2, ttfb=1623, source="turn_start"),
            "unanchored": _s2s_turn(2, ttfb=1623, source=None),
            "cold": _s2s_turn(1, ttfb=1623, cold=True),
        }
        for name, turn in cases.items():
            kinds = {a["kind"] for a in _detect_anomalies([turn], TARGETS)}
            self.assertNotIn("tts.ttfb_high", kinds, name)

    def test_every_honest_anchor_grades(self):
        for src in ("provider_speech_stopped", PROXY, "vp_vad", "client_speech_end", "text_submit"):
            kinds = {a["kind"] for a in _detect_anomalies([_s2s_turn(2, ttfb=900, source=src)], TARGETS)}
            self.assertIn("tts.ttfb_high", kinds, src)

    def test_cascade_rule_unchanged(self):
        # A cascade turn still needs `synthesis_ms`; an interrupted one stays silent.
        cascade = {"utt": 2, "interrupted": False, "terminated": False, "total_ms": 2000,
                   "stt": {"mode": "streaming", "endpointing_ms": 10, "processing_ms": None},
                   "llm": {"ttft_ms": 200, "cold_start": None},
                   "tts": {"ttfb_ms": 900, "synthesis_ms": 500, "audio_duration_ms": 3000, "cold_start": None},
                   "critical_path": {"total_ms": 1110.0, "dominant_stage": "tts_ttfb_ms"}}
        t = _by_kind(_detect_anomalies([cascade], TARGETS))["tts.ttfb_high"]
        self.assertEqual(t["level"], "warn")
        self.assertEqual(t["detail"], "TTS time-to-first-byte above target")
        self.assertNotIn("anchor_source", t)
        cascade["tts"]["synthesis_ms"] = None
        self.assertNotIn("tts.ttfb_high", {a["kind"] for a in _detect_anomalies([cascade], TARGETS)})


class S2sEndpointingAnomalyTests(unittest.TestCase):
    def test_provider_endpointing_980_fires_once_on_the_critical_path(self):
        anoms = _detect_anomalies([_s2s_turn(2, ttfb=300, endpt=980, vp=T0_MS)], TARGETS)
        eps = [a for a in anoms if a["kind"] == "stt.endpointing_high"]
        self.assertEqual(len(eps), 1)                               # not doubled via stt.endpointing_ms
        e = eps[0]
        self.assertEqual(e["observed"], 980)
        self.assertEqual(e["target"], 500)
        self.assertEqual(e["level"], "warn")
        self.assertEqual(e["severity"], "moderate")                 # 1.96 ×
        self.assertIs(e["on_critical_path"], True)
        self.assertEqual(e["anchor_source"], PROXY)
        self.assertEqual(
            e["detail"],
            "provider VAD/transcription noticed the end of speech 980 ms late (anchor: input_transcript_proxy)",
        )
        self.assertNotIn("tts.ttfb_high", {a["kind"] for a in anoms})

    def test_within_target_is_silent(self):
        kinds = {a["kind"] for a in _detect_anomalies([_s2s_turn(2, ttfb=300, endpt=420, vp=T0_MS)], TARGETS)}
        self.assertNotIn("stt.endpointing_high", kinds)

    def test_cascade_endpointing_detail_unchanged(self):
        row = _s2s_turn(2, ttfb=300, source=None)
        row["stt"]["endpointing_ms"] = 700                          # cascade VAD wait, no anchor block
        e = _by_kind(_detect_anomalies([row], TARGETS))["stt.endpointing_high"]
        self.assertEqual(e["detail"], "VAD endpointing took longer than target (slow turn-taking)")
        self.assertNotIn("on_critical_path", e)


class AnchorProxyTests(unittest.TestCase):
    def test_fires_info_only_when_nothing_better_exists(self):
        a = _by_kind(_detect_anomalies([_s2s_turn(2, ttfb=300)], TARGETS))["s2s.anchor_proxy"]
        self.assertEqual(a["level"], "info")
        self.assertEqual(a["severity"], "negligible")
        self.assertEqual(a["component"], "s2s")
        self.assertEqual(a["observed"], 300)
        self.assertIsNone(a["target"])
        self.assertEqual(a["detail"], "TTFB/E2E measured from a late proxy (provider input transcript); real wait is longer")
        self.assertEqual(_anomaly_severity(a), "negligible")
        # Also on the cold turn: provenance is not a latency.
        self.assertIn("s2s.anchor_proxy", {x["kind"] for x in _detect_anomalies([_s2s_turn(1, cold=True)], TARGETS)})

    def test_silent_with_a_vp_or_client_stamp_or_a_real_anchor(self):
        for name, turn in {
            "vp": _s2s_turn(2, vp=T0_MS, endpt=600),
            "client": _s2s_turn(2, client=T0_MS),
            "openai": _s2s_turn(2, source="provider_speech_stopped"),
            "legacy": _s2s_turn(2, source="provider"),
        }.items():
            self.assertNotIn("s2s.anchor_proxy", {a["kind"] for a in _detect_anomalies([turn], TARGETS)}, name)

    def test_never_affects_the_session_level(self):
        turns = [_s2s_turn(1, cold=True), _s2s_turn(2), _s2s_turn(3)]
        anoms = _detect_anomalies(turns, TARGETS)
        self.assertEqual({a["kind"] for a in anoms}, {"s2s.anchor_proxy"})
        for model in ("v1", "v2"):
            v = _compute_verdict(turns, [], anoms, GEMINI, model=model)
            self.assertEqual(v["level"], "healthy", model)
            self.assertEqual(v["primary_culprit"], "none", model)
            self.assertEqual(v["supporting_anomaly_kinds"], [], model)


class S2sVerdictTests(unittest.TestCase):
    def _verdict(self, turns, providers=GEMINI, model="v2"):
        return _compute_verdict(turns, [], _detect_anomalies(turns, TARGETS), providers, model=model)

    def test_answers_late_headline(self):
        turns = [_s2s_turn(1, ttfb=300, cold=True),
                 _s2s_turn(2, ttfb=1500, vp=T0_MS, endpt=300),
                 _s2s_turn(3, ttfb=1623, vp=T0_MS, endpt=300)]
        v = self._verdict(turns)
        self.assertEqual(v["primary_culprit"], "s2s")
        self.assertEqual(v["level"], "degraded")                   # severe on-path breach
        self.assertIn(
            "the gemini-2.5-flash-native-audio speech-to-speech model answers late "
            "(TTFB p50 1623 ms from input_transcript_proxy); 2 anomaly flag(s) across 3 turn(s).",
            v["headline"],
        )
        self.assertEqual(v["supporting_anomaly_kinds"], ["tts.ttfb_high"])

    def test_slow_to_notice_headline(self):
        turns = [_s2s_turn(1, cold=True),
                 _s2s_turn(2, ttfb=300, endpt=980, vp=T0_MS),
                 _s2s_turn(3, ttfb=300, endpt=900, vp=T0_MS)]
        v = self._verdict(turns)
        self.assertEqual(v["primary_culprit"], "s2s")               # stt owner collapsed to the model
        self.assertEqual(v["level"], "fair")                        # moderate, critical path p90 1.28 s
        self.assertIn(
            "the gemini-2.5-flash-native-audio speech-to-speech model is slow to notice the end "
            "of speech (endpointing p50 980 ms); 2 anomaly flag(s) across 3 turn(s).",
            v["headline"],
        )
        self.assertNotIn("answers late", v["headline"])

    def test_dominant_finding_picks_the_verb(self):
        # Both breach; the severe TTFB out-weighs the moderate endpointing.
        turns = [_s2s_turn(2, ttfb=1623, endpt=980, vp=T0_MS)]
        v = self._verdict(turns)
        self.assertIn("answers late (TTFB p50 1623 ms from input_transcript_proxy)", v["headline"])
        self.assertEqual(v["supporting_anomaly_kinds"], ["stt.endpointing_high", "tts.ttfb_high"])

    def test_cascade_tts_keeps_bottleneck_wording(self):
        cascade = {"utt": 2, "interrupted": False, "terminated": False, "total_ms": 2000,
                   "stt": {"mode": "streaming", "endpointing_ms": 10, "processing_ms": None},
                   "llm": {"ttft_ms": 200, "cold_start": None},
                   "tts": {"ttfb_ms": 1623, "synthesis_ms": 500, "audio_duration_ms": 3000, "cold_start": None},
                   "critical_path": {"total_ms": 1833.0, "dominant_stage": "tts_ttfb_ms"}}
        v = self._verdict([cascade], {"tts": {"provider": "cartesia"}})
        self.assertIn("TTS (cartesia) is the primary bottleneck", v["headline"])


def _incident_extras(ttfb: float, *, endpt: float | None = 980, vp: bool = True, first_text: float | None = 900) -> dict:
    ex = {"ttfb_ms": ttfb, "ttfb_anchor_source": PROXY, "audio_output_ms": 4000, "input_audio_ms": 1300,
          "user_speech_end_epoch_ms": T0_MS + (endpt or 0)}
    if first_text is not None:
        ex["first_text_ms"] = first_text
    if vp:
        ex["vp_speech_end_epoch_ms"] = T0_MS
        ex["provider_endpointing_ms"] = endpt
    return ex


class FlattenTurnTests(unittest.TestCase):
    def test_anchor_block_and_endpointing_mapping(self):
        # Headless client: the reader estimated E2E from the vp anchor.
        utt = {
            "utterance_id": 2,
            "summary": {"e2e_estimated": True, "e2e_anchor_source": "vp_vad"},
            "metric_snapshot": _realtime_snapshot(2, _incident_extras(1623)),
        }
        row = _flatten_turn(utt, None, 1, GEMINI)
        self.assertEqual(row["anchor"], {
            "ttfb_anchor_source": PROXY,
            "first_text_ms": 900.0,
            "provider_endpointing_ms": 980.0,
            "vp_speech_end_epoch_ms": T0_MS,
            "e2e_anchor_source": "vp_vad",
        })
        self.assertEqual(row["stt"]["endpointing_ms"], 980.0)
        self.assertEqual(row["tts"]["ttfb_ms"], 1623.0)
        self.assertIsNone(row["tts"]["synthesis_ms"])
        cp = row["critical_path"]
        self.assertEqual(cp["endpointing_ms"], 980.0)
        self.assertEqual(cp["tts_ttfb_ms"], 1623.0)
        self.assertEqual(cp["total_ms"], 2603.0)                    # vp end of speech → first audio
        self.assertEqual(cp["dominant_stage"], "tts_ttfb_ms")
        self.assertEqual(row["e2e_perceived_ms"], 2603.0)           # sum-of-components estimate
        self.assertEqual(row["e2e_anchor_source"], "vp_vad")

    def test_wallclock_e2e_reports_its_anchor(self):
        utt = {"utterance_id": 2, "summary": {},
               "metric_snapshot": _realtime_snapshot(2, _incident_extras(1623)),
               "_wallclock_ts_ms": {"user_audio_end_ms": T0_MS, "user_audio_end_source": "client_speech_end",
                                    "first_audio_out_ms": T0_MS + 2830, "client_speech_end_ms": T0_MS}}
        row = _flatten_turn(utt, None, 1, GEMINI)
        self.assertEqual(row["e2e_perceived_ms"], 2830.0)
        self.assertFalse(row["e2e_estimated"])
        self.assertEqual(row["e2e_anchor_source"], "client_speech_end")
        self.assertEqual(row["anchor"]["client_speech_end_epoch_ms"], T0_MS)
        self.assertEqual(row["anchor"]["e2e_anchor_source"], "client_speech_end")

    def test_cascade_and_legacy_rows(self):
        bare = _flatten_turn({"utterance_id": 1, "summary": {}, "metric_snapshot": {
            "utterance_id": 1, "total_ms": 1000, "components": [
                {"kind": "stt", "extras": {"endpointing_ms": 300}},
                {"kind": "tts", "extras": {"ttfb_ms": 400, "synthesis_ms": 500}}]}}, None, 1, {})
        self.assertNotIn("anchor", bare)
        self.assertEqual(bare["stt"]["endpointing_ms"], 300.0)
        self.assertIsNone(bare["e2e_anchor_source"])
        legacy = _flatten_turn({"utterance_id": 2, "summary": {}, "metric_snapshot": _realtime_snapshot(
            2, {"ttfb_ms": 800, "user_speech_end_epoch_ms": T0_MS, "audio_output_ms": 3000})}, None, 1, GEMINI)
        self.assertEqual(legacy["anchor"]["ttfb_anchor_source"], "provider")
        self.assertIsNone(legacy["anchor"]["provider_endpointing_ms"])
        self.assertIsNone(legacy["stt"]["endpointing_ms"])
        unanchored = _flatten_turn({"utterance_id": 3, "summary": {}, "metric_snapshot": _realtime_snapshot(
            3, {"ttfb_ms": 3, "audio_output_ms": 3000})}, None, 1, GEMINI)
        self.assertIsNone(unanchored["anchor"]["ttfb_anchor_source"])


class RollupTests(unittest.TestCase):
    def test_session_metrics_avg_provider_endpointing(self):
        utts = [
            {"utterance_id": 1, "metric_snapshot": _realtime_snapshot(1, _incident_extras(300, endpt=400))},
            {"utterance_id": 2, "metric_snapshot": _realtime_snapshot(2, _incident_extras(1623, endpt=980))},
            {"utterance_id": 3, "metric_snapshot": _realtime_snapshot(3, _incident_extras(500, vp=False))},
        ]
        m = _compute_session_metrics(utts, [])
        self.assertEqual(m["avgProviderEndpointingMs"], 690)
        self.assertIsNone(m["avgEndpointingMs"])                    # cascade meaning untouched
        self.assertIsNone(_compute_session_metrics(utts[2:], [])["avgProviderEndpointingMs"])
        totals = _build_report_for_llm(
            session_id="t", metadata={}, utterances=[], session_metrics=m, providers=GEMINI,
            targets=TARGETS, events=None, health_model="v2", generated_at="2026-08-29T00:00:00Z",
        )["totals"]
        self.assertEqual(totals["avg_provider_endpointing_ms"], 690)

    def test_markdown_notes_and_targets(self):
        r = _build_report_for_llm(session_id="t", metadata={}, utterances=[], session_metrics=None,
                                  providers=GEMINI, targets=TARGETS, events=None, health_model="v2",
                                  generated_at="2026-08-29T00:00:00Z")
        md = _render_report_markdown(r)
        self.assertNotIn("entire response latency lives in", md)
        self.assertIn("`stt.endpointing_ms` (= `anchor.provider_endpointing_ms`", md)
        self.assertIn("`anchor.first_text_ms` is the", md)
        self.assertIn("`turn_start` means NO anchor", md)
        self.assertIn("`input_transcript_proxy` (Gemini) lands 0.5–1 s late", md)
        self.assertIn("- ttfb_ms <= 400 (TTS time-to-first-byte; for s2s: end of speech → first audio)", md)
        self.assertIn("- endpointing_ms <= 500 (VAD silence-to-commit; for s2s: provider_endpointing_ms)", md)
        # Cascade report: no s2s paragraph at all.
        md = _render_report_markdown(_build_report_for_llm(
            session_id="t", metadata={}, utterances=[], session_metrics=None, providers={"tts": {}},
            targets=TARGETS, events=None, health_model="v2", generated_at="2026-08-29T00:00:00Z"))
        self.assertNotIn("provider_endpointing_ms`", md)


class CaptureReplayTests(unittest.TestCase):
    """The incident above, end to end: capture files → reader → report."""

    def setUp(self) -> None:
        self._tmp = TemporaryDirectory()
        self._old = config.CAPTURES_DIR
        config.CAPTURES_DIR = Path(self._tmp.name)

    def tearDown(self) -> None:
        config.CAPTURES_DIR = self._old
        self._tmp.cleanup()

    def _write(self, sid: str, *, with_client: bool) -> Path:
        d = Path(self._tmp.name) / sid
        d.mkdir()
        (d / "metadata.json").write_text(json.dumps({"id": sid, "pipeline_name": "gemini-live",
                                                     "start_time": "2026-08-29T20:12:00Z"}), encoding="utf-8")
        turns = {1: _incident_extras(300, endpt=200), 2: _incident_extras(1623, endpt=980),
                 3: _incident_extras(700, endpt=300, vp=False)}   # turn 3: proxy only
        pe = []
        for uid, ex in turns.items():
            pe.append({"kind": "utterance_received", "utterance_id": uid, "source": "realtime",
                       "text": "who are you", "timestamp": _ts(T0_MS + (ex.get("provider_endpointing_ms") or 300))})
            pe.append({"kind": "turn_metrics_ready", "utterance_id": uid, "metrics": _realtime_snapshot(uid, ex)})
            (d / f"{uid:03d}_turn_resolved.json").write_text(json.dumps({"utterance_id": uid}), encoding="utf-8")
        (d / "pipeline_events.jsonl").write_text("".join(json.dumps(e) + "\n" for e in pe), encoding="utf-8")
        events = []
        if with_client:
            events += [_client_timing(2, "speech_end", T0_MS - 40), _client_timing(2, "audio_receive", T0_MS + 2830),
                       _client_timing(2, "playback_start", T0_MS + 3050),
                       _client_timing(3, "audio_receive", T0_MS + 1200)]
        (d / "events.jsonl").write_text("".join(json.dumps(e) + "\n" for e in events), encoding="utf-8")
        return d

    def _report(self, sid: str):
        arts = _read_capture_artifacts(sid)
        return _build_report_for_llm(session_id=sid, metadata={}, utterances=arts["utterances"],
                                     session_metrics=arts["session_metrics"], providers=GEMINI,
                                     targets=TARGETS, events=arts["events"], health_model="v2",
                                     generated_at="2026-08-29T00:00:00Z")

    def test_incident_now_carries_flags_and_names_the_model(self):
        self._write("incident", with_client=True)
        r = self._report("incident")
        flags = sorted(f"{a['utt']}:{a['kind']}:{a['level']}" for a in r["anomalies"])
        self.assertEqual(flags, ["2:stt.endpointing_high:warn", "2:tts.ttfb_high:error",
                                 "3:s2s.anchor_proxy:info", "3:tts.ttfb_high:warn"])
        v = r["verdict"]
        self.assertEqual(v["level"], "degraded")
        self.assertEqual(v["primary_culprit"], "s2s")
        self.assertIn("speech-to-speech model answers late (TTFB p50 1623 ms from input_transcript_proxy)", v["headline"])
        t2 = next(t for t in r["turns"] if t["utt"] == 2)
        self.assertEqual(t2["e2e_perceived_ms"], 2870.0)            # client VAD → client first audio
        self.assertEqual(t2["e2e_anchor_source"], "client_speech_end")
        self.assertFalse(t2["e2e_estimated"])
        self.assertEqual(t2["stt"]["endpointing_ms"], 980.0)
        self.assertEqual(t2["anchor"]["client_speech_end_epoch_ms"], T0_MS - 40)
        self.assertEqual(t2["critical_path"]["total_ms"], 2603.0)
        t3 = next(t for t in r["turns"] if t["utt"] == 3)
        self.assertEqual(t3["e2e_anchor_source"], PROXY)
        self.assertEqual(t3["e2e_perceived_ms"], 900.0)             # 1200 − 300: the proxy under-reports
        self.assertEqual(r["totals"]["avg_provider_endpointing_ms"], 590)
        self.assertEqual(r["worst_turn_timeline"]["utterance_id"], 2)

    def test_headless_replay_estimates_from_vp_anchor(self):
        self._write("headless", with_client=False)
        r = self._report("headless")
        t2 = next(t for t in r["turns"] if t["utt"] == 2)
        self.assertEqual(t2["e2e_perceived_ms"], 2603.0)
        self.assertTrue(t2["e2e_estimated"])
        self.assertEqual(t2["e2e_anchor_source"], "vp_vad")
        self.assertIn("2:tts.ttfb_high:error", [f"{a['utt']}:{a['kind']}:{a['level']}" for a in r["anomalies"]])

    def test_analytics_rollup_carries_the_new_keys(self):
        d = self._write("analytics", with_client=True)
        self.assertGreaterEqual(analytics._ROLLUP_SCHEMA, 6)
        contrib = analytics._compute_session_rollup(
            d, {"id": "analytics", "pipeline_name": "gemini-live", "start_time": "2026-08-29T20:12:00Z"})
        by_utt = {u["utterance_id"]: u for u in contrib["utterances"]}
        self.assertEqual(by_utt[2]["provider_endpointing_ms"], 980.0)
        self.assertEqual(by_utt[2]["first_text_ms"], 900.0)
        self.assertEqual(by_utt[2]["ttfb_anchor_source"], PROXY)
        self.assertEqual(by_utt[2]["e2e_anchor_source"], "client_speech_end")
        self.assertEqual(by_utt[2]["client_ttfa_ms"], 3090.0)      # speech_end → playback_start (existing)
        self.assertIsNone(by_utt[2]["endpointing_ms"])              # cascade key stays STT-only
        self.assertEqual(by_utt[1]["e2e_anchor_source"], "vp_vad")
        self.assertEqual(by_utt[3]["e2e_anchor_source"], PROXY)
        self.assertIsNone(by_utt[3]["provider_endpointing_ms"])
        self.assertEqual(contrib["anomaly"]["by_kind"].get("tts.ttfb_high"), 2)
        self.assertEqual(contrib["anomaly"]["by_kind"].get("stt.endpointing_high"), 1)
        self.assertEqual(contrib["anomaly"]["by_kind"].get("s2s.anchor_proxy"), 1)


if __name__ == "__main__":
    unittest.main()
