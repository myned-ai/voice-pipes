"""Session metadata that reaches the diagnostic report and the session view:
STT mode, TTS voice, the orchestrator/capture metadata merge, and the event
timeline rows merged in from pipeline_events.jsonl.

Stdlib `unittest` only, matching the other report tests.
"""

import unittest

from .report.analysis import _extract_provider_identity
from .report.render import _compact_event_timeline
from .routers.sessions import merge_session_metadata


class SttModeTest(unittest.TestCase):
    def test_declared_mode_wins_over_the_provider_name(self):
        # A provider the name fallback would call batch, declared streaming.
        block = _extract_provider_identity([], {"stt_provider": "custom-plugin", "stt_mode": "streaming"})
        self.assertEqual(block["stt"]["mode"], "streaming")
        # And the reverse: a name on the streaming list, declared batch.
        block = _extract_provider_identity([], {"stt_provider": "assemblyai", "stt_mode": "batch"})
        self.assertEqual(block["stt"]["mode"], "batch")

    def test_google_is_streaming_without_a_declared_mode(self):
        # Captures written before `stt_mode` existed fall back to the name.
        block = _extract_provider_identity([], {"stt_provider": "google"})
        self.assertEqual(block["stt"]["mode"], "streaming")

    def test_unrecognised_declared_mode_falls_back(self):
        block = _extract_provider_identity([], {"stt_provider": "moonshine", "stt_mode": "sometimes"})
        self.assertEqual(block["stt"]["mode"], "batch")


class TtsVoiceTest(unittest.TestCase):
    def test_voice_comes_from_metadata(self):
        block = _extract_provider_identity([], {"tts_provider": "pocket-http", "tts_voice": "hr_male_leko"})
        self.assertEqual(block["tts"]["voice"], "hr_male_leko")

    def test_event_voice_still_used_for_older_captures(self):
        utts = [{"tts_detail": {"voice": "alba", "provider": "pocket-http"}}]
        block = _extract_provider_identity(utts, {"tts_provider": "pocket-http"})
        self.assertEqual(block["tts"]["voice"], "alba")


class MergeSessionMetadataTest(unittest.TestCase):
    LOCAL = {
        "pipeline_config_hash": "d0b9d2d9",
        "input_sample_rate": 16000,
        "output_sample_rate": 24000,
        "stt_mode": "streaming",
        "tags": ["local"],
    }

    def test_orchestrator_null_does_not_hide_capture_values(self):
        merged = merge_session_metadata(
            self.LOCAL,
            {
                "status": "completed",
                "pipeline_config_hash": None,
                "input_sample_rate": None,
                "worker_id": "worker-croatian",
                "turn_count": 3,
                "tags": [],
            },
        )
        self.assertEqual(merged["pipeline_config_hash"], "d0b9d2d9")
        self.assertEqual(merged["input_sample_rate"], 16000)
        self.assertEqual(merged["stt_mode"], "streaming")
        self.assertEqual(merged["status"], "completed")
        self.assertEqual(merged["worker_id"], "worker-croatian")
        self.assertEqual(merged["turn_count"], 3)
        self.assertEqual(merged["tags"], ["local"])

    def test_orchestrator_values_win_when_present(self):
        merged = merge_session_metadata(
            self.LOCAL,
            {"pipeline_config_hash": "from-orchestrator", "tags": ["orch"], "turn_count": 1},
        )
        self.assertEqual(merged["pipeline_config_hash"], "from-orchestrator")
        self.assertEqual(merged["tags"], ["orch"])

    def test_orchestrator_resource_fields_merged(self):
        merged = merge_session_metadata(
            self.LOCAL,
            {
                "peak_rss_bytes": 67108864,
                "container_peak_memory_bytes": 75497472,
                "avg_turn_cpu_ms": 38,
            },
        )
        self.assertEqual(merged["peak_rss_bytes"], 67108864)
        self.assertEqual(merged["container_peak_memory_bytes"], 75497472)
        self.assertEqual(merged["avg_turn_cpu_ms"], 38)


