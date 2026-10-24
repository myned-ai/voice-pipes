"""Health rules for turns that carried no words, and for sessions with
nothing to grade.

Every other rule in the engine measures how fast a stage was. None asked
whether what came out meant anything, so a session whose recogniser returned
".." for a spoken sentence graded green on each stage and was reported
"healthy — no anomalies flagged" twice in a row while the conversation was
plainly broken.

Run with::

    python -m unittest backend.app.test_empty_transcript
"""

import unittest

from .report.analysis import _compute_verdict, _detect_anomalies

TARGETS = {
    "ttft_ms": 300,
    "ttfb_ms": 400,
    "endpointing_ms": 500,
    "stt_processing_ms": 800,
    "draining_handshake_ms": 1500,
    "rtf_max": 0.6,
    "tokens_per_sec_min": 18,
    "barge_in_rtt_ms": 400,
    "self_barge_played_ms_max": 200,
    "outlier_total_ms_factor": 2.0,
}


def _turn(transcript, *, utt=0, initiator="user", terminated=False,
          stt_cold=False, llm_cold=False, processing_ms=200.0):
    """A turn whose stages are all comfortably inside target, so anything the
    detector reports comes from the transcript rule rather than a latency."""
    return {
        "utt": utt,
        "initiator": initiator,
        "terminated": terminated,
        "user_transcript": transcript,
        "stt": {
            "mode": "batch",
            "endpointing_ms": 120.0,
            "processing_ms": processing_ms,
            "cold_start": stt_cold,
        },
        "llm": {"ttft_ms": 150.0, "tokens_per_sec": 60.0,
                "completion_tokens": 20, "cold_start": llm_cold},
        "tts": {"ttfb_ms": 250.0, "rtf": 0.1, "cold_start": False},
        "total_ms": 400.0,
    }


def _kinds(anoms):
    return [a["kind"] for a in anoms]


class EmptyTranscriptTests(unittest.TestCase):
    def test_a_turn_of_punctuation_is_an_error(self):
        # The exact shape observed: Parakeet returned ".." for "Hello, how
        # are you?" while every stage timed inside target.
        anoms = _detect_anomalies([_turn("..")], TARGETS)
        self.assertIn("stt.transcript_empty", _kinds(anoms))
        row = next(a for a in anoms if a["kind"] == "stt.transcript_empty")
        self.assertEqual(row["level"], "error")
        self.assertEqual(row["severity"], "severe")
        self.assertEqual(row["component"], "stt")
        self.assertTrue(row["on_critical_path"])

    def test_an_empty_or_whitespace_transcript_is_an_error(self):
        for text in ("", "   ", "\n", "...", "—", "?!"):
            with self.subTest(text=text):
                self.assertIn(
                    "stt.transcript_empty",
                    _kinds(_detect_anomalies([_turn(text)], TARGETS)),
                )

    def test_a_transcript_with_words_is_not_flagged(self):
        for text in ("Hello, how are you?", "yes", "about three", "42"):
            with self.subTest(text=text):
                self.assertNotIn(
                    "stt.transcript_empty",
                    _kinds(_detect_anomalies([_turn(text)], TARGETS)),
                )

    def test_warm_up_does_not_excuse_an_empty_turn(self):
        # Cold start explains a slow turn, never a wordless one, so the rule
        # sits outside the cold-start skip.
        anoms = _detect_anomalies(
            [_turn("..", stt_cold=True, llm_cold=True)], TARGETS
        )
        self.assertIn("stt.transcript_empty", _kinds(anoms))

    def test_agent_turns_and_terminated_turns_are_exempt(self):
        # An agent-initiated turn has no user speech by definition, and a
        # terminated one reports its own cause already.
        self.assertNotIn(
            "stt.transcript_empty",
            _kinds(_detect_anomalies([_turn(None, initiator="agent")], TARGETS)),
        )
        self.assertNotIn(
            "stt.transcript_empty",
            _kinds(_detect_anomalies([_turn("..", terminated=True)], TARGETS)),
        )

    def test_a_missing_transcript_is_not_treated_as_empty(self):
        # `None` means the reader had no transcript field to read, which is
        # absence of evidence; only a transcript that exists and holds no
        # words is a recogniser failure.
        self.assertNotIn(
            "stt.transcript_empty",
            _kinds(_detect_anomalies([_turn(None)], TARGETS)),
        )

    def test_an_empty_turn_sinks_the_verdict(self):
        anoms = _detect_anomalies([_turn("..")], TARGETS)
        for model in ("v1", "v2"):
            with self.subTest(model=model):
                v = _compute_verdict([_turn("..")], [], anoms, {}, model=model)
                self.assertNotEqual(v["level"], "healthy")
                self.assertEqual(v["primary_culprit"], "stt")


class ColdStartIsPerStageTests(unittest.TestCase):
    def test_a_cold_model_no_longer_silences_the_recogniser(self):
        # The recogniser is warm and stalled well past target while the model
        # beside it is on its first turn. Reading only the model's flag hid
        # this; the stall is the recogniser's and must be reported.
        anoms = _detect_anomalies(
            [_turn("hello there", llm_cold=True, stt_cold=False,
                   processing_ms=10426.0)],
            TARGETS,
        )
        self.assertIn("stt.processing_high", _kinds(anoms))

    def test_a_cold_recogniser_is_still_exempt(self):
        anoms = _detect_anomalies(
            [_turn("hello there", llm_cold=True, stt_cold=True,
                   processing_ms=10426.0)],
            TARGETS,
        )
        self.assertNotIn("stt.processing_high", _kinds(anoms))


class NothingToGradeTests(unittest.TestCase):
    def test_turns_without_metrics_are_unknown_not_healthy(self):
        # Observed shape: turn_count 1, `turns` carrying no stage numbers.
        # No rule can fire, so the session used to pass for lack of evidence.
        empty = {
            "utt": 0, "initiator": "user", "terminated": False,
            "stt": {"mode": "batch", "endpointing_ms": None, "processing_ms": None},
            "llm": {"ttft_ms": None, "model": "gemma-3-1b-it"},
            "tts": {"ttfb_ms": None},
            "total_ms": None,
        }
        for model in ("v1", "v2"):
            with self.subTest(model=model):
                v = _compute_verdict([empty], [], [], {}, model=model)
                self.assertEqual(v["level"], "unknown")
                self.assertIn("cannot be graded", v["headline"])

    def test_one_gradeable_turn_is_enough_to_grade_the_session(self):
        good = _turn("hello there")
        v = _compute_verdict([good], [], [], {}, model="v2")
        self.assertEqual(v["level"], "healthy")

    def test_a_session_with_no_turns_at_all_is_left_alone(self):
        # An empty list is a session that never took a turn, not one whose
        # turns lost their metrics; the existing handling stays.
        v = _compute_verdict([], [], [], {}, model="v2")
        self.assertNotEqual(v["level"], "unknown")
