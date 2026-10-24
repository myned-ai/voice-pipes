"""Regression tests for `_detect_anomalies` and `_compute_verdict` (v1).

These pin the v1 (binary) health model explicitly (``model="v1"``) so they stay
valid regardless of the ``VP_HEALTH_MODEL`` default (now v2). See
test_verdict_graded.py for the v2 graded semantics.

Uses only stdlib `unittest` so the suite runs without adding a test
dependency to `requirements.txt`. Run with::

    python -m unittest backend.app.test_anomalies

The fixture mirrors the live-openai session captured on 2026-05-14
(session_id 6e5cb3eb-…) which surfaced the false-positive
`llm.throughput_low` and `tts.ttfb_high` flags this module guards
against.
"""

import unittest

from .report.analysis import _component_aggregates, _compute_verdict, _detect_anomalies

# Default targets — kept inline so a target-table change can't silently
# break the test's intent.
TARGETS = {
    "ttft_ms": 800,
    "ttfb_ms": 400,
    "endpointing_ms": 500,
    "stt_processing_ms": 800,
    "draining_handshake_ms": 1500,
    "rtf_max": 0.6,
    "tokens_per_sec_min": 20,
    "barge_in_rtt_ms": 400,
    "self_barge_played_ms_max": 200,
    "outlier_total_ms_factor": 2.0,
}


def _turn(
    utt: int,
    *,
    ttft_ms: float,
    ttfb_ms: float | None,
    completion_tokens: int,
    tokens_per_sec: float,
    generation_ms: float,
    cold_start: bool = False,
    interrupted: bool = False,
    synthesis_ms: float | None = 500.0,
    bi_rtt_ms: float | None = None,
    rtf: float = 0.15,
    tts_cold_start: bool | None = None,
) -> dict:
    """Build a minimal turn dict matching the synthesizer's expected shape."""
    return {
        "utt": utt,
        "interrupted": interrupted,
        "terminated": False,
        "total_ms": int(ttft_ms + (synthesis_ms or 0)),
        "stt": {"mode": "streaming", "endpointing_ms": 10, "processing_ms": None},
        "llm": {
            "ttft_ms": ttft_ms,
            "generation_ms": generation_ms,
            "completion_tokens": completion_tokens,
            "tokens_per_sec": tokens_per_sec,
            "cold_start": cold_start,
        },
        "tts": {
            "ttfb_ms": ttfb_ms,
            "synthesis_ms": synthesis_ms,
            "rtf": rtf,
            "chars": 80,
            "audio_duration_ms": 4000,
            "cold_start": (tts_cold_start if tts_cold_start is not None else cold_start),
        },
        "bi": (
            {"trigger": "server_vad", "played_ms": 2509, "barge_in_rtt_ms": bi_rtt_ms}
            if bi_rtt_ms is not None
            else {}
        ),
    }


class ThroughputAnomalyTests(unittest.TestCase):
    def test_cold_turn_throughput_not_flagged(self):
        """Cold turns: TLS handshake dominates `generation_ms`, so a low
        `tokens_per_sec` is an artifact, not a signal."""
        turns = [_turn(0, ttft_ms=1375, ttfb_ms=147,
                       completion_tokens=9, tokens_per_sec=5.6,
                       generation_ms=1600, cold_start=True)]
        kinds = {a["kind"] for a in _detect_anomalies(turns, TARGETS)}
        self.assertNotIn("llm.throughput_low", kinds)

    def test_short_output_throughput_not_flagged(self):
        """Short outputs (<30 completion tokens): per-request startup
        overhead swamps per-token decode rate."""
        turns = [_turn(1, ttft_ms=647, ttfb_ms=135,
                       completion_tokens=19, tokens_per_sec=16.7,
                       generation_ms=1140)]
        kinds = {a["kind"] for a in _detect_anomalies(turns, TARGETS)}
        self.assertNotIn("llm.throughput_low", kinds)

    def test_long_output_low_throughput_still_flagged(self):
        """A long, slow generation must still flag — the heuristic must
        not silently mask real throughput regressions."""
        turns = [_turn(2, ttft_ms=500, ttfb_ms=150,
                       completion_tokens=200, tokens_per_sec=10.0,
                       generation_ms=20000)]
        kinds = {a["kind"] for a in _detect_anomalies(turns, TARGETS)}
        self.assertIn("llm.throughput_low", kinds)


