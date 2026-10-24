"""Tests for the model-agnostic critical-path-latency attribution.

Covers the "cover all model types" matrix from the health model design, plus
the two double-count fixes that produced false "broken" verdicts:

- STT: `endpointing_ms` is measured at the commit decision and the commit is
  gated on the final transcript (pipeline/stt.rs), so the post-speech STT wait
  is already inside the endpointing stage — `stt_critical_ms` must be 0 when
  endpointing is present. The rtfx overlap bound remains only as the legacy
  fallback (captures without endpointing).
- TTS: the canonical `ttfb_ms` is runner-observed (utterance-commit → first
  audio chunk, transport/serve/turn.rs) and therefore *contains* LLM TTFT.
  When the `internal_ttfb_ms` marker is present, the TTS stage is
  `ttfb - ttft`, not the raw `ttfb`.

Run:: python -m unittest backend.app.test_perceived
"""

import unittest

from .report.analysis import _flatten_turn, _stt_critical_ms, critical_path_latency


class SttCriticalPathTests(unittest.TestCase):
    def test_streaming_stt_contributes_zero(self):
        # Transcript arrives during speech → no post-speech STT wait.
        self.assertEqual(
            _stt_critical_ms({"mode": "streaming", "processing_ms": 5000, "rtfx": 1.0}),
            0.0,
        )

    def test_batch_with_endpointing_contributes_zero(self):
        # The commit is gated on the final transcript and endpointing_ms is
        # measured at commit, so the STT tail is already inside endpointing.
        # Canonical false positive (session 049589c7 turn 3): a ~10s utterance
        # with processing 10284ms / rtfx 2.07 used to charge ~4965ms to the
        # critical path while the observed commit → llm_request gap was ~1ms.
        self.assertEqual(
            _stt_critical_ms(
                {"mode": "batch", "endpointing_ms": 1211.0,
                 "processing_ms": 10284.0, "rtfx": 2.07}
            ),
            0.0,
        )
        # Also zero for a blocking model (rtfx <= 1): the block shows up as a
        # long endpointing stage instead, which still attributes to STT.
        self.assertEqual(
            _stt_critical_ms(
                {"mode": "batch", "endpointing_ms": 2800.0,
                 "processing_ms": 2500.0, "rtfx": 0.8}
            ),
            0.0,
        )

    def test_legacy_incremental_stt_bounded_by_rtfx(self):
        # No endpointing_ms (legacy capture) → fall back to the overlap bound:
        # processing 2226ms overlaps speech; rtfx 4.92 → compute wall ~452ms.
        got = _stt_critical_ms({"mode": "batch", "processing_ms": 2226.0, "rtfx": 4.92})
        self.assertAlmostEqual(got, 2226.0 / 4.92, places=1)
        self.assertLess(got, 500.0)

    def test_legacy_blocking_batch_uses_full_processing(self):
        # No endpointing_ms and rtfx <= 1 (or absent) → the model does not keep
        # up during speech, so the full processing time is on the critical path.
        self.assertEqual(
            _stt_critical_ms({"mode": "batch", "processing_ms": 800.0, "rtfx": 0.9}),
            800.0,
        )
        self.assertEqual(
            _stt_critical_ms({"mode": "batch", "processing_ms": 800.0}),  # no rtfx
            800.0,
        )

    def test_missing_processing_is_zero(self):
        self.assertEqual(_stt_critical_ms({"mode": "batch"}), 0.0)
        self.assertEqual(_stt_critical_ms({"mode": "batch", "processing_ms": 0}), 0.0)


