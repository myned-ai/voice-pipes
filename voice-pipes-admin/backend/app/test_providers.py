"""Tests for the `session.providers` block builder `_extract_provider_identity`.

Two jobs:

1. **Characterisation / regression lock.** `_extract_provider_identity` had no
   test coverage; `test_legacy_*` pins the exact block shape produced for a
   capture WITHOUT device info, so the device extension provably cannot change
   the output for sessions predating it.
2. **Device extension.** The remaining tests cover the new `*_device` → per-
   component `device` mapping and its backward-compatible omission rules.

Stdlib `unittest` only, matching `test_anomalies.py`.
"""

import unittest

from .report.analysis import _coerce_device, _extract_provider_identity

# A metadata block as written today (pre-device): resolved model identities only.
LEGACY_META = {
    "stt_provider": "moonshine",
    "tts_provider": "pocket-http",
    "stt_model": {"id": "moonshine", "version": None},
    "llm_model": {"id": "gemma-3-1b-it", "version": None},
    "tts_model": {"id": None, "version": None},
}


class LegacyProviderBlockTest(unittest.TestCase):
    """Regression lock: a capture without any `*_device` must yield a providers
    block with NO `device` keys — byte-identical to pre-device behaviour."""

    def test_legacy_block_has_no_device_keys(self):
        block = _extract_provider_identity([], LEGACY_META)
        self.assertEqual(set(block), {"llm", "stt", "tts"})
        self.assertNotIn("device", block["llm"])
        self.assertNotIn("device", block["stt"])
        self.assertNotIn("device", block["tts"])
        # Existing field set is exactly as before.
        self.assertEqual(set(block["llm"]), {"provider", "model", "model_version", "stream"})
        self.assertEqual(set(block["stt"]), {"provider", "mode", "model", "model_version"})
        self.assertEqual(set(block["tts"]), {"provider", "voice", "model", "model_version"})
        self.assertEqual(block["llm"]["model"], "gemma-3-1b-it")

    def test_empty_metadata_no_crash_no_device(self):
        block = _extract_provider_identity([], {})
        for comp in ("llm", "stt", "tts"):
            self.assertNotIn("device", block[comp])


class DeviceExtractionTest(unittest.TestCase):
    def _meta_with_devices(self):
        meta = dict(LEGACY_META)
        meta.update(
            stt_device={"kind": "cpu", "detail": "VP_ORT_EP unset", "source": "config"},
            llm_device={"kind": "cuda", "detail": "28/28 layers", "source": "probe:/props"},
            tts_device={"kind": "cpu", "detail": "pocket-tts is CPU-only", "source": "static"},
        )
        return meta

    def test_device_surfaced_per_component(self):
        block = _extract_provider_identity([], self._meta_with_devices())
        self.assertEqual(block["llm"]["device"]["kind"], "cuda")
        self.assertEqual(block["llm"]["device"]["detail"], "28/28 layers")
        self.assertEqual(block["llm"]["device"]["source"], "probe:/props")
        self.assertEqual(block["stt"]["device"]["kind"], "cpu")
        self.assertEqual(block["tts"]["device"]["source"], "static")

    def test_null_device_omitted(self):
        # The engine always writes the key; an unreported slot is JSON null and
        # must NOT produce a device entry.
        meta = dict(LEGACY_META)
        meta.update(stt_device=None, llm_device=None, tts_device=None)
        block = _extract_provider_identity([], meta)
        for comp in ("llm", "stt", "tts"):
            self.assertNotIn("device", block[comp])

    def test_kindless_device_omitted(self):
        meta = dict(LEGACY_META)
        meta.update(llm_device={"detail": "no kind here"})
        block = _extract_provider_identity([], meta)
        self.assertNotIn("device", block["llm"])

    def test_kind_only_device_omits_optional_fields(self):
        meta = dict(LEGACY_META)
        meta.update(llm_device={"kind": "cpu"})
        dev = _extract_provider_identity([], meta)["llm"]["device"]
        self.assertEqual(dev, {"kind": "cpu"})


class CoerceDeviceTest(unittest.TestCase):
    def test_none_and_non_dict(self):
        self.assertIsNone(_coerce_device(None))
        self.assertIsNone(_coerce_device("cuda"))
        self.assertIsNone(_coerce_device({}))

    def test_kind_only(self):
        self.assertEqual(_coerce_device({"kind": "cuda"}), {"kind": "cuda"})

    def test_full(self):
        self.assertEqual(
            _coerce_device({"kind": "cuda", "detail": "x", "source": "config"}),
            {"kind": "cuda", "detail": "x", "source": "config"},
        )


if __name__ == "__main__":
    unittest.main()