class ReportResourcesTest(unittest.TestCase):
    def test_resources_rendered_in_session_and_markdown(self):
        from .report.render import _build_report_for_llm, _render_report_markdown
        from .report.turns import _flatten_turn

        meta = {
            "pipeline_name": "custom-classic",
            "pipeline_config_hash": "abc12345",
            "peak_rss_bytes": 64 * 1024 * 1024,
            "container_peak_memory_bytes": 80 * 1024 * 1024,
            "avg_turn_cpu_ms": 42,
        }
        utt = {
            "utterance_id": 1,
            "user_text": "Hello",
            "llm_output": {"text": "Hi there!"},
            "resource_usage": {
                "process_rss_bytes": 64 * 1024 * 1024,
                "cpu_percent": 15.2,
                "cpu_time_ms": 42,
            },
        }
        flat = _flatten_turn(utt, None, 1, {})
        self.assertIn("resources", flat)
        self.assertEqual(flat["resources"]["cpu_time_ms"], 42)

        targets = {
            "ttft_ms": 300,
            "ttfb_ms": 400,
            "stt_processing_ms": 800,
            "endpointing_ms": 500,
            "rtf_max": 0.6,
            "tokens_per_sec_min": 18,
            "barge_in_rtt_ms": 400,
            "self_barge_played_ms_max": 200,
        }
        report = _build_report_for_llm(
            session_id="test-res-sess",
            metadata=meta,
            utterances=[utt],
            session_metrics={},
            providers={},
            targets=targets,
        )
        self.assertEqual(report["session"]["peak_rss_bytes"], 64 * 1024 * 1024)
        self.assertEqual(report["totals"]["avg_turn_cpu_ms"], 42)
        self.assertIn("resources", report["turns"][0])

        md = _render_report_markdown(report)
        self.assertIn("resources:  peak_rss=64.0MB  container_peak=80.0MB  avg_turn_cpu=42ms", md)


def _trace_client_timing(event: str, client_ms: int, secs: int) -> dict:
    return {
        "timestamp": {"secs_since_epoch": secs, "nanos_since_epoch": 0},
        "component": "client",
        "event_type": "client_timing",
        "payload": {"client_timestamp_ms": client_ms, "event": event, "utterance_id": 0},
    }


def _pipeline_client_timing(event: str, client_ms: int) -> dict:
    # As merged by the session reader: `type` mapped from `kind`, component defaulted.
    return {
        "kind": "client_timing",
        "type": "client_timing",
        "component": "pipeline",
        "utterance_id": 0,
        "timing_event": event,
        "client_timestamp_ms": client_ms,
    }


class EventTimelineTest(unittest.TestCase):
    STARTED_AT = "2026-09-15T15:19:23Z"  # epoch 1789485563
    T0 = 1789485563

    def test_pipeline_twins_of_trace_client_timings_are_dropped(self):
        events = [
            _pipeline_client_timing("audio_receive", 1789485569225),
            _trace_client_timing("audio_receive", 1789485569225, self.T0 + 6),
        ]
        rows = _compact_event_timeline(events, self.STARTED_AT)
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]["component"], "client")
        self.assertIsNotNone(rows[0]["t_s"])
        self.assertIsNotNone(rows[0]["p"])

    def test_pipeline_only_rows_get_payload_and_client_time(self):
        rows = _compact_event_timeline(
            [_pipeline_client_timing("speech_end", 1789485579255)], self.STARTED_AT
        )
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]["p"]["event"], "speech_end")
        self.assertEqual(rows[0]["p"]["utterance_id"], 0)
        self.assertAlmostEqual(rows[0]["t_s"], 16.255, places=3)

    def test_untimed_pipeline_rows_keep_payload_and_sort_last(self):
        interrupted = {
            "kind": "interrupted",
            "type": "interrupted",
            "component": "pipeline",
            "utterance_id": 2,
            "played_ms": 900,
            "trigger": "server_vad",
        }
        events = [interrupted, _trace_client_timing("playback_start", 1789485569466, self.T0 + 6)]
        rows = _compact_event_timeline(events, self.STARTED_AT)
        self.assertEqual([r["type"] for r in rows], ["client_timing", "interrupted"])
        self.assertIsNone(rows[1]["t_s"])
        self.assertEqual(rows[1]["p"], {"utterance_id": 2, "played_ms": 900, "trigger": "server_vad"})


if __name__ == "__main__":
    unittest.main()