class TtsTtfbAnomalyTests(unittest.TestCase):
    def test_interrupted_turn_ttfb_not_flagged(self):
        """Interrupted turns have `synthesis_ms == None` because
        synthesis was cut short; `ttfb_ms` may include
        interrupt/cancellation stalls and isn't comparable."""
        turns = [_turn(1, ttft_ms=647, ttfb_ms=787,
                       completion_tokens=19, tokens_per_sec=16.7,
                       generation_ms=1140, interrupted=True,
                       synthesis_ms=None)]
        kinds = {a["kind"] for a in _detect_anomalies(turns, TARGETS)}
        self.assertNotIn("tts.ttfb_high", kinds)

    def test_clean_turn_high_ttfb_still_flagged(self):
        """A high ttfb on a clean turn (synthesis completed) is real
        signal and must still flag."""
        turns = [_turn(2, ttft_ms=500, ttfb_ms=900,
                       completion_tokens=50, tokens_per_sec=30.0,
                       generation_ms=1500)]
        kinds = {a["kind"] for a in _detect_anomalies(turns, TARGETS)}
        self.assertIn("tts.ttfb_high", kinds)


class TtsRtfAnomalyTests(unittest.TestCase):
    """RTF on the cold-start turn pays a one-time model warm-up cost
    (e.g. rtf 1.88 cold vs 0.49 warm in one session). It must be
    surfaced as `info`, not `warn`, so a warm-up blip cannot pin the
    verdict / primary_culprit. Mirrors the cold-start exemption the
    latency metrics already get."""

    def test_cold_turn_high_rtf_demoted_to_info(self):
        turns = [_turn(0, ttft_ms=600, ttfb_ms=None,
                       completion_tokens=11, tokens_per_sec=14.3,
                       generation_ms=768, cold_start=True,
                       interrupted=True, synthesis_ms=None, rtf=1.88)]
        anoms = _detect_anomalies(turns, TARGETS)
        rtf_anoms = [a for a in anoms if a["kind"] == "tts.rtf_high"]
        self.assertEqual(len(rtf_anoms), 1)
        self.assertEqual(rtf_anoms[0]["level"], "info")

    def test_cold_turn_high_rtf_does_not_pin_verdict(self):
        """Regression for session 98b0c5f5: a single cold-start RTF blip
        must not, by itself, make TTS the primary culprit."""
        turns = [_turn(0, ttft_ms=600, ttfb_ms=None,
                       completion_tokens=11, tokens_per_sec=14.3,
                       generation_ms=768, cold_start=True,
                       interrupted=True, synthesis_ms=None, rtf=1.88)]
        anoms = _detect_anomalies(turns, TARGETS)
        verdict = _compute_verdict(turns, [], anoms, providers={}, model="v1")
        self.assertEqual(verdict["primary_culprit"], "none")
        self.assertEqual(verdict["level"], "healthy")

    def test_warm_turn_high_rtf_still_warns(self):
        """A high RTF on a warm turn is a genuine synthesis-speed defect
        and must still flag at warn level."""
        turns = [_turn(2, ttft_ms=500, ttfb_ms=150,
                       completion_tokens=50, tokens_per_sec=30.0,
                       generation_ms=1500, rtf=1.5)]
        rtf_anoms = [a for a in _detect_anomalies(turns, TARGETS)
                     if a["kind"] == "tts.rtf_high"]
        self.assertEqual(len(rtf_anoms), 1)
        self.assertEqual(rtf_anoms[0]["level"], "warn")


