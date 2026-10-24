// Client-side builder for an LLM-friendly diagnostic report of a single
// (variant × scenario) pair from a benchmark experiment.  Mirrors the
// session-level `/api/v1/sessions/{id}/report-for-llm` markdown shape
// (verdict + goal + targets + notes + fenced JSON) so the same paste-it-
// into-an-LLM workflow works from the benchmark detail page.
//
// Everything is computed client-side from the data the page already has;
// no extra HTTP roundtrip is needed.
import type { ScenarioResult, SyntheticCallerResult, SyntheticStopReason } from './types';

export interface ScenarioReportContext {
  experimentId: string;
  experimentName?: string;
  scenarioId: string;
  scenarioName?: string;
  variantId: string;
  variantName: string;
  configPath?: string | null;
  configHash?: string | null;
  sessionsMissingArtifacts?: string[];
  result: ScenarioResult;
}

// Same SLA defaults the backend session report uses.  Kept terse on
// purpose; the LLM only needs ballpark numbers to reason about
// healthy/degraded/broken.
const DEFAULT_TARGETS = {
  ttft_ms: 300,
  ttfb_ms: 400,
  endpointing_ms: 500,
  stt_processing_ms: 800,
  rtf_max: 0.6,
  tokens_per_sec_min: 30,
  barge_in_rtt_ms: 400,
} as const;

type Verdict = {
  level: 'healthy' | 'degraded' | 'broken';
  icon: string;
  primary_culprit: string;
  headline: string;
};

function classifyVerdict(result: ScenarioResult): Verdict {
  const status = result.status;
  const synth = result.synthetic_caller ?? null;
  const stop = synth?.stop_reason ?? null;
  const pipelineTurns = (synth?.turns ?? []).filter(t => t.speaker === 'pipeline').length;

  // Hard-broken signals first.
  if (status === 'error') {
    return {
      level: 'broken', icon: '❌', primary_culprit: 'runner',
      headline: result.error_message ?? 'Scenario errored before producing usable data.',
    };
  }
  if (stop === 'caller_error') {
    return {
      level: 'broken', icon: '❌', primary_culprit: 'synthetic_caller',
      headline: 'Synthetic caller errored (WS/upstream failure) — pipeline never got a usable turn.',
    };
  }
  if (stop === 'pipeline_error') {
    return {
      level: 'broken', icon: '❌', primary_culprit: 'pipeline',
      headline: 'Pipeline returned an error mid-conversation.',
    };
  }

  // Soft-degraded: scenario ran but produced nothing.
  if (synth && pipelineTurns === 0) {
    const reasonHint = stop ? ` (stop_reason=${stop})` : '';
    return {
      level: 'degraded', icon: '⚠️', primary_culprit: 'synthetic_caller',
      headline: `Caller connected but produced 0 usable pipeline turns${reasonHint} — investigate caller config / persona / API auth.`,
    };
  }

  if (status === 'failed') {
    const fails = result.scoring?.expectation_failures ?? [];
    return {
      level: 'degraded', icon: '⚠️', primary_culprit: 'expectations',
      headline: fails.length
        ? `Expectations not met: ${fails.join('; ')}`
        : 'Scenario completed but failed expectation checks.',
    };
  }
  if (status === 'timeout') {
    return {
      level: 'degraded', icon: '⚠️', primary_culprit: 'pipeline',
      headline: 'Scenario timed out before completion.',
    };
  }

  return {
    level: 'healthy', icon: '✅', primary_culprit: 'none',
    headline: synth
      ? `Healthy scenario — ${pipelineTurns} pipeline turn(s), stop_reason=${stop ?? 'n/a'}.`
      : `Healthy scenario — status=${status}.`,
  };
}

function summarizeSynth(synth: SyntheticCallerResult | null | undefined): {
  stop_reason: SyntheticStopReason | null;
  total_duration_ms: number | null;
  caller_turns: number;
  pipeline_turns: number;
  barge_in_attempts: number;
  barge_in_successes: number;
} {
  if (!synth) {
    return {
      stop_reason: null, total_duration_ms: null,
      caller_turns: 0, pipeline_turns: 0,
      barge_in_attempts: 0, barge_in_successes: 0,
    };
  }
  return {
    stop_reason: synth.stop_reason ?? null,
    total_duration_ms: synth.total_duration_ms ?? null,
    caller_turns: synth.turns.filter(t => t.speaker === 'caller').length,
    pipeline_turns: synth.turns.filter(t => t.speaker === 'pipeline').length,
    barge_in_attempts: synth.barge_in_attempts ?? 0,
    barge_in_successes: synth.barge_in_successes ?? 0,
  };
}

