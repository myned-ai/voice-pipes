"""Tests for descriptor-declared budgets.

A provider that ships `health.budget` on its descriptor fields no longer needs
an entry in the hardcoded `_PROVIDER_TARGETS` table; the table remains the
fallback for captures/providers that don't declare budgets.

Run:: python -m unittest backend.app.test_targets
"""

import unittest

from .report.analysis import _DEFAULT_TARGETS, _resolve_targets, _targets_from_descriptors


def _descriptor(kind, fields):
    return {"component_id": kind, "kind": kind, "label": kind, "fields": fields}


class TargetsFromDescriptorsTests(unittest.TestCase):
    def test_extracts_declared_budgets(self):
        md = [
            _descriptor("llm", [{"key": "ttft_ms", "unit": "ms",
                                 "health": {"role": "latency", "budget": 1200.0}}]),
            _descriptor("tts", [
                {"key": "ttfb_ms", "unit": "ms", "health": {"budget": 500.0}},
                {"key": "rtf", "unit": "ratio", "health": {"budget": 0.8}},
            ]),
            _descriptor("stt", [{"key": "processing_ms", "unit": "ms",
                                 "health": {"budget": 900.0}}]),
        ]
        got = _targets_from_descriptors(md)
        self.assertEqual(got, {
            "ttft_ms": 1200.0, "ttfb_ms": 500.0, "rtf_max": 0.8, "stt_processing_ms": 900.0,
        })

    def test_no_health_block_yields_empty(self):
        md = [_descriptor("llm", [{"key": "ttft_ms", "unit": "ms"}])]  # legacy capture
        self.assertEqual(_targets_from_descriptors(md), {})

    def test_none_is_empty(self):
        self.assertEqual(_targets_from_descriptors(None), {})


class ResolveTargetsPrecedenceTests(unittest.TestCase):
    def test_descriptor_budget_overrides_hardcoded_table(self):
        # openai-compat's hardcoded table sets ttft_ms=800; a declared budget wins.
        providers = {"llm": {"provider": "openai-compat", "model": "gpt-4o-mini"}}
        md = [_descriptor("llm", [{"key": "ttft_ms", "unit": "ms", "health": {"budget": 1234.0}}])]
        t = _resolve_targets(providers, None, metric_descriptors=md)
        self.assertEqual(t["ttft_ms"], 1234.0)

    def test_caller_override_still_wins_over_descriptor(self):
        md = [_descriptor("llm", [{"key": "ttft_ms", "unit": "ms", "health": {"budget": 1234.0}}])]
        t = _resolve_targets({}, "ttft_ms:222", metric_descriptors=md)
        self.assertEqual(t["ttft_ms"], 222.0)

    def test_legacy_capture_falls_back_to_defaults(self):
        # No descriptors → identical to today's resolution (the golden guarantee).
        t = _resolve_targets({}, None, metric_descriptors=None)
        self.assertEqual(t["ttft_ms"], _DEFAULT_TARGETS["ttft_ms"])
        self.assertEqual(t["stt_processing_ms"], _DEFAULT_TARGETS["stt_processing_ms"])


if __name__ == "__main__":
    unittest.main()