class CulpritTieBreakTests(unittest.TestCase):
    """On an equal warn count, the culprit is the component that breached
    its target by the wider margin — not whichever anomaly was appended
    first. Regression for session 98b0c5f5: a 1.1x LLM TTFT overage must
    not outrank a 3.5x TTS TTFB breach just because TTFT is detected
    first."""

    def test_wider_overage_component_wins_tie(self):
        # ttft 885/800 = 1.11x; ttfb 1394/400 = 3.49x. Count ties 1:1 and
        # llm.ttft_high is detected first, so insertion order alone would
        # pick "llm"; the overage tie-break must pick "tts".
        turns = [_turn(0, ttft_ms=885, ttfb_ms=1394,
                       completion_tokens=16, tokens_per_sec=15.4,
                       generation_ms=1041)]
        anoms = _detect_anomalies(turns, TARGETS)
        self.assertEqual(
            sorted(a["kind"] for a in anoms),
            ["llm.ttft_high", "tts.ttfb_high"],
        )
        verdict = _compute_verdict(turns, [], anoms, providers={}, model="v1")
        self.assertEqual(verdict["primary_culprit"], "tts")

    def test_tie_break_is_symmetric(self):
        # Inverse: ttft 2400/800 = 3.0x dwarfs ttfb 520/400 = 1.3x → llm.
        turns = [_turn(0, ttft_ms=2400, ttfb_ms=520,
                       completion_tokens=16, tokens_per_sec=15.4,
                       generation_ms=1041)]
        anoms = _detect_anomalies(turns, TARGETS)
        verdict = _compute_verdict(turns, [], anoms, providers={}, model="v1")
        self.assertEqual(verdict["primary_culprit"], "llm")


class LiveOpenAiSessionFixtureTest(unittest.TestCase):
    """End-to-end replay of the 2026-05-14 live-openai report.

    Pre-fix: 6 anomalies, primary_culprit = "llm" (false positive).
    Post-fix: 2 anomalies (utt 3 ttft marginally over, utt 1 bi rtt high),
    primary_culprit attributed to transport, not LLM.
    """

    def test_replay(self):
        turns = [
            _turn(0, ttft_ms=1375, ttfb_ms=147, completion_tokens=9,
                  tokens_per_sec=5.6, generation_ms=1600, cold_start=True),
            _turn(1, ttft_ms=647, ttfb_ms=787, completion_tokens=19,
                  tokens_per_sec=16.7, generation_ms=1140,
                  interrupted=True, synthesis_ms=None, bi_rtt_ms=503),
            _turn(2, ttft_ms=436, ttfb_ms=135, completion_tokens=17,
                  tokens_per_sec=24.0, generation_ms=709),
            _turn(3, ttft_ms=803, ttfb_ms=127, completion_tokens=23,
                  tokens_per_sec=15.5, generation_ms=1484),
        ]
        anomalies = _detect_anomalies(turns, TARGETS)
        kinds = sorted(a["kind"] for a in anomalies)
        # Expect only the two genuine signals to survive.
        self.assertEqual(kinds, ["bi.rtt_high", "llm.ttft_high"])

        verdict = _compute_verdict(turns, [], anomalies, providers={}, model="v1")
        # LLM must not be the primary culprit when its only flag is a
        # 0.4% TTFT overage and the dominant warn is a transport-side
        # barge-in RTT.
        self.assertNotEqual(verdict.get("primary_culprit"), "llm")


