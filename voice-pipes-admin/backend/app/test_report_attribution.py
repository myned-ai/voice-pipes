"""The verdict must name the real cause, and count what a person felt.

Both faults here sat downstream of correct numbers: every measurement on
session 92d2a30c was right, and the headline still read "STT (parakeet-tdt-v3)
is the primary bottleneck; 8 anomaly flag(s)". The recogniser had added 16 to
75 ms a turn -- the other 700 were `silence_timeout_ms = 700` in a config file
-- and four of the eight flags were ones the engine had itself marked off the
critical path with negligible severity.

Run with::

    python -m unittest backend.app.test_report_attribution
"""

import unittest

from .report.analysis import _compute_verdict, _detect_anomalies
from .report.anomalies import _anomaly_severity, _endpointing_attribution
from .report.providers import _coerce_turn_detection

TARGETS = {
    "ttft_ms": 300, "ttfb_ms": 400, "endpointing_ms": 500, "stt_processing_ms": 800,
    "draining_handshake_ms": 1500, "rtf_max": 0.6, "tokens_per_sec_min": 18,
    "barge_in_rtt_ms": 400, "self_barge_played_ms_max": 200,
    "outlier_total_ms_factor": 2.0,
}

# endpointing_ms, processing_ms for turns 1-4 of the session, as measured.
MEASURED = [(736.0, 1729.0), (744.0, 2197.0), (775.0, 2688.0), (716.0, 11554.0)]


def _turn(utt, endpointing_ms, processing_ms, silence_timeout_ms=700.0):
    return {
        "utt": utt, "initiator": "user", "terminated": False,
        "user_transcript": "some words",
        "stt": {"mode": "batch", "endpointing_ms": endpointing_ms,
                "processing_ms": processing_ms, "rtfx": 6.0,
                "silence_timeout_ms": silence_timeout_ms},
        "llm": {"ttft_ms": 110.0, "tokens_per_sec": 80.0, "completion_tokens": 20},
        "tts": {"ttfb_ms": 300.0, "rtf": 0.05, "synthesis_ms": 150.0},
        "total_ms": 400.0,
        "critical_path": {"total_ms": endpointing_ms + 300.0,
                          "dominant_stage": "endpointing_ms"},
    }


def _graded(turns):
    anoms = _detect_anomalies(turns, TARGETS)
    for a in anoms:
        a.setdefault("severity", _anomaly_severity(a))
    return anoms


class EndpointingAttributionTests(unittest.TestCase):
    def test_a_wait_the_timeout_explains_belongs_to_the_timeout(self):
        owner, why = _endpointing_attribution(744.0, 500.0, 700.0)
        self.assertEqual(owner, "turn_detection")
        self.assertIn("700 ms of silence", why)
        self.assertIn("added 44 ms", why)

    def test_a_recogniser_that_really_was_late_keeps_the_blame(self):
        # Session 049589c7's shape: 511 ms on top of the wait is the
        # recogniser's, and no setting excuses it.
        owner, why = _endpointing_attribution(1211.0, 500.0, 700.0)
        self.assertIsNone(owner)
        self.assertIn("511 ms after", why)

    def test_a_timeout_inside_the_target_cannot_be_the_cause(self):
        owner, _ = _endpointing_attribution(620.0, 500.0, 300.0)
        self.assertIsNone(owner)

    def test_with_no_timeout_on_record_nothing_is_claimed(self):
        # Captures from before the engine recorded it. Guessing a floor would
        # excuse a slow recogniser on no evidence.
        for unknown in (None, 0, -5, True, "700"):
            with self.subTest(timeout=unknown):
                owner, why = _endpointing_attribution(744.0, 500.0, unknown)
                self.assertIsNone(owner)
                self.assertIn("slow turn-taking", why)

    def test_the_boundary_goes_to_the_larger_share(self):
        # Floor 200 past target; recogniser adds 199, then 201.
        self.assertEqual(_endpointing_attribution(899.0, 500.0, 700.0)[0], "turn_detection")
        self.assertIsNone(_endpointing_attribution(901.0, 500.0, 700.0)[0])


class VerdictNamesTheRealCauseTests(unittest.TestCase):
    def _verdict(self, turns, model="v2"):
        return _compute_verdict(turns, [], _graded(turns), {"stt": {"model": "parakeet-tdt-v3"}},
                                model=model)

    def test_the_measured_session_blames_the_setting_not_the_recogniser(self):
        turns = [_turn(i + 1, e, p) for i, (e, p) in enumerate(MEASURED)]
        v = self._verdict(turns)
        self.assertEqual(v["primary_culprit"], "turn_detection")
        self.assertIn("TURN DETECTION", v["headline"])
        self.assertIn("700 ms of silence", v["headline"])
        self.assertIn("a setting, not a slow component", v["headline"])
        self.assertNotIn("parakeet", v["headline"].lower())

    def test_the_same_numbers_without_a_recorded_timeout_still_blame_stt(self):
        # An old capture: the report cannot know, so it says what it used to.
        turns = [_turn(i + 1, e, p, silence_timeout_ms=None)
                 for i, (e, p) in enumerate(MEASURED)]
        self.assertEqual(self._verdict(turns)["primary_culprit"], "stt")

    def test_the_kind_is_unchanged_so_dashboards_keep_working(self):
        turns = [_turn(1, 744.0, 2197.0)]
        kinds = {a["kind"] for a in _graded(turns)}
        self.assertIn("stt.endpointing_high", kinds)


class HeadlineCountTests(unittest.TestCase):
    def test_the_headline_counts_what_was_felt_and_names_the_rest(self):
        turns = [_turn(i + 1, e, p) for i, (e, p) in enumerate(MEASURED)]
        anoms = _graded(turns)
        # Eight rows, as the session reported: four felt, four diagnostic.
        self.assertEqual(len(anoms), 8)
        v = _compute_verdict(turns, [], anoms, {}, model="v2")
        self.assertIn("4 anomaly flag(s)", v["headline"])
        self.assertIn("plus 4 diagnostic", v["headline"])
        self.assertNotIn("8 anomaly flag(s)", v["headline"])

    def test_nothing_is_appended_when_every_flag_was_felt(self):
        turns = [_turn(1, 1400.0, 300.0)]
        v = _compute_verdict(turns, [], _graded(turns), {}, model="v2")
        self.assertNotIn("diagnostic", v["headline"])

    def test_v1_counts_every_flag_as_it_always_has(self):
        turns = [_turn(i + 1, e, p) for i, (e, p) in enumerate(MEASURED)]
        v = _compute_verdict(turns, [], _graded(turns), {}, model="v1")
        self.assertIn("8 anomaly flag(s)", v["headline"])
        self.assertNotIn("diagnostic", v["headline"])


class TurnDetectionMetadataTests(unittest.TestCase):
    def test_what_the_engine_writes_is_read(self):
        self.assertEqual(
            _coerce_turn_detection({"mode": "silence", "silence_timeout_ms": 700}),
            {"mode": "silence", "silence_timeout_ms": 700.0},
        )

    def test_a_malformed_block_is_no_block(self):
        for bad in (None, "silence", {}, {"silence_timeout_ms": "700"},
                    {"silence_timeout_ms": -1}, {"silence_timeout_ms": True}):
            with self.subTest(raw=bad):
                self.assertIsNone(_coerce_turn_detection(bad))


if __name__ == "__main__":
    unittest.main()
