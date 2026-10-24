"""Which `speech_end` anchors a turn.

A detector cannot tell a pause from the end of a turn, so a client reports
one `speech_end` per pause. The reader took the first, which is where the
speaker hesitated: on session 92d2a30c turn 4 that measured a reply from the
"uh" in "…the internal bond in, uh, particle boards" and reported 7.7 s for
a turn that took 1.1 s.

Run with::

    python -m unittest backend.app.test_speech_end_anchor
"""

import unittest

from .captures.reader import pick_client_speech_end, stt_speech_end_anchor


class PickClientSpeechEndTests(unittest.TestCase):
    def test_a_single_report_is_taken_as_it_is(self):
        self.assertEqual(pick_client_speech_end([1000.0], 2000.0), 1000.0)

    def test_the_last_report_before_the_reply_wins_over_a_pause(self):
        # The measured shape: a pause at 139.5 s, the real end at 146.1 s,
        # first audio at 147.2 s.
        ends = [139_545.0, 146_100.0]
        self.assertEqual(pick_client_speech_end(ends, 147_207.0), 146_100.0)
        # The wait this yields is the real one...
        self.assertAlmostEqual(147_207.0 - 146_100.0, 1_107.0)
        # ...where the first report gave the figure that was shown.
        self.assertAlmostEqual(147_207.0 - ends[0], 7_662.0)

    def test_an_end_after_the_reply_began_does_not_anchor_it(self):
        # Said over the agent's reply: it belongs to whatever comes next, and
        # using it would make this turn's wait negative.
        self.assertEqual(
            pick_client_speech_end([1000.0, 5000.0], first_audio_ms=2000.0), 1000.0
        )

    def test_order_of_arrival_does_not_matter(self):
        self.assertEqual(pick_client_speech_end([3000.0, 1000.0, 2000.0], 3500.0), 3000.0)

    def test_without_audio_on_record_the_last_report_stands(self):
        self.assertEqual(pick_client_speech_end([1000.0, 2000.0], None), 2000.0)

    def test_when_every_report_follows_the_audio_the_earliest_is_used(self):
        # Nothing anchors the turn properly; the earliest is the least wrong,
        # and the wait comes out negative, which is visibly a fault rather
        # than a plausible wrong number.
        self.assertEqual(pick_client_speech_end([5000.0, 6000.0], 2000.0), 5000.0)

    def test_no_reports_is_no_anchor(self):
        self.assertIsNone(pick_client_speech_end([], 2000.0))
        self.assertIsNone(pick_client_speech_end([None, "x"], 2000.0))  # type: ignore[list-item]


class SttSpeechEndAnchorTests(unittest.TestCase):
    """The server-side fallback, used when the client sent no `speech_end` --
    every turn the user begins by talking over the agent."""

    def test_the_commit_is_moved_back_by_the_endpointing_it_waited(self):
        # Session 47ac5e15 turn 3, a barge-in: commit at 22 542 ms, having
        # waited 717.8 ms of silence. The user stopped at 21 824.2.
        anchor, source = stt_speech_end_anchor(22_542.0, {"endpointing_ms": 717.8})
        self.assertAlmostEqual(anchor, 21_824.2)
        self.assertEqual(source, "stt_speech_end")

    def test_this_makes_a_barge_in_turn_comparable_to_its_neighbours(self):
        # First audio reached the client at 22 876 ms. From the commit that is
        # 334 ms, which is what was reported beside 1.5-1.7 s for every other
        # turn. From the end of speech it is 1 052 ms, in line with them.
        anchor, _ = stt_speech_end_anchor(22_542.0, {"endpointing_ms": 717.8})
        self.assertAlmostEqual(22_876.0 - anchor, 1_051.8)
        self.assertAlmostEqual(22_876.0 - 22_542.0, 334.0)

    def test_without_endpointing_the_commit_stands_and_says_so(self):
        for payload in (None, {}, {"endpointing_ms": None}, {"endpointing_ms": "717"},
                        {"endpointing_ms": 0}, {"endpointing_ms": -5},
                        {"endpointing_ms": True}, {"endpointing_ms": 999_999}):
            with self.subTest(payload=payload):
                anchor, source = stt_speech_end_anchor(22_542.0, payload)
                self.assertEqual(anchor, 22_542.0)
                # The label differs, so a reader can tell a bare commit time
                # from a derived end of speech.
                self.assertEqual(source, "stt_timing")

    def test_no_event_time_is_no_anchor(self):
        self.assertEqual(stt_speech_end_anchor(None, {"endpointing_ms": 700}), (None, "stt_timing"))


if __name__ == "__main__":
    unittest.main()
