"""Perceived-E2E for realtime S2S captures (session-details reader).

S2S turns have no ``stt_timing`` event, and their ``utterance_received``
event fires at input-transcript *arrival* — for OpenAI Realtime often
after response audio already started — so the wallclock pair
``first_audio_out_ms - user_audio_end_ms`` came out negative, failed the
``0 <= delta`` sanity guard, and the E2E badge never showed for realtime
sessions. The realtime runner now stamps the provider-VAD end-of-speech
wall clock into the turn snapshot (``user_speech_end_epoch_ms``,
transport/serve/realtime_runner/turn.rs); these tests pin the reader's
consumption of it and the no-regression behavior for pipeline and
legacy-realtime captures.

Run:: python -m unittest backend.app.test_s2s_e2e
"""

import json
import unittest
from pathlib import Path
from tempfile import TemporaryDirectory

from .captures.reader import _read_capture_artifacts
from .core import config

# Fixed wall-clock base for deterministic fixtures: 2026-01-01T00:00:00Z.
T0_MS = 1_767_225_600_000.0


def _ts(ms: float) -> dict:
    """Server-side event timestamp in the Rust SystemTime serde shape."""
    return {
        "secs_since_epoch": int(ms // 1000),
        "nanos_since_epoch": int((ms % 1000) * 1_000_000),
    }


def _realtime_snapshot(uid: int, extras: dict) -> dict:
    return {
        "utterance_id": uid,
        "total_ms": 3000,
        "interrupted": False,
        "components": [
            {
                "component_id": "realtime",
                "kind": "realtime",
                "provider": "openai_realtime",
                "extras": extras,
            }
        ],
    }


class _CaptureFixture(unittest.TestCase):
    """Temp captures dir + one-session writer shared by the reader tests."""

    def setUp(self) -> None:
        self._tmp = TemporaryDirectory()
        self._old_captures_dir = config.CAPTURES_DIR
        config.CAPTURES_DIR = Path(self._tmp.name)

    def tearDown(self) -> None:
        config.CAPTURES_DIR = self._old_captures_dir
        self._tmp.cleanup()

    def _write_session(
        self,
        sid: str,
        pipeline_events: list[dict],
        events: list[dict],
    ) -> None:
        d = Path(self._tmp.name) / sid
        d.mkdir()
        (d / "metadata.json").write_text(json.dumps({"id": sid}), encoding="utf-8")
        (d / "pipeline_events.jsonl").write_text(
            "".join(json.dumps(e) + "\n" for e in pipeline_events), encoding="utf-8"
        )
        (d / "events.jsonl").write_text(
            "".join(json.dumps(e) + "\n" for e in events), encoding="utf-8"
        )

    def _summary(self, sid: str) -> dict:
        artifacts = _read_capture_artifacts(sid)
        utts = artifacts["utterances"]
        self.assertEqual(len(utts), 1)
        return utts[0].get("summary") or {}


def _client_timing(uid: int, event: str, client_ms: float) -> dict:
    return {
        "event_type": "client_timing",
        "timestamp": _ts(client_ms + 5),
        "payload": {"utterance_id": uid, "event": event, "client_timestamp_ms": client_ms},
    }


class S2sE2eReaderTest(_CaptureFixture):
    def test_realtime_wallclock_e2e_from_snapshot_anchor(self) -> None:
        # Provider VAD stopped at T0; the input transcript (and therefore
        # utterance_received) arrived at T0+1200 — AFTER the client already
        # received first audio at T0+900. The snapshot anchor must win.
        self._write_session(
            "rt-wallclock",
            pipeline_events=[
                {
                    "kind": "utterance_received",
                    "utterance_id": 1,
                    "source": "realtime",
                    "text": "hello",
                    "timestamp": _ts(T0_MS + 1200),
                },
                {
                    "kind": "turn_metrics_ready",
                    "utterance_id": 1,
                    "metrics": _realtime_snapshot(
                        1, {"ttfb_ms": 800, "user_speech_end_epoch_ms": T0_MS}
                    ),
                },
            ],
            events=[
                {
                    "event_type": "client_timing",
                    "timestamp": _ts(T0_MS + 905),
                    "payload": {
                        "utterance_id": 1,
                        "event": "audio_receive",
                        "client_timestamp_ms": T0_MS + 900,
                    },
                }
            ],
        )
        summary = self._summary("rt-wallclock")
        self.assertEqual(summary.get("e2e_perceived_ms"), 900.0)
        self.assertFalse(summary.get("e2e_estimated"))

    def test_realtime_estimated_e2e_without_client_timing(self) -> None:
        # Headless/bench clients send no client_timing reports: fall back to
        # the anchored TTFB (speech end → first audio byte at the server),
        # flagged as an estimate.
        self._write_session(
            "rt-estimate",
            pipeline_events=[
                {
                    "kind": "turn_metrics_ready",
                    "utterance_id": 1,
                    "metrics": _realtime_snapshot(
                        1, {"ttfb_ms": 800, "user_speech_end_epoch_ms": T0_MS}
                    ),
                },
            ],
            events=[],
        )
        summary = self._summary("rt-estimate")
        self.assertEqual(summary.get("e2e_perceived_ms"), 800.0)
        self.assertTrue(summary.get("e2e_estimated"))

    def test_legacy_realtime_capture_stays_suppressed(self) -> None:
        # Pre-fix capture: no snapshot anchor. The late utterance_received
        # timestamp makes the wallclock delta negative (guard drops it) and
        # the unanchored ttfb must NOT masquerade as an E2E estimate — an
        # unanchored ttfb is measured from turn creation and is ~0.
        self._write_session(
            "rt-legacy",
            pipeline_events=[
                {
                    "kind": "utterance_received",
                    "utterance_id": 1,
                    "source": "realtime",
                    "text": "hello",
                    "timestamp": _ts(T0_MS + 1200),
                },
                {
                    "kind": "turn_metrics_ready",
                    "utterance_id": 1,
                    "metrics": _realtime_snapshot(1, {"ttfb_ms": 3}),
                },
            ],
            events=[
                {
                    "event_type": "client_timing",
                    "timestamp": _ts(T0_MS + 905),
                    "payload": {
                        "utterance_id": 1,
                        "event": "audio_receive",
                        "client_timestamp_ms": T0_MS + 900,
                    },
                }
            ],
        )
        summary = self._summary("rt-legacy")
        self.assertIsNone(summary.get("e2e_perceived_ms"))

    def test_pipeline_wallclock_e2e_unaffected(self) -> None:
        # Classic STT→LLM→TTS session: user_audio_end still comes from
        # stt_timing (sequence-matched via utterance_received source=Audio);
        # the realtime override must not interfere.
        self._write_session(
            "pipeline-classic",
            pipeline_events=[
                {
                    "kind": "utterance_received",
                    "utterance_id": 1,
                    "source": "Audio",
                    "text": "hello",
                    "timestamp": _ts(T0_MS + 5),
                },
                {
                    "kind": "turn_metrics_ready",
                    "utterance_id": 1,
                    "metrics": {
                        "utterance_id": 1,
                        "total_ms": 2000,
                        "interrupted": False,
                        "components": [
                            {"component_id": "stt", "kind": "stt", "extras": {"endpointing_ms": 300}},
                            {"component_id": "llm", "kind": "llm", "extras": {"ttft_ms": 200}},
                            {"component_id": "tts", "kind": "tts", "extras": {"ttfb_ms": 400}},
                        ],
                    },
                },
            ],
            events=[
                {
                    "event_type": "stt_timing",
                    "timestamp": _ts(T0_MS),
                    "payload": {"endpointing_ms": 300},
                },
                {
                    "event_type": "client_timing",
                    "timestamp": _ts(T0_MS + 705),
                    "payload": {
                        "utterance_id": 1,
                        "event": "audio_receive",
                        "client_timestamp_ms": T0_MS + 700,
                    },
                },
            ],
        )
        summary = self._summary("pipeline-classic")
        # The commit is at T0 having waited 300 ms of silence, so the speaker
        # stopped at T0 - 300 and waited 1000 ms for the audio at T0 + 700.
        # This asserted 700 -- measured from the commit -- which leaves the
        # whole endpointing wait out of "perceived" latency. The anchor is
        # still the server's, untouched by the realtime override.
        self.assertEqual(summary.get("e2e_perceived_ms"), 1000.0)
        self.assertFalse(summary.get("e2e_estimated"))
        self.assertEqual(summary.get("e2e_anchor_source"), "stt_speech_end")

    def test_legacy_anchored_capture_is_labelled_provider(self) -> None:
        # Pre-plan capture with `user_speech_end_epoch_ms` but no source key:
        # anchored, provenance unrecorded.
        self._write_session(
            "rt-legacy-anchored",
            pipeline_events=[{
                "kind": "turn_metrics_ready", "utterance_id": 1,
                "metrics": _realtime_snapshot(1, {"ttfb_ms": 800, "user_speech_end_epoch_ms": T0_MS}),
            }],
            events=[_client_timing(1, "audio_receive", T0_MS + 900)],
        )
        summary = self._summary("rt-legacy-anchored")
        self.assertEqual(summary.get("e2e_perceived_ms"), 900.0)
        self.assertEqual(summary.get("e2e_anchor_source"), "provider")
        self.assertEqual(summary.get("ttfb_anchor_source"), "provider")


# ── Anchor precedence ──
#
# One Gemini-shaped turn: the user really stopped at T0 (client VAD stamp at
# T0-40 on the client's clock, vp's uplink VAD at T0), the provider's input
# transcript — its only "end of speech" — landed at T0+700, first audio left
# the server at T0+1800 (ttfb 1100 from the proxy) and reached the client at
# T0+1850. The three anchors give three different E2Es; each test removes the
# best one and expects the next.

PROXY_EXTRAS = {
    "ttfb_ms": 1100,
    "first_text_ms": 700,
    "ttfb_anchor_source": "input_transcript_proxy",
    "user_speech_end_epoch_ms": T0_MS + 700,
    "vp_speech_end_epoch_ms": T0_MS,
    "provider_endpointing_ms": 700,
    "audio_output_ms": 4000,
    "input_audio_ms": 1200,
}


class S2sAnchorPrecedenceTest(_CaptureFixture):
    def _turn(self, sid: str, extras: dict, events: list[dict]) -> dict:
        self._write_session(
            sid,
            pipeline_events=[
                {"kind": "utterance_received", "utterance_id": 1, "source": "realtime",
                 "text": "who are you", "timestamp": _ts(T0_MS + 700)},
                {"kind": "turn_metrics_ready", "utterance_id": 1, "metrics": _realtime_snapshot(1, extras)},
            ],
            events=events,
        )
        return self._summary(sid)

    def test_client_stamp_beats_vp_stamp_beats_provider_stamp(self):
        s = self._turn("prec-client", PROXY_EXTRAS,
                       [_client_timing(1, "speech_end", T0_MS - 40),
                        _client_timing(1, "audio_receive", T0_MS + 1850)])
        self.assertEqual(s["e2e_perceived_ms"], 1890.0)
        self.assertEqual(s["e2e_anchor_source"], "client_speech_end")
        self.assertFalse(s.get("e2e_estimated"))

    def test_vp_stamp_when_client_sent_none(self):
        s = self._turn("prec-vp", PROXY_EXTRAS, [_client_timing(1, "audio_receive", T0_MS + 1850)])
        self.assertEqual(s["e2e_perceived_ms"], 1850.0)
        self.assertEqual(s["e2e_anchor_source"], "vp_vad")

    def test_provider_stamp_when_vp_did_not_stamp(self):
        extras = {k: v for k, v in PROXY_EXTRAS.items()
                  if k not in ("vp_speech_end_epoch_ms", "provider_endpointing_ms")}
        s = self._turn("prec-provider", extras, [_client_timing(1, "audio_receive", T0_MS + 1850)])
        self.assertEqual(s["e2e_perceived_ms"], 1150.0)          # the late proxy under-reports
        self.assertEqual(s["e2e_anchor_source"], "input_transcript_proxy")

    def test_last_speech_end_before_the_reply_wins_and_only_speech_end_counts(self):
        # A client reports one speech_end per pause, because a detector cannot
        # tell a pause from the end of a turn. The later one -- still before
        # the reply's audio -- is when the speaker stopped, so it anchors the
        # turn. This used to assert the first won ("at most one per turn"),
        # which measured replies from mid-sentence hesitations. A speech_start
        # never moves the anchor.
        s = self._turn("prec-last", PROXY_EXTRAS,
                       [_client_timing(1, "speech_start", T0_MS - 2000),
                        _client_timing(1, "speech_end", T0_MS - 40),
                        _client_timing(1, "speech_end", T0_MS + 300),
                        _client_timing(1, "audio_receive", T0_MS + 1850)])
        self.assertEqual(s["e2e_perceived_ms"], 1550.0)

    def test_a_speech_end_after_the_reply_began_does_not_anchor_it(self):
        # Said over the agent's reply: it belongs to what comes next. Letting
        # it anchor this turn would make the wait negative.
        s = self._turn("prec-after", PROXY_EXTRAS,
                       [_client_timing(1, "speech_end", T0_MS - 40),
                        _client_timing(1, "audio_receive", T0_MS + 1850),
                        _client_timing(1, "speech_end", T0_MS + 2500)])
        self.assertEqual(s["e2e_perceived_ms"], 1890.0)

    def test_estimate_falls_back_in_order_without_audio_receive(self):
        # No client audio_receive: the estimate is re-anchored at vp's stamp
        # (provider endpointing + ttfb) …
        s = self._turn("est-vp", PROXY_EXTRAS, [])
        self.assertEqual(s["e2e_perceived_ms"], 1800.0)
        self.assertEqual(s["e2e_anchor_source"], "vp_vad")
        self.assertTrue(s["e2e_estimated"])
        # … and at the provider anchor when vp did not stamp.
        extras = {k: v for k, v in PROXY_EXTRAS.items()
                  if k not in ("vp_speech_end_epoch_ms", "provider_endpointing_ms")}
        s = self._turn("est-provider", extras, [])
        self.assertEqual(s["e2e_perceived_ms"], 1100.0)
        self.assertEqual(s["e2e_anchor_source"], "input_transcript_proxy")
        self.assertTrue(s["e2e_estimated"])
        # A client speech_end alone (no audio_receive) cannot form a pair
        # either — still the server-side estimate, honestly labelled.
        s = self._turn("est-client-only", PROXY_EXTRAS, [_client_timing(1, "speech_end", T0_MS - 40)])
        self.assertEqual(s["e2e_perceived_ms"], 1800.0)
        self.assertEqual(s["e2e_anchor_source"], "vp_vad")

    def test_turn_start_never_yields_e2e(self):
        # No anchor at all: ttfb ≈ 0 measures from turn creation. Neither the
        # wallclock pair (the late utterance_received proxy) nor the ttfb
        # fallback may produce an E2E, with or without client reports.
        extras = {"ttfb_ms": 3, "ttfb_anchor_source": "turn_start", "audio_output_ms": 4000}
        s = self._turn("ts-with-client", extras, [_client_timing(1, "audio_receive", T0_MS + 1850)])
        self.assertIsNone(s.get("e2e_perceived_ms"))
        self.assertIsNone(s.get("e2e_anchor_source"))
        s = self._turn("ts-headless", extras, [])
        self.assertIsNone(s.get("e2e_perceived_ms"))
        self.assertFalse(s.get("e2e_estimated"))
        # …but a client VAD stamp is a real anchor even for such a turn.
        s = self._turn("ts-client-vad", extras,
                       [_client_timing(1, "speech_end", T0_MS), _client_timing(1, "audio_receive", T0_MS + 1850)])
        self.assertEqual(s["e2e_perceived_ms"], 1850.0)
        self.assertEqual(s["e2e_anchor_source"], "client_speech_end")

    def test_text_submit_ignores_vad_stamps(self):
        extras = {"ttfb_ms": 900, "ttfb_anchor_source": "text_submit",
                  "user_speech_end_epoch_ms": T0_MS + 500, "vp_speech_end_epoch_ms": T0_MS,
                  "audio_output_ms": 2000}
        s = self._turn("text", extras,
                       [_client_timing(1, "speech_end", T0_MS - 3000), _client_timing(1, "audio_receive", T0_MS + 1450)])
        self.assertEqual(s["e2e_perceived_ms"], 950.0)
        self.assertEqual(s["e2e_anchor_source"], "text_submit")

    def test_new_keys_are_projected_onto_the_summary(self):
        s = self._turn("proj", PROXY_EXTRAS, [])
        self.assertEqual(s["ttfb_ms"], 1100.0)
        self.assertEqual(s["first_text_ms"], 700.0)
        self.assertEqual(s["ttfb_anchor_source"], "input_transcript_proxy")
        self.assertEqual(s["vp_speech_end_epoch_ms"], T0_MS)
        self.assertEqual(s["provider_endpointing_ms"], 700.0)
        self.assertEqual(s["user_speech_end_epoch_ms"], T0_MS + 700)
        self.assertIs(s["realtime"], True)
        # Cascade `endpointing_ms` stays STT-only on the summary vocabulary.
        self.assertIsNone(s.get("endpointing_ms"))


if __name__ == "__main__":
    unittest.main()
