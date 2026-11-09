"""Golden-replay regression net for the health engine (verdict + anomalies).

Snapshots the *current* verdict and
per-turn anomaly kinds/levels for every real capture in the corpus so later
phases can prove they don't silently drift a real session's health output.

- Golden fixture: ``testdata/health_golden.json`` (committed).
- Regenerate intentionally with ``UPDATE_HEALTH_GOLDEN=1``.
- Capture corpus is resolved from ``VP_GOLDEN_CAPTURES_DIR`` (default:
  ``../../voice-pipes/captures`` relative to this file).
- Without ``VP_GOLDEN_CAPTURES_DIR`` the default is the live captures
  directory, which gains and loses sessions all the time: the test skips,
  saying why, when that directory is absent (CI) or holds none of the golden
  sessions. With ``VP_GOLDEN_CAPTURES_DIR`` naming a corpus that exists, a
  corpus with no golden session is a failure: the caller asked for this
  corpus to be checked.

Run::

    python -m unittest backend.app.test_health_golden
    UPDATE_HEALTH_GOLDEN=1 python -m unittest backend.app.test_health_golden  # refresh
"""

import json
import os
import unittest
from pathlib import Path

from . import core
from .core import config
from .report.analysis import (
    _extract_provider_identity,
    _resolve_targets,
)
from .report.render import _build_report_for_llm

_THIS_DIR = Path(__file__).resolve().parent
_GOLDEN_PATH = _THIS_DIR / "testdata" / "health_golden.json"


_DEFAULT_CORPUS = _THIS_DIR.parents[2] / "voice-pipes" / "captures"


def _corpus_choice() -> tuple[Path | None, bool, str]:
    """(corpus, named explicitly, why there is none)."""
    env = os.getenv("VP_GOLDEN_CAPTURES_DIR", "").strip()
    if env:
        path = Path(env)
        if path.is_dir():
            return path, True, ""
        return None, True, f"VP_GOLDEN_CAPTURES_DIR={env} is not a directory"
    if _DEFAULT_CORPUS.is_dir():
        return _DEFAULT_CORPUS, False, ""
    return None, False, (
        f"no capture corpus: VP_GOLDEN_CAPTURES_DIR is unset and {_DEFAULT_CORPUS} does not exist"
    )


def _corpus_dir() -> Path | None:
    return _corpus_choice()[0]


def _session_ids(corpus: Path) -> list[str]:
    return sorted(
        p.name for p in corpus.iterdir()
        if p.is_dir() and (p / "metadata.json").exists()
    )


def _health_snapshot(session_id: str) -> dict:
    """Reproduce the report-for-llm health output for one session.

    Mirrors routers/sessions.py::report_for_llm so the snapshot matches what
    the operator actually sees.
    """
    # Import here so a missing captures dir doesn't break collection.
    from .captures.reader import _read_capture_artifacts

    artifacts = _read_capture_artifacts(session_id, events_offset=0, events_limit=5000)
    meta_path = config.CAPTURES_DIR / session_id / "metadata.json"
    metadata: dict = {}
    if meta_path.exists():
        metadata = json.loads(meta_path.read_text(encoding="utf-8"))

    utterances = artifacts.get("utterances") or []
    providers = _extract_provider_identity(utterances, metadata)
    targets = _resolve_targets(
        providers, None, metric_descriptors=metadata.get("metric_descriptors")
    )
    report = _build_report_for_llm(
        session_id=session_id,
        metadata=metadata,
        utterances=utterances,
        session_metrics=artifacts.get("session_metrics"),
        providers=providers,
        targets=targets,
        events=artifacts.get("events"),
        # Pin v1 explicitly so the golden's v1 regression floor is independent of
        # the runtime `VP_HEALTH_MODEL` default (which may be flipped to v2). With
        # v1 active, the graded v2 verdict rides along as `verdict_v2` preview.
        health_model="v1",
        client_tool_calls=artifacts.get("client_tool_calls"),
    )
    verdict = report.get("verdict") or {}
    verdict_v2 = report.get("verdict_v2") or {}  # preview attached while v1 active
    anomalies = report.get("anomalies") or []

    def _v(v):
        return {
            "level": v.get("level"),
            "primary_culprit": v.get("primary_culprit"),
            "unstable_components": sorted(v.get("unstable_components") or []),
        }

    return {
        # v1 = the pinned regression floor (must never drift silently).
        "verdict": _v(verdict),
        # v2 = the graded model, pinned so its rollout is also reviewed.
        "verdict_v2": {**_v(verdict_v2), "critical_path_p90_ms": verdict_v2.get("critical_path_p90_ms")},
        # Anomaly identity that must stay stable: (utt, kind, level). Sorted so
        # ordering changes don't produce spurious diffs.
        "anomalies": sorted(
            [f"{a.get('utt')}:{a.get('kind')}:{a.get('level')}" for a in anomalies]
        ),
        "targets": targets,
    }


