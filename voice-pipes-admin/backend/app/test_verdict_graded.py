"""Tests for the graded (v2) verdict + severity.

v1 behavior is pinned by test_anomalies.py + test_health_golden.py; this module
pins the *new* graded semantics: severity bands, the `fair` level, the
critical-path-latency anchor, and the on-critical-path filter that stops the
STT-overlap false-positive from degrading a session.

Run:: python -m unittest backend.app.test_verdict_graded
"""

import unittest

from .report.analysis import _anomaly_severity, _compute_verdict


def _anom(kind, observed, target, *, level="warn", severity=None, on_critical_path=None):
    a = {"kind": kind, "level": level, "observed": observed, "target": target}
    a["severity"] = severity if severity is not None else _anomaly_severity(a)
    if on_critical_path is not None:
        a["on_critical_path"] = on_critical_path
    return a


def _turn(crit_total, *, dominant="tts_ttfb_ms", cold=False, terminated=False):
    return {
        "terminated": terminated,
        "llm": {"cold_start": cold or None},
        "critical_path": {"total_ms": crit_total, "dominant_stage": dominant},
    }


class SeverityBandTests(unittest.TestCase):
    def test_near_target_is_negligible(self):
        # endpointing 513 vs 500 = 1.026x → negligible (the false-positive noise).
        self.assertEqual(_anomaly_severity(_anom("stt.endpointing_high", 513, 500)), "negligible")

    def test_bands(self):
        self.assertEqual(_anomaly_severity(_anom("tts.ttfb_high", 500, 400)), "minor")      # 1.25x
        self.assertEqual(_anomaly_severity(_anom("tts.ttfb_high", 800, 400)), "moderate")   # 2.0x
        self.assertEqual(_anomaly_severity(_anom("tts.ttfb_high", 1200, 400)), "severe")    # 3.0x

    def test_info_is_negligible(self):
        self.assertEqual(_anomaly_severity(_anom("llm.ttft_high", 5000, 800, level="info")), "negligible")

    def test_higher_is_better_direction(self):
        # throughput 5 vs 20 min = 4x under → severe.
        self.assertEqual(_anomaly_severity(_anom("llm.throughput_low", 5, 20)), "severe")

    def test_zero_on_path_contribution_is_negligible(self):
        # A fully-overlapped breach contributes 0ms to the critical path — the
        # old `obs > 0` guard fell through and mis-graded it as "moderate".
        a = {"kind": "stt.processing_high", "level": "warn", "observed": 3224,
             "target": 800, "on_path_observed": 0.0, "on_critical_path": False}
        self.assertEqual(_anomaly_severity(a), "negligible")

    def test_zero_observed_higher_is_better_is_severe(self):
        # For a higher-is-better metric, zero is the worst possible value (and
        # the ratio math would div-by-zero).
        a = {"kind": "llm.throughput_low", "level": "warn", "observed": 0,
             "target": 18}
        self.assertEqual(_anomaly_severity(a), "severe")

    def test_severity_uses_on_path_observed_when_present(self):
        # Real case (session 90ed2134 turn 4): STT processing 4803ms looks severe
        # off the raw metric (6x), but overlaps speech — the on-critical-path
        # share is 1146ms (1.43x = minor). Severity must follow the on-path value.
        a = {"kind": "stt.processing_high", "level": "warn", "observed": 4803,
             "target": 800, "on_path_observed": 1146.1, "on_critical_path": True}
        self.assertEqual(_anomaly_severity(a), "minor")


