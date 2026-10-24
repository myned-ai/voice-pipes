"""A truncated or malformed capture WAV is a 404 from the audio routes,
never a 500: sessions cut off mid-write are common and must not break the
session page."""

import asyncio
import unittest
import wave
from pathlib import Path
from tempfile import TemporaryDirectory

from fastapi import HTTPException

from app.core import config
from app.routers import audio


def _write_wav(path: Path, frames: int = 1600) -> None:
    with wave.open(str(path), "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(16000)
        w.writeframes(b"\x01\x00" * frames)


class TruncatedWavTests(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = TemporaryDirectory()
        self.caps = Path(self._tmp.name) / "captures"
        self.session = self.caps / "sess-1"
        self.session.mkdir(parents=True)
        self._old_caps = config.CAPTURES_DIR
        config.CAPTURES_DIR = self.caps

    def tearDown(self) -> None:
        config.CAPTURES_DIR = self._old_caps
        self._tmp.cleanup()

    def _status(self, coro) -> int:
        try:
            asyncio.run(coro)
        except HTTPException as exc:
            return exc.status_code
        return 200

    def test_truncated_fragment_concat_is_404(self) -> None:
        # Two fragments, both cut off after the RIFF header.
        for i in range(2):
            (self.session / f"001_tts_frag_{i:03d}.wav").write_bytes(b"RIFF\x00\x00\x00\x00WAVE")
        self.assertEqual(self._status(audio.get_turn_tts_concat("sess-1", 1)), 404)
        self.assertEqual(self._status(audio.get_turn_as_heard("sess-1", 1)), 404)

    def test_one_bad_fragment_is_skipped(self) -> None:
        _write_wav(self.session / "001_tts_frag_000.wav")
        (self.session / "001_tts_frag_001.wav").write_bytes(b"RIFF junk")
        _write_wav(self.session / "001_tts_frag_002.wav")
        self.assertEqual(self._status(audio.get_turn_tts_concat("sess-1", 1)), 200)

    def test_truncated_mic_capture_is_404(self) -> None:
        (self.session / "001_mic_raw.wav").write_bytes(b"RIFF\x00\x00\x00\x00WAVEfmt ")
        self.assertEqual(self._status(audio.get_turn_user_speech("sess-1", 1)), 404)

    def test_a_dropped_recording_never_stands_in_for_a_turn(self) -> None:
        # An utterance the recogniser made no turn of keeps its audio under
        # its reason; a request for a turn's audio by kind must not get it.
        _write_wav(self.session / "001_dropped_empty_transcript_mic_raw.wav")
        self.assertEqual(self._status(audio.get_session_audio("sess-1", "mic_raw")), 404)
        _write_wav(self.session / "001_mic_raw.wav")
        self.assertEqual(self._status(audio.get_session_audio("sess-1", "mic_raw")), 200)

    def test_open_wav_maps_wave_errors_to_404(self) -> None:
        bad = self.session / "bad.wav"
        bad.write_bytes(b"not a wav at all")
        with self.assertRaises(HTTPException) as ctx:
            audio._open_wav(bad)
        self.assertEqual(ctx.exception.status_code, 404)


if __name__ == "__main__":
    unittest.main()
