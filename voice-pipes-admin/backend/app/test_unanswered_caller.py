"""Health rules for the caller who spoke and got nothing.

A session's recogniser produced words for an utterance, returned nothing on
its final pass, and the pipeline discarded the turn. The caller waited
thirteen seconds and repeated themselves. The report graded the session
"fair" for a 700 ms silence timeout: there was no turn to be slow, and the
thirteen seconds appeared only as an ungraded `e2e_perceived_ms` on the turn
that finally answered.

Two rules now see it. `stt.utterance_dropped` reads the `utterance_dropped`
event the engine records at the discard. `turn.unanswered_wait` compares the
wait the caller's device measured with the wait the server timed, so a lost
first attempt is caught even from a capture with no dropped-utterance event.
"""

import unittest

from .report.analysis import _compute_verdict, _detect_anomalies

TARGETS = {
    "ttft_ms": 300, "ttfb_ms": 400, "endpointing_ms": 500, "stt_processing_ms": 800,
    "draining_handshake_ms": 1500, "rtf_max": 0.6, "tokens_per_sec_min": 18,
    "barge_in_rtt_ms": 400, "self_barge_played_ms_max": 200, "outlier_total_ms_factor": 2.0,
    "unanswered_wait_ms": 3000,
}


def _turn(utt=1, *, felt=1000.0, timed=900.0, heard=1500.0, dropped=None, estimated=False):
    t = {
        "utt": utt, "initiator": "user", "interrupted": False, "terminated": False,
        "stt": {"mode": "batch", "endpointing_ms": 450.0, "processing_ms": 300.0,
                "audio_duration_ms": heard, "silence_timeout_ms": 400.0},
        "llm": {"ttft_ms": 80.0, "tokens_per_sec": 90.0, "generation_ms": 200.0},
        "tts": {"ttfb_ms": 150.0, "rtf": 0.05, "chars": 40, "audio_duration_ms": 2000.0},
        "total_ms": 300.0,
        "e2e_perceived_ms": felt, "e2e_estimated": estimated,
        "critical_path": {"total_ms": timed},
        "user_transcript": "I said no, it doesn't.",
        "agent_transcript": "Hmm, okay.",
    }
    if dropped is not None:
        t["dropped_before"] = dropped
    return t


def _kinds(turns):
    return [(a["utt"], a["kind"], a["severity"]) for a in _detect_anomalies(turns, TARGETS)]


class DroppedUtteranceTest(unittest.TestCase):
    def test_words_produced_then_lost_is_the_recognisers_fault_and_severe(self):
        turns = [_turn(dropped=[{"reason": "empty_transcript", "text": "", "had_interim_text": True, "audio_ms": 1400}])]
        self.assertIn((1, "stt.utterance_dropped", "severe"), _kinds(turns))
        v = _compute_verdict(turns, [], _detect_anomalies(turns, TARGETS), {}, model="v2")
        self.assertEqual(v["level"], "degraded")
        self.assertEqual(v["primary_culprit"], "stt")

    def test_a_sound_with_no_words_in_it_is_a_note_not_a_fault(self):
        turns = [_turn(dropped=[{"reason": "empty_transcript", "text": "", "had_interim_text": False, "audio_ms": 900}])]
        kinds = _kinds(turns)
        self.assertIn((1, "vad.non_speech_forwarded", "negligible"), kinds)
        self.assertNotIn("stt.utterance_dropped", [k for _, k, _ in kinds])
        v = _compute_verdict(turns, [], _detect_anomalies(turns, TARGETS), {}, model="v2")
        self.assertEqual(v["level"], "healthy")

    def test_a_short_transcript_thrown_away_is_named_but_negligible(self):
        turns = [_turn(dropped=[{"reason": "too_short", "text": "Hm", "had_interim_text": False, "audio_ms": 400}])]
        self.assertIn((1, "stt.utterance_dropped", "negligible"), _kinds(turns))