class OverlapAttributionTests(unittest.TestCase):
    """Regression net for session 049589c7 (the false "broken — STT" verdict).

    Two metrics overlap other stages and must be graded on their exclusive
    on-path share, not the raw value:
    - batch STT `processing_ms` spans the whole utterance and the commit is
      gated on the final transcript, so with `endpointing_ms` present the
      post-endpointing STT share is 0;
    - canonical `ttfb_ms` is commit-anchored (contains LLM TTFT) whenever the
      `internal_ttfb_ms` marker is present, so the TTS share is ttfb - ttft.
    v1 `level` stays warn in both cases — only the v2 grading fields change.
    """

    def test_batch_stt_processing_with_endpointing_is_off_path(self):
        # 049589c7 turn 3: processing 10284ms on a ~10s utterance used to be
        # graded severe/on-path and single-handedly pin the session to broken.
        t = _turn(3, ttft_ms=369, ttfb_ms=482, completion_tokens=21,
                  tokens_per_sec=40.7, generation_ms=516)
        t["stt"] = {"mode": "batch", "endpointing_ms": 1211.0,
                    "processing_ms": 10284.0, "rtfx": 2.07}
        anoms = _detect_anomalies([t], TARGETS)
        a = next(x for x in anoms if x["kind"] == "stt.processing_high")
        self.assertEqual(a["level"], "warn")              # v1 unchanged
        self.assertIs(a["on_critical_path"], False)
        self.assertEqual(a["on_path_observed"], 0.0)
        self.assertEqual(a["severity"], "negligible")

    def test_commit_anchored_ttfb_does_not_echo_llm_stall(self):
        # 049589c7 turn 4: ttfb 1284 contains ttft 1130 (an LLM connect
        # stall). The TTS-side share is 154ms — the same stall must not be
        # counted twice as a second "severe" breach against TTS.
        t = _turn(4, ttft_ms=1130, ttfb_ms=1284, completion_tokens=17,
                  tokens_per_sec=13.8, generation_ms=1233)
        t["tts"]["internal_ttfb_ms"] = 217.0
        anoms = _detect_anomalies([t], TARGETS)
        a = next(x for x in anoms if x["kind"] == "tts.ttfb_high")
        self.assertEqual(a["level"], "warn")              # v1 unchanged
        self.assertIs(a["on_critical_path"], False)
        self.assertEqual(a["on_path_observed"], 154.0)
        self.assertEqual(a["severity"], "negligible")

    def test_genuinely_slow_tts_still_graded_on_path(self):
        # Guard against over-correcting: when the post-first-token share
        # itself breaches the target, TTS keeps the blame.
        t = _turn(2, ttft_ms=200, ttfb_ms=1400, completion_tokens=50,
                  tokens_per_sec=30.0, generation_ms=1500)
        t["tts"]["internal_ttfb_ms"] = 1100.0
        anoms = _detect_anomalies([t], TARGETS)
        a = next(x for x in anoms if x["kind"] == "tts.ttfb_high")
        self.assertIs(a["on_critical_path"], True)
        self.assertEqual(a["on_path_observed"], 1200.0)   # 1400 - 200
        self.assertEqual(a["severity"], "severe")

    def test_legacy_ttfb_without_marker_keeps_raw_grading(self):
        # No internal_ttfb_ms → anchor unknown → grade on the raw value.
        t = _turn(2, ttft_ms=500, ttfb_ms=900, completion_tokens=50,
                  tokens_per_sec=30.0, generation_ms=1500)
        anoms = _detect_anomalies([t], TARGETS)
        a = next(x for x in anoms if x["kind"] == "tts.ttfb_high")
        self.assertNotIn("on_path_observed", a)
        self.assertEqual(a["severity"], "moderate")       # 900/400 = 2.25x


