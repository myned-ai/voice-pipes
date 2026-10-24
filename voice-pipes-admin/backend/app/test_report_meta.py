"""Tests for the report's portable-display metadata: report_version,
generated_at, and the self-describing `legend` block (levels, severities,
per-metric units/direction).

The key guarantee: the legend covers EVERY target key with a known direction, so
a consumer can render any metric correctly without hardcoding — and it can't
silently drift when a new target is added.

Run:: python -m unittest backend.app.test_report_meta
"""

import unittest

from .report.analysis import _SEVERITY_WEIGHT, _resolve_targets
from .report.render import _REPORT_VERSION, _build_report_for_llm, _render_report_markdown


def _report(targets=None):
    targets = targets if targets is not None else _resolve_targets({}, None)
    return _build_report_for_llm(
        session_id="t",
        metadata={},
        utterances=[],
        session_metrics=None,
        providers={"llm": {}, "stt": {}, "tts": {}},
        targets=targets,
        events=None,
        health_model="v2",
        generated_at="2026-07-02T00:00:00Z",
    )


class ReportMetaTests(unittest.TestCase):
    def test_version_and_generated_at_present(self):
        r = _report()
        self.assertEqual(r["report_version"], _REPORT_VERSION)
        self.assertEqual(r["generated_at"], "2026-07-02T00:00:00Z")

    def test_generated_at_defaults_to_now_utc(self):
        r = _build_report_for_llm(
            session_id="t", metadata={}, utterances=[], session_metrics=None,
            providers={}, targets=_resolve_targets({}, None), events=None, health_model="v2",
        )
        # Injected default is UTC, ISO-8601, Z-suffixed.
        self.assertTrue(r["generated_at"].endswith("Z"), r["generated_at"])

    def test_legend_covers_every_target_key(self):
        # The whole point: a consumer must be able to look up unit/direction for
        # any metric it's about to display. Missing coverage → unknown direction.
        targets = _resolve_targets({}, None)
        metrics = _report(targets)["legend"]["metrics"]
        for k in targets:
            self.assertIn(k, metrics, f"legend.metrics missing target key: {k}")
            entry = metrics[k]
            self.assertIn(
                entry["direction"], ("lower_better", "higher_better", "threshold"),
                f"{k} has unknown/invalid direction: {entry['direction']}",
            )
            self.assertTrue(entry["label"], f"{k} missing label")
            self.assertTrue(entry["unit"], f"{k} missing unit")

    def test_legend_covers_provider_relaxed_targets(self):
        # Cloud-LLM targets add/relax keys via _resolve_targets; legend must still
        # cover the full resolved set, not just the defaults.
        targets = _resolve_targets({"llm": {"provider": "openai-compat", "model": "gpt-4o-mini"}}, None)
        metrics = _report(targets)["legend"]["metrics"]
        for k in targets:
            self.assertIn(k, metrics, f"legend.metrics missing relaxed target key: {k}")

    def test_levels_legend_covers_all_v2_levels(self):
        levels = _report()["legend"]["levels"]
        for lvl in ("healthy", "fair", "degraded", "broken"):
            self.assertIn(lvl, levels)
            self.assertTrue(levels[lvl]["icon"])
            self.assertTrue(levels[lvl]["meaning"])

    def test_severities_weights_match_analysis(self):
        # Legend severity weights must equal the engine's culprit weights (no drift).
        sev = _report()["legend"]["severities"]
        for name, weight in _SEVERITY_WEIGHT.items():
            self.assertIn(name, sev)
            self.assertEqual(sev[name]["weight"], weight, f"{name} weight drifted")

    def test_markdown_fair_verdict_has_distinct_icon(self):
        # Regression: `fair` (a v2 level) used to render with the unknown glyph.
        r = _report()
        r["verdict"] = {"level": "fair", "headline": "hd", "primary_culprit": "tts",
                        "unstable_components": []}
        md = _render_report_markdown(r)
        self.assertIn("FAIR", md)
        self.assertNotIn("❓ FAIR", md)


if __name__ == "__main__":
    unittest.main()