class UnansweredWaitTest(unittest.TestCase):
    def test_a_wait_far_beyond_what_the_server_timed_is_severe(self):
        # The thirteen-second turn: the server timed under a second of it.
        turns = [_turn(felt=12906.0, timed=968.0, heard=2060.0)]
        self.assertIn((1, "turn.unanswered_wait", "severe"), _kinds(turns))
        v = _compute_verdict(turns, [], _detect_anomalies(turns, TARGETS), {}, model="v2")
        self.assertEqual(v["level"], "degraded")

    def test_one_lost_turn_outranks_several_slow_timeouts_as_the_culprit(self):
        # Three turns over the endpointing target (moderate each) and one lost
        # turn (severe): the headline must name the lost turn's owner.
        slow = {"felt": 1100.0, "timed": 1000.0, "heard": 1500.0}
        turns = [_turn(1, **slow), _turn(2, **slow), _turn(3, **slow),
                 _turn(4, felt=12906.0, timed=968.0, heard=2060.0)]
        for t in turns:
            t["stt"]["endpointing_ms"] = 760.0
            t["stt"]["silence_timeout_ms"] = 700.0
        v = _compute_verdict(turns, [], _detect_anomalies(turns, TARGETS), {}, model="v2")
        self.assertEqual(v["level"], "degraded")
        self.assertEqual(v["primary_culprit"], "stt")

    def test_a_wait_spanning_wordless_drops_is_named_and_graded_moderate(self):
        # Session 491bfc0f: two sounds the recogniser found no words in, then
        # the turn. The same drops were graded negligible notes while the
        # wait they explain was graded a severe recogniser fault.
        drops = [
            {"reason": "empty_transcript", "text": "", "had_interim_text": False, "audio_ms": 1377},
            {"reason": "empty_transcript", "text": "", "had_interim_text": False, "audio_ms": 1597},
        ]
        turns = [_turn(2, felt=17018.0, timed=1245.0, heard=1640.0, dropped=drops)]
        found = [a for a in _detect_anomalies(turns, TARGETS) if a["kind"] == "turn.unanswered_wait"]
        self.assertEqual(len(found), 1)
        self.assertEqual(found[0]["severity"], "moderate")
        self.assertEqual(found[0]["level"], "warn")
        self.assertIn("2 sound(s)", found[0]["detail"])
        self.assertIn("1.4 s, 1.6 s", found[0]["detail"])
        self.assertIn("dropped recordings", found[0]["detail"])

    def test_a_wait_spanning_a_drop_that_had_words_stays_severe(self):
        drops = [{"reason": "empty_transcript", "text": "", "had_interim_text": True, "audio_ms": 1400}]
        turns = [_turn(2, felt=17018.0, timed=1245.0, heard=1640.0, dropped=drops)]
        found = [a for a in _detect_anomalies(turns, TARGETS) if a["kind"] == "turn.unanswered_wait"]
        self.assertEqual(found[0]["severity"], "severe")
        self.assertIn("produced words", found[0]["detail"])

    def test_speech_the_client_took_for_a_pause_is_not_a_wait(self):
        # The client's detector ended the utterance at a pause; the caller
        # went on for another ten seconds and the recogniser heard all of it.
        turns = [_turn(felt=7662.0, timed=1037.0, heard=11400.0)]
        self.assertNotIn("turn.unanswered_wait", [k for _, k, _ in _kinds(turns)])

    def test_an_estimated_wait_cannot_raise_it(self):
        # Without a client mark the felt wait is the server's own estimate,
        # so the two figures are the same measurement and prove nothing.
        turns = [_turn(felt=12906.0, timed=968.0, heard=2060.0, estimated=True)]
        self.assertNotIn("turn.unanswered_wait", [k for _, k, _ in _kinds(turns)])

    def test_an_ordinary_turn_is_left_alone(self):
        self.assertNotIn("turn.unanswered_wait", [k for _, k, _ in _kinds([_turn()])])


class ToolInvocationWaitTest(unittest.TestCase):
    """Session eeae8199: a clean request for a song, then 21.8 s of silence
    before the model dispatched `play_media`. The invocation turn has no
    spoken reply and so no `e2e_perceived_ms`; the only rule that looked
    blamed the client tool that was still waiting to be resolved."""

    @staticmethod
    def _invocation(utt, *, speech_end, called, name="play_media"):
        t = _turn(utt, felt=None, timed=None, heard=1258.0)
        t["kind"] = "tool_invocation"
        t["e2e_perceived_ms"] = None
        t["critical_path"] = {"total_ms": None}
        t["tts"] = {"ttfb_ms": None, "audio_duration_ms": 0.0}
        t["agent_transcript"] = None
        t["anchor"] = {"client_speech_end_epoch_ms": speech_end}
        call = {"call_id": "call_1", "name": name, "status": "pending", "speak_on_result": True,
                "called_at_ms": called, "after_utterance_id": utt}
        return t, call

    def test_a_long_silence_before_the_tool_call_is_the_models_unanswered_wait(self):
        t, call = self._invocation(6, speech_end=1_791_629_729_113.0, called=1_791_629_750_900.0)
        found = [a for a in _detect_anomalies([t], TARGETS, [call]) if a["kind"] == "turn.unanswered_wait"]
        self.assertEqual(len(found), 1)
        self.assertEqual(found[0]["utt"], 6)
        self.assertEqual(found[0]["severity"], "severe")
        self.assertEqual(found[0]["component"], "llm")
        self.assertIn("21.8 s", found[0]["detail"])
        self.assertIn("play_media", found[0]["detail"])
        # On a speech-to-speech pipeline the blame is the model's.
        v = _compute_verdict([t], [], _detect_anomalies([t], TARGETS, [call]), {"s2s": True}, model="v2")
        self.assertEqual(v["level"], "degraded")
        self.assertEqual(v["primary_culprit"], "s2s")

    def test_a_prompt_tool_call_raises_nothing(self):
        t, call = self._invocation(6, speech_end=100_000.0, called=101_900.0)
        kinds = [a["kind"] for a in _detect_anomalies([t], TARGETS, [call])]
        self.assertNotIn("turn.unanswered_wait", kinds)

    def test_without_a_speech_end_anchor_nothing_is_measured(self):
        t, call = self._invocation(6, speech_end=100_000.0, called=130_000.0)
        t["anchor"] = {}
        kinds = [a["kind"] for a in _detect_anomalies([t], TARGETS, [call])]
        self.assertNotIn("turn.unanswered_wait", kinds)


