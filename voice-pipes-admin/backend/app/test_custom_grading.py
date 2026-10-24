"""Operator-defined grading rules: that they apply, in the right order, and
that a grade reached by them never travels without saying so.

Run with::

    python -m unittest backend.app.test_custom_grading
"""

import os
import pathlib
import re
import unittest
from unittest import mock

from .report.analysis import (
    DEFAULT_LEVELS,
    _compute_verdict,
    _detect_anomalies,
    describe_custom_grading,
    resolve_grading,
)
from .report.anomalies import _anomaly_severity
from .report.targets import _DEFAULT_TARGETS


def _turn(endpointing_ms=744.0, crit_total=1050.0):
    return {
        "utt": 1, "initiator": "user", "terminated": False, "user_transcript": "hello there",
        "stt": {"mode": "batch", "endpointing_ms": endpointing_ms, "processing_ms": 400.0},
        "llm": {"ttft_ms": 110.0, "tokens_per_sec": 80.0, "completion_tokens": 20},
        "tts": {"ttfb_ms": 300.0, "rtf": 0.05, "synthesis_ms": 150.0},
        "total_ms": 400.0,
        "critical_path": {"total_ms": crit_total, "dominant_stage": "endpointing_ms"},
    }


def _verdict(turns, grading):
    anoms = _detect_anomalies(turns, grading["targets"])
    for a in anoms:
        a.setdefault("severity", _anomaly_severity(a))
    return _compute_verdict(turns, [], anoms, {}, model="v2", grading=grading)


def _no_console():
    return mock.patch.dict(os.environ, {"ADMIN_GRADING_TARGETS": "", "ADMIN_GRADING_LEVELS": ""})


class ResolutionTests(unittest.TestCase):
    def test_with_no_rules_a_session_is_graded_by_the_defaults_and_says_nothing(self):
        with _no_console():
            g = resolve_grading({}, {})
        self.assertEqual(g["targets"]["endpointing_ms"], 500)
        self.assertEqual(g["levels"], DEFAULT_LEVELS)
        self.assertEqual(g["custom"], [])
        self.assertIsNone(describe_custom_grading(g["custom"]))

    def test_the_pipelines_rules_apply_and_are_disclosed_with_their_defaults(self):
        with _no_console():
            g = resolve_grading({}, {"grading": {"targets": {"endpointing_ms": 800}}})
        self.assertEqual(g["targets"]["endpointing_ms"], 800.0)
        self.assertEqual(g["custom"], [{
            "name": "endpointing_ms", "value": 800.0, "default": 500, "source": "pipeline config",
        }])

    def test_the_pipeline_wins_over_the_console_and_the_request_over_both(self):
        env = {"ADMIN_GRADING_TARGETS": "endpointing_ms:900,ttft_ms:600", "ADMIN_GRADING_LEVELS": ""}
        with mock.patch.dict(os.environ, env):
            g = resolve_grading({}, {"grading": {"targets": {"endpointing_ms": 800}}})
            self.assertEqual(g["targets"]["endpointing_ms"], 800.0)   # pipeline over console
            self.assertEqual(g["targets"]["ttft_ms"], 600.0)          # console where pipeline is silent
            by_name = {c["name"]: c["source"] for c in g["custom"]}
            self.assertEqual(by_name, {"endpointing_ms": "pipeline config", "ttft_ms": "console"})
            g = resolve_grading({}, {"grading": {"targets": {"endpointing_ms": 800}}},
                                "endpointing_ms:1000")
            self.assertEqual(g["targets"]["endpointing_ms"], 1000.0)

    def test_a_rule_equal_to_its_default_is_not_reported_as_custom(self):
        with _no_console():
            g = resolve_grading({}, {"grading": {"targets": {"endpointing_ms": 500}}})
        self.assertEqual(g["custom"], [])

    def test_the_default_disclosed_is_the_providers_not_the_generic_one(self):
        # A cloud model's TTFT default is already relaxed to 800. Disclosing
        # "default 300" would overstate how far the operator moved it.
        with _no_console():
            g = resolve_grading({"llm": {"provider": "openai-compat", "model": "gpt-4o-mini"}},
                                {"grading": {"targets": {"ttft_ms": 1200}}})
        self.assertEqual(g["custom"][0]["default"], 800)

    def test_unreadable_rules_are_dropped_not_guessed_at(self):
        with _no_console():
            g = resolve_grading({}, {"grading": {
                "targets": {"endpointng_ms": 800, "ttft_ms": -5, "ttfb_ms": "fast",
                            "rtf_max": True, "barge_in_rtt_ms": float("nan")},
                "levels": {"terrible": 9000},
            }})
        self.assertEqual(g["custom"], [])
        self.assertNotIn("endpointng_ms", g["targets"])
        for junk in (None, "lenient", 7, []):
            with _no_console():
                self.assertEqual(resolve_grading({}, {"grading": junk})["custom"], [])

    def test_bands_that_end_up_out_of_order_fall_back_to_the_defaults(self):
        # Each layer may be valid alone and still cross the other.
        env = {"ADMIN_GRADING_TARGETS": "", "ADMIN_GRADING_LEVELS": "good:4500"}
        with mock.patch.dict(os.environ, env):
            g = resolve_grading({}, {"grading": {"levels": {"poor": 4000}}})
        self.assertEqual(g["levels"], DEFAULT_LEVELS)
        self.assertEqual([c for c in g["custom"] if c["name"].startswith("levels.")], [])


