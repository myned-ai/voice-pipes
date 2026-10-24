"""Health/diagnostic engine — public facade.

The engine was split into focused modules; this module preserves the original
`app.report.analysis` import surface so callers (routers, analytics, tests) are
unaffected. Import from the specific submodule for new code:

  - ``report.util``       — small shared helpers (`_iso_to_ts`)
  - ``report.targets``    — SLA target resolution
  - ``report.providers``  — provider identity + device
  - ``report.stats``      — small-n distribution statistics
  - ``report.turns``      — per-turn flattening + critical-path decomposition
  - ``report.aggregates`` — per-component aggregates
  - ``report.anomalies``  — anomaly detection + severity grading
  - ``report.verdict``    — session-level verdict (level + primary culprit)

Builds a compact, LLM-friendly bundle an operator can paste into a chat to ask
"is this pipeline healthy?": a session header, per-turn rows, per-component
aggregates, precomputed `anomalies[]`, and SLA `targets`. See `report.render`
for the markdown/JSON assembly.
"""

from .aggregates import _CONNECT_DOMINATED_OUTLIER_MS, _component_aggregates
from .anomalies import (
    _HIGHER_IS_BETTER_KINDS,
    _SEVERITY_BANDS,
    _anomaly_severity,
    _detect_anomalies,
)
from .providers import (
    _STREAMING_STT_PROVIDERS,
    _coerce_device,
    _extract_provider_identity,
    _provider_mode,
)
from .stats import _distribution_stats, _percentiles
from .targets import (
    _DEFAULT_TARGETS,
    _DESCRIPTOR_BUDGET_TO_TARGET,
    _PROVIDER_TARGETS,
    DEFAULT_LEVELS,
    _parse_targets_override,
    _resolve_targets,
    _targets_from_descriptors,
    describe_custom_grading,
    resolve_grading,
)
from .turns import (
    _apply_tool_context,
    _estimated_llm_wait_ms,
    _flatten_turn,
    _llm_has_work,
    _stt_critical_ms,
    _usable_ttft_ms,
    critical_path_latency,
    resolve_turn_ttft,
    tool_roundtrip_ms,
)
from .util import _iso_to_ts
from .verdict import (
    _ANOMALY_COMPONENT,
    _CRITICAL_PATH_GOOD_P90_MS,
    _CRITICAL_PATH_POOR_P90_MS,
    _CRITICAL_PATH_UNUSABLE_P90_MS,
    _SEVERITY_WEIGHT,
    _STAGE_COMPONENT,
    _compute_verdict,
    _critical_path_percentiles,
    _health_model,
    _verdict_level_v2,
)

__all__ = [
    "DEFAULT_LEVELS",
    "_ANOMALY_COMPONENT",
    "_CONNECT_DOMINATED_OUTLIER_MS",
    "_CRITICAL_PATH_GOOD_P90_MS",
    "_CRITICAL_PATH_POOR_P90_MS",
    "_CRITICAL_PATH_UNUSABLE_P90_MS",
    "_DEFAULT_TARGETS",
    "_DESCRIPTOR_BUDGET_TO_TARGET",
    "_HIGHER_IS_BETTER_KINDS",
    "_PROVIDER_TARGETS",
    "_SEVERITY_BANDS",
    "_SEVERITY_WEIGHT",
    "_STAGE_COMPONENT",
    "_STREAMING_STT_PROVIDERS",
    "_anomaly_severity",
    "_apply_tool_context",
    "_coerce_device",
    "_component_aggregates",
    "_compute_verdict",
    "_critical_path_percentiles",
    "_detect_anomalies",
    "_distribution_stats",
    "_estimated_llm_wait_ms",
    "_extract_provider_identity",
    "_flatten_turn",
    "_health_model",
    "_iso_to_ts",
    "_llm_has_work",
    "_parse_targets_override",
    "_percentiles",
    "_provider_mode",
    "_resolve_targets",
    "_stt_critical_ms",
    "_targets_from_descriptors",
    "_usable_ttft_ms",
    "_verdict_level_v2",
    "critical_path_latency",
    "describe_custom_grading",
    "resolve_grading",
    "resolve_turn_ttft",
    "tool_roundtrip_ms",
]