class CriticalPathBreakdownTests(unittest.TestCase):
    def test_moonshine_turn_shape(self):
        # The canonical a6024e2c turn 2: endpointing present → STT contributes
        # nothing beyond it, despite proc=2226.
        turn = {
            "stt": {"mode": "batch", "endpointing_ms": 510.0, "processing_ms": 2226.0, "rtfx": 4.92},
            "llm": {"ttft_ms": 219.0},
            "tts": {"ttfb_ms": 617.0},
        }
        p = critical_path_latency(turn)
        self.assertEqual(p["stt_critical_ms"], 0.0)
        # Total is far below the naive endpointing+processing+ttft+ttfb=3572.
        self.assertEqual(p["total_ms"], 510.0 + 219.0 + 617.0)
        # STT is NOT the dominant critical-path stage (the false-degrade culprit).
        self.assertNotEqual(p["dominant_stage"], "stt_critical_ms")

    def test_commit_anchored_ttfb_not_double_counted(self):
        # Session 049589c7 turn 4: ttfb 1284 is commit-anchored (marker:
        # internal_ttfb_ms present) and contains ttft 1130 — the old
        # endpointing+ttft+ttfb sum charged the LLM connect stall twice.
        turn = {
            "stt": {"mode": "batch", "endpointing_ms": 324.0, "processing_ms": 3224.0, "rtfx": 5.64},
            "llm": {"ttft_ms": 1130.0},
            "tts": {"ttfb_ms": 1284.0, "internal_ttfb_ms": 217.0},
        }
        p = critical_path_latency(turn)
        self.assertEqual(p["llm_ttft_ms"], 1130.0)
        self.assertEqual(p["tts_ttfb_ms"], 154.0)          # 1284 - 1130
        self.assertEqual(p["total_ms"], 324.0 + 1130.0 + 154.0)
        self.assertEqual(p["dominant_stage"], "llm_ttft_ms")

    def test_commit_anchored_ttfb_clamps_at_zero(self):
        # ttft >= ttfb (clock jitter between the two observers) must not go
        # negative.
        turn = {
            "stt": {"mode": "streaming", "endpointing_ms": 100.0},
            "llm": {"ttft_ms": 500.0},
            "tts": {"ttfb_ms": 480.0, "internal_ttfb_ms": 50.0},
        }
        p = critical_path_latency(turn)
        self.assertEqual(p["tts_ttfb_ms"], 0.0)
        self.assertEqual(p["total_ms"], 600.0)

    def test_legacy_ttfb_without_marker_keeps_sum(self):
        # No internal_ttfb_ms → we can't prove the anchor; keep the old
        # (conservative) ttft + ttfb sum.
        turn = {
            "stt": {"mode": "streaming", "endpointing_ms": 120.0},
            "llm": {"ttft_ms": 250.0},
            "tts": {"ttfb_ms": 200.0},
        }
        p = critical_path_latency(turn)
        self.assertEqual(p["tts_ttfb_ms"], 200.0)
        self.assertEqual(p["total_ms"], 570.0)

    def test_legacy_blocking_stt_dominates(self):
        # A genuinely blocking STT in a legacy capture (no endpointing_ms)
        # should surface as the dominant stage.
        turn = {
            "stt": {"mode": "batch", "processing_ms": 2500.0, "rtfx": 0.8},
            "llm": {"ttft_ms": 200.0},
            "tts": {"ttfb_ms": 300.0},
        }
        p = critical_path_latency(turn)
        self.assertEqual(p["dominant_stage"], "stt_critical_ms")
        self.assertAlmostEqual(p["stt_critical_ms"], 2500.0, places=1)

    def test_slow_stt_finalize_surfaces_as_endpointing(self):
        # Modern capture where STT was slow to finalize: the wait lands in
        # endpointing (measured at commit) and endpointing dominates — which
        # still maps to the stt component in the verdict's stage table.
        turn = {
            "stt": {"mode": "batch", "endpointing_ms": 2800.0, "processing_ms": 3000.0, "rtfx": 0.9},
            "llm": {"ttft_ms": 200.0},
            "tts": {"ttfb_ms": 300.0},
        }
        p = critical_path_latency(turn)
        self.assertEqual(p["stt_critical_ms"], 0.0)
        self.assertEqual(p["dominant_stage"], "endpointing_ms")

    def test_empty_turn_yields_none_total(self):
        p = critical_path_latency({})
        self.assertIsNone(p["total_ms"])
        self.assertIsNone(p["dominant_stage"])


class FallbackE2ETests(unittest.TestCase):
    """P1.4 (S12, P5): fallback `e2e_perceived_ms` must not double-count batch
    `processing_ms` on top of `endpointing_ms` or `ttft_ms` on top of a
    commit-anchored `ttfb_ms`."""

    def test_fallback_e2e_does_not_double_count_batch_processing_over_endpointing(self):
        utt = {
            "utterance_id": 1,
            "metric_snapshot": {
                "total_ms": 1100,
                "components": [
                    {
                        "kind": "stt",
                        "extras": {
                            "endpointing_ms": 400.0,
                            "processing_ms": 2500.0,
                            "stt_rtfx": 5.0,
                        },
                    },
                    {"kind": "llm", "extras": {"ttft_ms": 200.0, "generation_ms": 400.0}},
                    {"kind": "tts", "extras": {"ttfb_ms": 300.0, "synthesis_ms": 200.0}},
                ],
            },
        }
        row = _flatten_turn(utt, None, 1, {"stt": {"mode": "batch"}})
        # endpointing (400) + ttft (200) + legacy ttfb (300) = 900, NOT 900 + 2500 = 3400.
        self.assertEqual(row["e2e_perceived_ms"], 900.0)
        self.assertTrue(row["e2e_estimated"])

    def test_fallback_e2e_does_not_double_count_ttft_on_commit_anchored_ttfb(self):
        utt = {
            "utterance_id": 2,
            "metric_snapshot": {
                "total_ms": 1600,
                "components": [
                    {"kind": "stt", "extras": {"endpointing_ms": 300.0}},
                    {"kind": "llm", "extras": {"ttft_ms": 1100.0, "generation_ms": 1300.0}},
                    {
                        "kind": "tts",
                        "extras": {
                            "ttfb_ms": 1250.0,
                            "tts_internal_ttfb_ms": 150.0,
                            "synthesis_ms": 250.0,
                        },
                    },
                ],
            },
        }
        row = _flatten_turn(utt, None, 1, {"stt": {"mode": "streaming"}})
        # endpointing (300) + commit-anchored ttfb (1250, which already includes ttft 1100) = 1550,
        # NOT 300 + 1100 + 1250 = 2650.
        self.assertEqual(row["e2e_perceived_ms"], 1550.0)
        self.assertTrue(row["e2e_estimated"])


if __name__ == "__main__":
    unittest.main()