class GradeAndDisclosureTests(unittest.TestCase):
    def test_a_deliberately_slower_pipeline_stops_being_graded_fair(self):
        # The session that prompted this: 744 ms endpointing on a 700 ms
        # timeout chosen so callers are not cut off. Against the default 500 it
        # is `fair` on every session for ever.
        with _no_console():
            default = _verdict([_turn()], resolve_grading({}, {}))
            custom = _verdict([_turn()], resolve_grading(
                {}, {"grading": {"targets": {"endpointing_ms": 800}}}))
        self.assertEqual(default["level"], "fair")
        self.assertEqual(custom["level"], "healthy")

    def test_a_custom_grade_never_travels_without_its_rules(self):
        with _no_console():
            v = _verdict([_turn()], resolve_grading(
                {}, {"grading": {"targets": {"endpointing_ms": 800}}}))
        self.assertIn("Graded against custom rules", v["headline"])
        self.assertIn("endpointing_ms 800 (default 500, from pipeline config)", v["headline"])
        self.assertEqual(v["custom_grading"][0]["name"], "endpointing_ms")

    def test_a_default_grade_carries_no_disclosure(self):
        with _no_console():
            v = _verdict([_turn()], resolve_grading({}, {}))
        self.assertNotIn("custom rules", v["headline"])
        self.assertNotIn("custom_grading", v)

    def test_relaxed_rules_do_not_hide_a_real_failure(self):
        # The point of grading is lost if custom rules can launder anything.
        # 3 s of endpointing breaches even a generous 800 ms target severely.
        with _no_console():
            v = _verdict([_turn(endpointing_ms=3000.0, crit_total=3400.0)], resolve_grading(
                {}, {"grading": {"targets": {"endpointing_ms": 800}}}))
        self.assertIn(v["level"], ("degraded", "broken"))

    def test_the_operators_bands_move_the_level(self):
        turn = _turn(endpointing_ms=300.0, crit_total=2500.0)   # no anomaly; slow overall
        with _no_console():
            self.assertEqual(_verdict([turn], resolve_grading({}, {}))["level"], "fair")
            g = resolve_grading({}, {"grading": {"levels": {"good": 2600, "poor": 4000}}})
            v = _verdict([turn], g)
        self.assertEqual(v["level"], "healthy")
        self.assertIn("levels.good 2600 (default 1500", v["headline"])

    def test_a_verdict_computed_without_grading_behaves_as_it_always_did(self):
        anoms = _detect_anomalies([_turn()], dict(_DEFAULT_TARGETS))
        for a in anoms:
            a.setdefault("severity", _anomaly_severity(a))
        v = _compute_verdict([_turn()], [], anoms, {}, model="v2")
        self.assertEqual(v["level"], "fair")
        self.assertNotIn("custom_grading", v)


class RustKeyListDriftTests(unittest.TestCase):
    def test_the_workers_list_of_target_names_matches_the_engines(self):
        # The worker rejects an unknown `[grading.targets]` key at startup, from
        # a list in Rust; the engine applies them from `_DEFAULT_TARGETS`. If
        # the two drift, a valid target is refused or a refused one is valid.
        src = (pathlib.Path(__file__).resolve().parents[3]
               / "voice-pipes" / "src" / "config" / "types.rs")
        if not src.exists():
            self.skipTest("engine source not present beside the console")
        text = src.read_text(encoding="utf-8")
        block = re.search(r"GRADING_TARGET_KEYS: &\[&str\] = &\[(.*?)\];", text, re.S)
        self.assertIsNotNone(block, "GRADING_TARGET_KEYS not found in types.rs")
        rust_keys = set(re.findall(r'"([a-z_]+)"', block.group(1)))
        self.assertEqual(rust_keys, set(_DEFAULT_TARGETS))


if __name__ == "__main__":
    unittest.main()
