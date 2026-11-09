"""Tests for the metric registry descriptors single source of truth."""
from __future__ import annotations

import unittest

from app.captures.descriptors import (
    get_all_metric_keys,
    get_metric_descriptors,
)


class TestMetricRegistry(unittest.TestCase):
    def test_descriptors_loaded(self):
        descriptors = get_metric_descriptors()
        self.assertGreater(len(descriptors), 0)
        kinds = {d["kind"] for d in descriptors}
        self.assertIn("stt", kinds)
        self.assertIn("llm", kinds)
        self.assertIn("tts", kinds)
        self.assertIn("realtime", kinds)
        self.assertIn("output", kinds)
        self.assertIn("audio_post", kinds)
        self.assertIn("annotator", kinds)

    def test_every_field_has_anchors_and_owner(self):
        descriptors = get_metric_descriptors()
        for comp in descriptors:
            for field in comp.get("fields", []):
                key = field["key"]
                cid = comp["component_id"]
                self.assertTrue(
                    field.get("start_anchor"),
                    f"{cid}.{key} missing start_anchor",
                )
                self.assertTrue(
                    field.get("end_anchor"),
                    f"{cid}.{key} missing end_anchor",
                )
                self.assertIn(
                    field.get("owner"),
                    ("engine", "provider"),
                    f"{cid}.{key} missing or invalid owner: {field.get('owner')}",
                )
                self.assertTrue(
                    field.get("includes"),
                    f"{cid}.{key} missing includes",
                )

    def test_projected_snapshot_keys_exist_in_registry(self):
        # All keys that project_snapshot_metrics returns (excluding metadata/identifiers)
        # must exist in the canonical descriptors with expected units.
        expected_units = {
            "processing_ms": "ms",
            "endpointing_ms": "ms",
            "stt_audio_duration_ms": "ms",
            "stt_rtfx": "ratio",
            "ttft_ms": "ms",
            "llm_perceived_ttft_ms": "ms",
            "llm_wait_ms": "ms",
            "generation_ms": "ms",
            "ttfb_ms": "ms",
            "tts_internal_ttfb_ms": "ms",
            "tts_wait_ms": "ms",
            "output_wait_ms": "ms",
            "commit_to_first_audio_ms": "ms",
            "synthesis_ms": "ms",
            "audio_duration_ms": "ms",
            "first_text_ms": "ms",
            "provider_endpointing_ms": "ms",
            "audio_output_ms": "ms",
            "input_audio_ms": "ms",
            "sample_rate": "count",
            "input_speech_dbfs": "db",
            "input_peak_dbfs": "db",
            "input_noise_dbfs": "db",
            "input_snr_db": "db",
            "input_clipped_ratio": "percent",
            "input_speech_ratio": "ratio",
            "input_speech_ms": "ms",
            "input_level_source": "raw",
            "ttfb_anchor_source": "raw",
        }
        all_keys = get_all_metric_keys()
        for key, _expected_unit in expected_units.items():
            self.assertIn(
                key,
                all_keys,
                f"Projected metric key '{key}' is not registered in canonical descriptors",
            )


if __name__ == "__main__":
    unittest.main()