class BargeInRttAnomalyTests(unittest.TestCase):
    def _turn_with_bi(
        self,
        *,
        bi_rtt_ms: float,
        played_ms: float,
    ) -> dict:
        # Like `_turn` but lets the test override `played_ms` (the
        # canned helper hardcodes 2509ms which is always "real user
        # barge-in" for the demotion heuristic).
        t = _turn(0, ttft_ms=400, ttfb_ms=200, completion_tokens=50,
                  tokens_per_sec=40.0, generation_ms=1200,
                  bi_rtt_ms=bi_rtt_ms)
        t["bi"]["played_ms"] = played_ms
        return t

    def test_bi_rtt_high_real_user_barge_marginal_demoted(self):
        """played_ms >> threshold AND <=1.5x target → info, not warn.
        A real user barge-in mid-utterance with a small RTT overage is
        within network jitter and should not pin transport as the
        primary culprit."""
        turns = [self._turn_with_bi(bi_rtt_ms=500, played_ms=3000)]
        anomalies = _detect_anomalies(turns, TARGETS)
        bi = next(a for a in anomalies if a["kind"] == "bi.rtt_high")
        self.assertEqual(bi["level"], "info")

    def test_bi_rtt_high_short_played_still_warn(self):
        """Low played_ms → likely self-barge / spurious VAD trigger;
        the RTT overage represents a real bug surface and must warn."""
        turns = [self._turn_with_bi(bi_rtt_ms=500, played_ms=300)]
        anomalies = _detect_anomalies(turns, TARGETS)
        bi = next(a for a in anomalies if a["kind"] == "bi.rtt_high")
        self.assertEqual(bi["level"], "warn")

    def test_bi_rtt_high_huge_overage_still_warn(self):
        """Even with a real user barge-in, >1.5x target is a real
        regression (user heard hundreds of ms of stale TTS) → warn."""
        turns = [self._turn_with_bi(bi_rtt_ms=2000, played_ms=3000)]
        anomalies = _detect_anomalies(turns, TARGETS)
        bi = next(a for a in anomalies if a["kind"] == "bi.rtt_high")
        self.assertEqual(bi["level"], "warn")


class LlmTtftAnomalyTests(unittest.TestCase):
    def test_ttft_marginal_overage_demoted(self):
        """<=5% over target = within normal cloud jitter → info."""
        turns = [_turn(1, ttft_ms=820, ttfb_ms=150, completion_tokens=50,
                       tokens_per_sec=40.0, generation_ms=1200)]
        anomalies = _detect_anomalies(turns, TARGETS)
        ttft = next(a for a in anomalies if a["kind"] == "llm.ttft_high")
        self.assertEqual(ttft["level"], "info")

    def test_ttft_connect_dominated_demoted(self):
        """When connect_ms / first_token_ms > 0.9 the model wasn't
        slow — TLS handshake was. Demote to info and avoid blaming
        the LLM for a transport hiccup."""
        turns = [_turn(1, ttft_ms=1260, ttfb_ms=150, completion_tokens=50,
                       tokens_per_sec=40.0, generation_ms=1500)]
        # Inject the cloud-cold-socket signal directly.
        turns[0]["llm"]["connect_ms"] = 1255.0
        turns[0]["llm"]["first_token_ms"] = 1260.0
        anomalies = _detect_anomalies(turns, TARGETS)
        ttft = next(a for a in anomalies if a["kind"] == "llm.ttft_high")
        self.assertEqual(ttft["level"], "info")
        self.assertIn("connect_ms", ttft.get("detail", ""))

    def test_ttft_genuine_slow_still_warn(self):
        """Substantial overage with no transport explanation → warn,
        so genuine LLM regressions still surface."""
        turns = [_turn(1, ttft_ms=1500, ttfb_ms=150, completion_tokens=50,
                       tokens_per_sec=40.0, generation_ms=1500)]
        anomalies = _detect_anomalies(turns, TARGETS)
        ttft = next(a for a in anomalies if a["kind"] == "llm.ttft_high")
        self.assertEqual(ttft["level"], "warn")


class TtsStabilityIgnoresInterruptedTests(unittest.TestCase):
    """`_component_aggregates` must skip interrupted turns when
    bucketing TTS ttfb_ms — otherwise a single inflated ttfb on an
    aborted turn poisons p99/p50 and falsely flags TTS as `unstable`.
    """

    def test_interrupted_ttfb_excluded_from_tts_distribution(self):
        turns = [
            _turn(0, ttft_ms=400, ttfb_ms=120, completion_tokens=40,
                  tokens_per_sec=40.0, generation_ms=1000),
            _turn(1, ttft_ms=420, ttfb_ms=130, completion_tokens=42,
                  tokens_per_sec=40.0, generation_ms=1050),
            # Outlier on an interrupted turn — must NOT influence
            # the TTS stability classification.
            _turn(2, ttft_ms=430, ttfb_ms=4000, completion_tokens=45,
                  tokens_per_sec=40.0, generation_ms=1100,
                  interrupted=True, synthesis_ms=None),
        ]
        agg = _component_aggregates(turns)
        tts = next((c for c in agg if c["kind"] == "tts"), None)
        self.assertIsNotNone(tts)
        # With the outlier excluded, only two clean ttfb samples remain
        # (120, 130) → stable.  Without the fix this would be unstable
        # because 4000 / median ≫ 5.0.
        self.assertEqual(tts["count"], 2)
        self.assertEqual(tts["stability"], "stable")