class GradedVerdictTests(unittest.TestCase):
    def _v2(self, turns, anomalies, aggregates=None):
        return _compute_verdict(turns, aggregates or [], anomalies, {}, model="v2")

    def test_off_critical_stt_processing_does_not_degrade(self):
        # The canonical moonshine case: a SEVERE stt.processing breach that is
        # off the critical path (overlaps speech) must NOT degrade; with an
        # acceptable critical-path p90 the session is fair.
        turns = [_turn(1500, dominant="tts_ttfb_ms") for _ in range(5)]
        anoms = [
            _anom("stt.processing_high", 2226, 800, severity="severe", on_critical_path=False),
            _anom("tts.ttfb_high", 560, 400),  # moderate, on critical path
        ]
        v = self._v2(turns, anoms)
        self.assertEqual(v["level"], "fair")

    def test_overlapped_stt_stall_is_fair_not_degraded(self):
        # A single turn with a raw-huge but overlap-adjusted-minor STT stall (on
        # the critical path) must NOT degrade the whole session when perceived
        # latency held up — it grades as `fair`. Regression for 90ed2134.
        turns = [_turn(1400) for _ in range(5)] + [_turn(2700, dominant="stt_critical_ms")]
        anoms = [
            # severe by raw magnitude, but minor on the critical path:
            _anom("stt.processing_high", 4803, 800, on_critical_path=True),
            _anom("tts.ttfb_high", 761, 400),  # moderate, on path
        ]
        anoms[0]["on_path_observed"] = 1146.1
        anoms[0]["severity"] = _anomaly_severity(anoms[0])  # recompute with on-path
        v = self._v2(turns, anoms)
        self.assertEqual(v["level"], "fair")

    def test_severe_critical_breach_degrades(self):
        # A genuine severe TTS breach on the critical path → degraded.
        turns = [_turn(1500) for _ in range(5)]
        anoms = [_anom("tts.ttfb_high", 1400, 400)]  # 3.5x severe, on critical path
        v = self._v2(turns, anoms)
        self.assertEqual(v["level"], "degraded")
        self.assertEqual(v["primary_culprit"], "tts")

    def test_poor_critical_path_degrades_without_anomalies(self):
        # No anomalies, but the user waited too long → degraded, culprit from the
        # dominant critical-path stage.
        turns = [_turn(3500, dominant="tts_ttfb_ms") for _ in range(5)]
        v = self._v2(turns, [])
        self.assertEqual(v["level"], "degraded")
        self.assertEqual(v["primary_culprit"], "tts")

    def test_good_critical_path_no_anoms_is_healthy(self):
        turns = [_turn(900) for _ in range(5)]
        v = self._v2(turns, [])
        self.assertEqual(v["level"], "healthy")
        self.assertEqual(v["primary_culprit"], "none")

    def test_material_but_acceptable_is_fair(self):
        turns = [_turn(1800) for _ in range(5)]
        anoms = [_anom("tts.ttfb_high", 560, 400)]  # moderate
        v = self._v2(turns, anoms)
        self.assertEqual(v["level"], "fair")

    def test_terminated_majority_is_broken(self):
        turns = [_turn(1000, terminated=True) for _ in range(3)] + [_turn(1000)]
        v = self._v2(turns, [])
        self.assertEqual(v["level"], "broken")

    def test_unusable_critical_path_is_broken(self):
        turns = [_turn(6000) for _ in range(4)]
        v = self._v2(turns, [])
        self.assertEqual(v["level"], "broken")

    def test_cold_start_turn_excluded_from_critical_path(self):
        # A slow cold-start turn must not drag the critical-path p90.
        turns = [_turn(8000, cold=True)] + [_turn(900) for _ in range(4)]
        v = self._v2(turns, [])
        self.assertEqual(v["level"], "healthy")

    def test_v2_exposes_critical_path_fields(self):
        turns = [_turn(1000) for _ in range(3)]
        v = self._v2(turns, [])
        self.assertEqual(v["model"], "v2")
        self.assertEqual(v["critical_path_p90_ms"], 1000.0)


