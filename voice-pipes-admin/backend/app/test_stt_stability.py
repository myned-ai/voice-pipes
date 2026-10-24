"""The STT stability series must be something a person actually waits for.

Run with::

    python -m unittest backend.app.test_stt_stability
"""

import unittest

from .report.analysis import _component_aggregates


def _turn(processing_ms, endpointing_ms, mode="batch"):
    return {
        "stt": {"mode": mode, "processing_ms": processing_ms,
                "endpointing_ms": endpointing_ms},
        "llm": {"ttft_ms": 110.0},
        "tts": {"ttfb_ms": 300.0, "synthesis_ms": 150.0},
    }


def _stt_row(turns):
    return next(r for r in _component_aggregates(turns) if r["kind"] == "stt")


class BatchSttStabilityTests(unittest.TestCase):
    def test_a_long_sentence_does_not_make_a_batch_recogniser_unstable(self):
        # Session 92d2a30c, as measured. `processing_ms` contains the user's
        # speech, so turn 4 -- one long sentence -- reads 11.5 s against a
        # 2.2 s median, and the check called the session's response latency
        # unstable. The wait for the commit varied by 59 ms.
        turns = [
            _turn(1705.0, 768.0), _turn(1729.0, 736.0), _turn(2197.0, 744.0),
            _turn(2688.0, 775.0), _turn(11554.0, 716.0),
        ]
        row = _stt_row(turns)
        self.assertEqual(row["primary_metric"], "endpointing_ms")
        self.assertFalse(row["unstable"], f"flagged unstable on {row}")
        self.assertLess(row["p99_p50_ratio"], 1.2)
        # The row describes the metric it names, not the one it replaced.
        self.assertEqual(row["max"], 775.0)

    def test_a_batch_recogniser_whose_commit_wait_really_varies_is_still_flagged(self):
        # The change must not blind the check: a stall in what the person
        # waits for has to surface exactly as before.
        turns = [_turn(2000.0, 700.0) for _ in range(5)] + [_turn(2000.0, 9000.0)]
        row = _stt_row(turns)
        self.assertTrue(row["unstable"], f"a 9 s commit wait went unflagged: {row}")

    def test_a_non_batch_recogniser_is_aggregated_as_before(self):
        turns = [_turn(300.0, 500.0, mode="other") for _ in range(4)]
        self.assertEqual(_stt_row(turns)["primary_metric"], "processing_ms")


if __name__ == "__main__":
    unittest.main()