export function buildScenarioReportForLlm(ctx: ScenarioReportContext): string {
  const { result } = ctx;
  const verdict = classifyVerdict(result);
  const synthSummary = summarizeSynth(result.synthetic_caller);
  const sessionsMissingArtifacts = (ctx.sessionsMissingArtifacts ?? []).filter(Boolean);

  const data = {
    experiment: {
      id: ctx.experimentId,
      name: ctx.experimentName ?? null,
    },
    variant: {
      id: ctx.variantId,
      name: ctx.variantName,
      config_path: ctx.configPath ?? null,
      config_hash: ctx.configHash ?? null,
    },
    scenario: {
      id: ctx.scenarioId,
      name: ctx.scenarioName ?? result.scenario_name ?? null,
      status: result.status,
      session_ids: result.session_ids,
      error_message: result.error_message ?? null,
      repetitions_failed: result.repetitions_failed ?? null,
    },
    verdict: {
      level: verdict.level,
      primary_culprit: verdict.primary_culprit,
      headline: verdict.headline,
    },
    targets: DEFAULT_TARGETS,
    metrics: result.metrics,
    scoring: result.scoring ?? null,
    synthetic_caller_summary: synthSummary,
    synthetic_caller: result.synthetic_caller ?? null,
    turns: result.turns ?? null,
    barge_in: result.barge_in ?? null,
    tts_entries: result.tts_entries ?? null,
    diagnostics: {
      sessions_missing_artifacts: sessionsMissingArtifacts,
    },
  };

  const t = DEFAULT_TARGETS;
  const sessionLinks = result.session_ids.length
    ? result.session_ids.map(id => `- ${id} (open the Session Detail page for full per-turn telemetry)`).join('\n')
    : '- (no session captured — caller likely failed before opening the pipeline WS)';
  const missingArtifactsNote = sessionsMissingArtifacts.length > 0
    ? (
      `- Capture artifacts were not found for ${sessionsMissingArtifacts.length} linked session(s):\n` +
      sessionsMissingArtifacts.map(id => `  - ${id}`).join('\n') + '\n' +
      `  Session detail may show sparse/empty telemetry when worker/orchestrator/admin resolve different captures_dir paths.\n`
    )
    : '';

  return (
    `# Benchmark scenario diagnostic\n` +
    `\n` +
    `## Verdict ${verdict.icon} ${verdict.level.toUpperCase()}\n` +
    `${verdict.headline}\n` +
    `  primary_culprit: ${verdict.primary_culprit}\n` +
    `\n` +
    `experiment: ${ctx.experimentName ?? ctx.experimentId} (${ctx.experimentId})\n` +
    `variant:    ${ctx.variantName} (${ctx.variantId})${ctx.configPath ? `  config=${ctx.configPath}` : ''}\n` +
    `scenario:   ${ctx.scenarioName ?? result.scenario_name ?? ctx.scenarioId} (${ctx.scenarioId})\n` +
    `status:     ${result.status}\n` +
    (result.synthetic_caller
      ? `caller:     ${synthSummary.caller_turns} caller turn(s), ${synthSummary.pipeline_turns} pipeline turn(s), stop_reason=${synthSummary.stop_reason ?? 'n/a'}, duration_ms=${synthSummary.total_duration_ms ?? 'n/a'}\n`
      : '') +
    `\n` +
    `## Goal\n` +
    `Tell me whether this scenario ran healthy, and if not which component is\n` +
    `the likely culprit and why.  Confirm or override the precomputed verdict\n` +
    `above.  For synthetic-caller scenarios, distinguish between:\n` +
    `- caller-side failures (WS/auth/persona/model issues — \`stop_reason\` is\n` +
    `  \`caller_error\`/\`caller_silence\` and \`pipeline_turns == 0\`),\n` +
    `- pipeline failures (\`stop_reason == "pipeline_error"\` or expectation\n` +
    `  failures), and\n` +
    `- successful runs with no expectations defined (status \`passed\` is then\n` +
    `  trivially true — say so explicitly).\n` +
    `\n` +
    `## Targets (interpret latencies against these; lower = better)\n` +
    `- ttft_ms <= ${t.ttft_ms} (LLM time-to-first-token)\n` +
    `- ttfb_ms <= ${t.ttfb_ms} (TTS time-to-first-byte)\n` +
    `- stt_processing_ms <= ${t.stt_processing_ms}\n` +
    `- endpointing_ms <= ${t.endpointing_ms} (VAD silence-to-commit)\n` +
    `- rtf <= ${t.rtf_max} (TTS realtime factor; >1.0 = worse than realtime)\n` +
    `- tokens_per_sec >= ${t.tokens_per_sec_min}\n` +
    `- barge_in_rtt_ms <= ${t.barge_in_rtt_ms}\n` +
    `\n` +
    `## Notes\n` +
    `- This bundle is scenario-scoped: it carries the per-scenario metrics\n` +
    `  the orchestrator aggregated, plus the raw synthetic-caller turn\n` +
    `  transcript when present.  For per-turn pipeline telemetry (STT/LLM/TTS\n` +
    `  latencies, anomalies), fetch the per-session report-for-llm of each\n` +
    `  \`session_ids[]\` entry below.\n` +
    `- \`stop_reason == "caller_silence"\` means the synthetic caller fell\n` +
    `  silent past the configured cap.  Combined with \`pipeline_turns == 0\`,\n` +
    `  the caller never actually opened a turn (typical when the upstream\n` +
    `  Realtime API rejected the session or the persona produced no audio).\n` +
    missingArtifactsNote +
    `- All times are milliseconds unless suffixed \`_s\` or labelled \`rtf\`.\n` +
    `\n` +
    `## Pipeline sessions for follow-up\n` +
    `${sessionLinks}\n` +
    `\n` +
    `## Data\n` +
    `\`\`\`json\n` +
    `${JSON.stringify(data, null, 2)}\n` +
    `\`\`\`\n`
  );
}