def _build_all() -> dict:
    corpus = _corpus_dir()
    assert corpus is not None
    config.CAPTURES_DIR = corpus.resolve()
    core.config.CAPTURES_DIR = corpus.resolve()
    return {sid: _health_snapshot(sid) for sid in _session_ids(corpus)}


class HealthGoldenReplayTests(unittest.TestCase):
    def setUp(self):
        self.corpus, self.explicit, why = _corpus_choice()
        if self.corpus is None:
            self.skipTest(why)
        config.CAPTURES_DIR = self.corpus.resolve()
        core.config.CAPTURES_DIR = self.corpus.resolve()

    def test_replay_matches_golden(self):
        if os.getenv("UPDATE_HEALTH_GOLDEN") == "1":
            current = {sid: _health_snapshot(sid) for sid in _session_ids(self.corpus)}
            _GOLDEN_PATH.parent.mkdir(parents=True, exist_ok=True)
            _GOLDEN_PATH.write_text(
                json.dumps(current, indent=2, sort_keys=True) + "\n", encoding="utf-8"
            )
            self.skipTest(f"golden regenerated at {_GOLDEN_PATH}")

        if not _GOLDEN_PATH.exists():
            self.fail(
                f"golden fixture missing at {_GOLDEN_PATH}; "
                "generate with UPDATE_HEALTH_GOLDEN=1"
            )

        golden = json.loads(_GOLDEN_PATH.read_text(encoding="utf-8"))
        # Compare per-session so a diff pinpoints the drifted session.
        #
        # Only sessions with a snapshot are judged. The corpus is the live
        # captures directory, which gains a session every time anyone talks to
        # a worker and loses them to retention; a test that failed on either
        # failed constantly for a reason that was not a defect, and a test
        # that is always red is one nobody reads. What this pins is that the
        # engine's answer for a given capture does not change unnoticed.
        shared = sorted(set(golden) & set(_session_ids(self.corpus)))
        if not shared and not self.explicit:
            # The live directory has moved on; that is not a defect.
            self.skipTest(
                f"{self.corpus} holds none of the {len(golden)} golden sessions; set "
                "VP_GOLDEN_CAPTURES_DIR to a corpus that does, or regenerate with "
                "UPDATE_HEALTH_GOLDEN=1"
            )
        self.assertGreater(
            len(shared), 0,
            f"no session in the golden file exists in {self.corpus}; "
            "regenerate with UPDATE_HEALTH_GOLDEN=1",
        )
        current = {sid: _health_snapshot(sid) for sid in shared}
        for sid in shared:
            self.assertEqual(
                current[sid], golden[sid],
                f"health output drifted for session {sid}",
            )

    def test_replay_session_73d30ef8_shape_names_llm_as_culprit(self):
        """Incident session 73d30ef8 (zero TTFT with LLM work) must blame the LLM, not TTS."""
        from .report.anomalies import _detect_anomalies
        from .report.verdict import _compute_verdict
        from .test_measurement import TARGETS, _session_73d30ef8_turns

        turns = _session_73d30ef8_turns()
        anomalies = _detect_anomalies(turns, TARGETS)
        providers = {"llm": {"provider": "gguf"}, "tts": {"provider": "piper"}}
        verdict = _compute_verdict(turns, [], anomalies, providers, model="v1")

        # The regression: the old code blamed TTS because tts.ttfb_ms was ~2221ms (anchored to commit)
        # while llm.ttft_ms was reported as 0.0.
        # With metric integrity, the primary culprit must be LLM, never TTS.
        self.assertNotEqual(verdict.get("primary_culprit"), "tts")
        self.assertIn("measurement.zero_with_work", [a["kind"] for a in anomalies])
        self.assertEqual(verdict.get("primary_culprit"), "llm")


if __name__ == "__main__":
    unittest.main()