class CulpritAttributionTests(unittest.TestCase):
    """Locks WHICH component gets blamed. The primary culprit must be chosen by
    user-visible impact (severity × on-critical-path), NOT by raw flag count —
    the bug where many negligible/off-path STT flags out-voted a few severe
    on-path TTS breaches (session 84ac6f2d)."""

    def _v2(self, turns, anomalies, aggregates=None):
        return _compute_verdict(turns, aggregates or [], anomalies, {}, model="v2")

    def test_impact_beats_raw_count(self):
        # Session 84ac6f2d shape: STT trips MORE flags (8) than TTS (6), but every
        # STT flag is negligible / off-critical-path while TTS has severe on-path
        # TTFB breaches. Raw count → "stt"; impact → "tts". Lock "tts".
        turns = [_turn(1600) for _ in range(5)]
        stt = [_anom("stt.endpointing_high", 530, 500) for _ in range(4)]           # negligible
        stt += [_anom("stt.processing_high", 1900, 800, severity="negligible",
                      on_critical_path=False) for _ in range(4)]                     # off-path
        tts = [_anom("tts.ttfb_high", 1600, 400) for _ in range(3)]                  # severe, on-path
        tts += [_anom("tts.rtf_high", 1.2, 0.6) for _ in range(3)]                   # moderate
        anoms = stt + tts
        # Precondition: raw count really would blame STT (guards the test itself).
        stt_n = sum(1 for a in anoms if a["kind"].startswith("stt."))
        tts_n = sum(1 for a in anoms if a["kind"].startswith("tts."))
        self.assertGreater(stt_n, tts_n)
        self.assertEqual(self._v2(turns, anoms)["primary_culprit"], "tts")

    def test_off_critical_path_flags_have_zero_impact(self):
        # An off-critical-path SEVERE breach must not win the culprit over an
        # on-path material one — it never reached the user.
        turns = [_turn(1600) for _ in range(5)]
        anoms = [
            _anom("stt.processing_high", 5000, 800, severity="severe", on_critical_path=False),
            _anom("tts.ttfb_high", 560, 400),  # moderate, on path
        ]
        self.assertEqual(self._v2(turns, anoms)["primary_culprit"], "tts")

    def test_genuine_stt_bottleneck_still_blames_stt(self):
        # Guard against over-correcting to TTS: an on-critical-path severe STT
        # breach with only a mild TTS breach must still blame STT.
        turns = [_turn(2000, dominant="stt_critical_ms") for _ in range(5)]
        anoms = [
            _anom("stt.processing_high", 3000, 800, on_critical_path=True),  # 3.75x severe, on-path
            _anom("tts.ttfb_high", 560, 400),                               # moderate
        ]
        self.assertEqual(self._v2(turns, anoms)["primary_culprit"], "stt")

    def test_overage_breaks_equal_impact_tie(self):
        # Two moderate on-path breaches (equal impact) → the wider target-overage
        # wins the tie: llm 2.0x beats tts 1.5x.
        turns = [_turn(1600) for _ in range(5)]
        anoms = [
            _anom("llm.ttft_high", 1600, 800),  # 2.0x moderate
            _anom("tts.ttfb_high", 600, 400),   # 1.5x moderate
        ]
        self.assertEqual(self._v2(turns, anoms)["primary_culprit"], "llm")

    def test_headline_names_the_culprit(self):
        turns = [_turn(1600) for _ in range(5)]
        v = self._v2(turns, [_anom("tts.ttfb_high", 1600, 400)])  # severe
        self.assertEqual(v["primary_culprit"], "tts")
        self.assertIn("TTS", v["headline"])


class V1UnchangedTests(unittest.TestCase):
    def test_v1_still_binary_degraded_on_any_warn(self):
        # The SAME inputs that v2 grades as fair must still be degraded under v1.
        turns = [_turn(1500) for _ in range(5)]
        anoms = [_anom("tts.ttfb_high", 560, 400)]
        v1 = _compute_verdict(turns, [], anoms, {}, model="v1")
        self.assertEqual(v1["level"], "degraded")
        # v1 output carries no critical-path fields.
        self.assertNotIn("critical_path_p90_ms", v1)


if __name__ == "__main__":
    unittest.main()