class LlmStabilityIgnoresConnectDominatedTests(unittest.TestCase):
    """`_component_aggregates` must skip LLM ttft_ms when a cold TLS
    handshake dominated the measurement (`connect_ms / first_token_ms
    > 0.9`) — that latency belongs to the cloud network path, not the
    model. Including such turns inflates p99/p50 and pins LLM as
    `unstable` even when every model-side first-token came back
    promptly.
    """

    def test_connect_dominated_turn_excluded_from_llm_distribution(self):
        turns = []
        # Five clean turns: ttft 600-700ms, connect << first_token.
        for i, ttft in enumerate([600.0, 620.0, 640.0, 660.0, 680.0]):
            t = _turn(i, ttft_ms=ttft, ttfb_ms=150,
                      completion_tokens=40, tokens_per_sec=40.0,
                      generation_ms=1000)
            t["llm"]["connect_ms"] = 50.0  # 50/600 = 8% — not dominant
            t["llm"]["first_token_ms"] = ttft
            turns.append(t)
        # One cold-socket outlier: ttft 7150ms but connect 7095 (99%).
        t = _turn(5, ttft_ms=7150.0, ttfb_ms=150,
                  completion_tokens=10, tokens_per_sec=10.0,
                  generation_ms=1500)
        t["llm"]["connect_ms"] = 7095.0
        t["llm"]["first_token_ms"] = 7150.0
        turns.append(t)

        agg = _component_aggregates(turns)
        llm = next((c for c in agg if c["kind"] == "llm"), None)
        self.assertIsNotNone(llm)
        # Outlier excluded → only the 5 clean samples count, max=680.
        self.assertEqual(llm["count"], 5)
        self.assertEqual(llm["max"], 680.0)
        self.assertEqual(llm["stability"], "stable")
        self.assertFalse(llm["unstable"])


class VerdictUnstableAloneIsHealthyTests(unittest.TestCase):
    """A high p99/p50 ratio with zero warn-level anomalies must NOT
    escalate the verdict to `degraded` — jitter alone isn't a
    user-visible defect when no individual turn breached its target.
    The `unstable_components` field still surfaces the jitter as a
    note in the headline.
    """

    def test_unstable_only_no_warn_anoms_stays_healthy(self):
        # Synthetic aggregates: LLM unstable, no anomalies.
        aggregates = [
            {"kind": "llm", "unstable": True, "count": 8},
            {"kind": "tts", "unstable": False, "count": 8},
        ]
        verdict = _compute_verdict(turns=[{}] * 8, aggregates=aggregates,
                                   anomalies=[], providers={}, model="v1")
        self.assertEqual(verdict["level"], "healthy")
        self.assertEqual(verdict["primary_culprit"], "none")
        self.assertIn("llm", verdict["unstable_components"])
        # Headline must still mention the jitter so reviewers see it.
        self.assertIn("jittery", verdict["headline"].lower())

    def test_unstable_with_warn_anoms_still_degraded(self):
        # When warn-level signals exist, instability still escalates.
        aggregates = [{"kind": "llm", "unstable": True, "count": 8}]
        anoms = [{"kind": "llm.ttft_high", "level": "warn",
                  "observed": 1500, "target": 800, "detail": ""}]
        verdict = _compute_verdict(turns=[{}] * 8, aggregates=aggregates,
                                   anomalies=anoms, providers={}, model="v1")
        self.assertEqual(verdict["level"], "degraded")
        self.assertEqual(verdict["primary_culprit"], "llm")


if __name__ == "__main__":
    unittest.main()