class ToolFollowupWaitTest(unittest.TestCase):
    """A spoken tool follow-up waits for the client's tool round-trip. That
    wait is application time the report already excludes from the turn's
    latency; it must not read as a lost first attempt to speak."""

    def _followup(self, *, felt, timed, heard, excluded):
        t = _turn(2, felt=felt, timed=timed, heard=heard)
        t["kind"] = "tool_followup"
        t["tool_context"] = {"invocation_utt": 1, "tool_roundtrip_ms": excluded, "excluded_wait_ms": excluded}
        return t

    def test_the_tool_round_trip_is_not_an_unanswered_wait(self):
        # "Can you play I Wish You Were Here?": 7.2 s felt, 5.9 s of it the
        # client starting the media, 1.6 s timed by the server.
        turns = [self._followup(felt=7246.0, timed=1613.0, heard=1258.0, excluded=5900.0)]
        self.assertNotIn("turn.unanswered_wait", [k for _, k, _ in _kinds(turns)])

    def test_a_lost_attempt_beyond_the_tool_wait_is_still_caught(self):
        turns = [self._followup(felt=14000.0, timed=1600.0, heard=1200.0, excluded=5900.0)]
        self.assertIn((2, "turn.unanswered_wait", "severe"), _kinds(turns))


class InterruptionUnheardTest(unittest.TestCase):
    """The caller talks over the agent, the server VAD cuts the reply, and
    the recogniser finds no words in the interrupting speech. Session
    95d5cda5 (Nemotron 3.5, 2026-10-10): the agent fell silent and the
    report called the speech a sound with no words in it, which graded as
    the system working. The VAD had confirmed it as the caller talking, so
    it is the recogniser's miss."""

    @staticmethod
    def _interrupted(utt):
        t = _turn(utt)
        t["interrupted"] = True
        t["bi"] = {"trigger": "server_vad", "played_ms": 2731, "barge_in_rtt_ms": 4}
        return t

    def test_the_first_drop_after_a_barge_in_is_the_unheard_interruption(self):
        drop = {"reason": "empty_transcript", "text": "", "had_interim_text": False, "audio_ms": 1254}
        turns = [self._interrupted(11), _turn(12, dropped=[drop])]
        kinds = _kinds(turns)
        self.assertIn((12, "stt.interruption_unheard", "moderate"), kinds)
        self.assertNotIn("vad.non_speech_forwarded", [k for _, k, _ in kinds])
        v = _compute_verdict(turns, [], _detect_anomalies(turns, TARGETS), {}, model="v2")
        self.assertEqual(v["primary_culprit"], "stt")

    def test_a_second_drop_after_the_same_barge_in_is_ordinary_noise(self):
        drop = {"reason": "empty_transcript", "text": "", "had_interim_text": False, "audio_ms": 1254}
        noise = {"reason": "empty_transcript", "text": "", "had_interim_text": False, "audio_ms": 975}
        turns = [self._interrupted(15), _turn(16, dropped=[drop, noise])]
        kinds = [(u, k) for u, k, _ in _kinds(turns)]
        self.assertEqual(kinds.count((16, "stt.interruption_unheard")), 1)
        self.assertIn((16, "vad.non_speech_forwarded"), kinds)

    def test_a_drop_after_a_client_cancel_or_a_clean_turn_stays_a_note(self):
        drop = {"reason": "empty_transcript", "text": "", "had_interim_text": False, "audio_ms": 1254}
        cancelled = _turn(3)
        cancelled["interrupted"] = True
        cancelled["bi"] = {"trigger": "client_cancel", "played_ms": 900}
        for before in (cancelled, _turn(3)):
            kinds = [k for _, k, _ in _kinds([before, _turn(4, dropped=[drop])])]
            self.assertNotIn("stt.interruption_unheard", kinds)
            self.assertIn("vad.non_speech_forwarded", kinds)


if __name__ == "__main__":
    unittest.main()
