// Shared per-turn metrics fixtures for unit + component tests.
//
// Tests that need a `TurnMetricsSnapshot` should call `makeSnapshot()`
// rather than building one inline — keeps the test corpus aligned with
// the canonical extras key vocabulary (`pipeline::extras`) and makes it
// easy to evolve the wire shape without touching every test file.

import type {
  ComponentMetricDescriptor,
  MetricFieldDescriptor,
  TurnComponentMetrics,
  TurnMetricsSnapshot,
} from "../../lib/transport/types";

export interface PartialComponent extends Partial<TurnComponentMetrics> {
  component_id: string;
  kind: string;
}

/** Build a `TurnComponentMetrics` with sensible defaults. */
export function makeComponent(p: PartialComponent): TurnComponentMetrics {
  return {
    started_at_ms: 0,
    finished_at_ms: 100,
    latency_ms: 100,
    ...p,
  };
}

export interface MakeSnapshotOpts {
  utterance_id?: number;
  total_ms?: number;
  interrupted?: boolean;
  components?: TurnComponentMetrics[];
  /** Convenience: build canonical stt/llm/tts components when no
   *  explicit `components` array is provided. */
  stt?: Partial<TurnComponentMetrics>;
  llm?: Partial<TurnComponentMetrics>;
  tts?: Partial<TurnComponentMetrics>;
}

/** Build a `TurnMetricsSnapshot`. When `components` is omitted, a
 *  default stt/llm/tts triple is synthesised (each can be tweaked via
 *  `stt`/`llm`/`tts` overrides). */
export function makeSnapshot(opts: MakeSnapshotOpts = {}): TurnMetricsSnapshot {
  const components =
    opts.components ?? [
      makeComponent({
        component_id: "stt",
        kind: "stt",
        started_at_ms: 0,
        finished_at_ms: 200,
        latency_ms: 200,
        provider: "whisper",
        extras: { processing_ms: 180, endpointing_ms: 20 },
        ...opts.stt,
      }),
      makeComponent({
        component_id: "llm",
        kind: "llm",
        started_at_ms: 200,
        finished_at_ms: 600,
        latency_ms: 400,
        provider: "openai",
        extras: { ttft_ms: 120, generation_ms: 400, token_count: 42 },
        ...opts.llm,
      }),
      makeComponent({
        component_id: "tts",
        kind: "tts",
        started_at_ms: 600,
        finished_at_ms: 1000,
        latency_ms: 400,
        provider: "piper",
        extras: {
          ttfb_ms: 60,
          synthesis_ms: 380,
          audio_duration_ms: 2400,
          sample_rate: 22050,
          chars: 120,
          rtf: 0.16,
        },
        ...opts.tts,
      }),
    ];
  return {
    utterance_id: opts.utterance_id ?? 1,
    total_ms: opts.total_ms ?? 1000,
    interrupted: opts.interrupted ?? false,
    components,
  };
}

export interface MakeDescriptorOpts {
  component_id: string;
  kind: string;
  label?: string;
  fields?: MetricFieldDescriptor[];
}

export function makeDescriptor(opts: MakeDescriptorOpts): ComponentMetricDescriptor {
  return {
    component_id: opts.component_id,
    kind: opts.kind,
    label: opts.label ?? opts.component_id,
    fields: opts.fields ?? [],
  };
}
