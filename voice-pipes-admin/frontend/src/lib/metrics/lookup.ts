// Per-turn metrics lookup helpers.
//
// Wire-shape consumers (playground / session-detail panels) read fields
// from `TurnMetricsSnapshot` via these helpers instead of indexing
// `extras` directly. This keeps kind strings and extras keys in exactly
// one place — adding a new component or renaming a key on the backend
// only forces edits here, never in the rendering sites.

import type {
  TurnComponentMetrics,
  TurnMetricsSnapshot,
} from "../transport/types";

/** First component matching `kind` (case-sensitive, snake_case). */
export function componentByKind(
  s: TurnMetricsSnapshot | undefined,
  kind: string,
): TurnComponentMetrics | undefined {
  return s?.components.find((c) => c.kind === kind);
}

/** First component matching `component_id` exactly. */
export function componentById(
  s: TurnMetricsSnapshot | undefined,
  id: string,
): TurnComponentMetrics | undefined {
  return s?.components.find((c) => c.component_id === id);
}

/** Read an extras value by key, narrowed to `T` (no runtime check). */
export function extra<T = unknown>(
  c: TurnComponentMetrics | undefined,
  key: string,
): T | undefined {
  return c?.extras?.[key] as T | undefined;
}

/** Same as `extra<number>(c, key)` but rejects non-finite numbers so
 *  callers don't accidentally render `NaN` or `Infinity`. */
export function extraNumber(
  c: TurnComponentMetrics | undefined,
  key: string,
): number | undefined {
  const v = c?.extras?.[key];
  return typeof v === "number" && Number.isFinite(v) ? v : undefined;
}

// ── Well-known stage helpers ────────────────────────────────
//
// These are the only places that hardcode the stt/llm/tts kind strings
// and the canonical extras keys (`ttft_ms`, `ttfb_ms`, …). Any new
// well-known stage gets one helper here; everything else iterates
// `snapshot.components` generically driven by descriptors.

/** LLM time-to-first-token (ms). */
export function ttftMs(s: TurnMetricsSnapshot | undefined): number | undefined {
  return extraNumber(componentByKind(s, "llm"), "ttft_ms");
}

/** Runner-observed time to first audio byte forwarded to the client (ms).
 *  Falls back to the realtime S2S component when the snapshot has no
 *  classic `tts` component (S2S sessions collapse stt+llm+tts into a
 *  single `kind: "realtime"` slice). */
export function ttfbMs(s: TurnMetricsSnapshot | undefined): number | undefined {
  const v = extraNumber(componentByKind(s, "tts"), "ttfb_ms");
  if (v !== undefined) return v;
  return extraNumber(componentByKind(s, "realtime"), "ttfb_ms");
}

/** STT processing latency: end-of-speech flush → final transcript (ms). */
export function sttLatencyMs(
  s: TurnMetricsSnapshot | undefined,
): number | undefined {
  const c = componentByKind(s, "stt");
  return extraNumber(c, "processing_ms") ?? c?.latency_ms;
}

/** LLM full generation duration: first token → last token (ms). */
export function llmGenerationMs(
  s: TurnMetricsSnapshot | undefined,
): number | undefined {
  const c = componentByKind(s, "llm");
  return extraNumber(c, "generation_ms") ?? c?.latency_ms;
}

/** TTS pipeline-internal (pre-pacer) TTFB: first LLM fragment → first audio
 *  sample (ms). Distinct from `ttfbMs` (the runner-observed/perceived TTFB the
 *  engine writes to `ttfb_ms`); this is the synthesis-startup portion used to
 *  draw the timeline's TTS-startup sub-segment. */
export function ttsTtfbMs(
  s: TurnMetricsSnapshot | undefined,
): number | undefined {
  return extraNumber(componentByKind(s, "tts"), "tts_internal_ttfb_ms");
}

/** Total TTS synthesis span (ms) — first fragment → last audio chunk. */
export function ttsSynthesisMs(
  s: TurnMetricsSnapshot | undefined,
): number | undefined {
  return extraNumber(componentByKind(s, "tts"), "synthesis_ms");
}

/** Wall-clock duration of synthesised audio for the turn (ms). */
export function ttsAudioDurationMs(
  s: TurnMetricsSnapshot | undefined,
): number | undefined {
  return extraNumber(componentByKind(s, "tts"), "audio_duration_ms");
}

/** TTS realtime factor (synthesis_ms / audio_duration_ms). */
export function ttsRtf(s: TurnMetricsSnapshot | undefined): number | undefined {
  return extraNumber(componentByKind(s, "tts"), "rtf");
}

/** TTS character count fed to the engine for this turn. */
export function ttsChars(
  s: TurnMetricsSnapshot | undefined,
): number | undefined {
  return extraNumber(componentByKind(s, "tts"), "chars");
}

/** TTS provider id (e.g. "piper"). */
export function ttsProvider(
  s: TurnMetricsSnapshot | undefined,
): string | undefined {
  return componentByKind(s, "tts")?.provider;
}

// ── Realtime S2S helpers ────────────────────────────────────
//
// S2S sessions (OpenAI Realtime, Gemini Live) emit a single component
// with `kind: "realtime"` whose extras carry the only latencies the
// provider exposes: TTFB to the first audio delta, audio output
// duration, accumulated input audio duration, sample rate. There is
// no separate stt/llm/tts split, so callers that need realtime-only
// metrics use these helpers instead of the classic stage lookups.

/** Realtime: time from utterance commit to first audio delta (ms). */
export function realtimeTtfbMs(
  s: TurnMetricsSnapshot | undefined,
): number | undefined {
  return extraNumber(componentByKind(s, "realtime"), "ttfb_ms");
}

/** Realtime: anchor → first output-transcript delta (ms). Diagnostic —
 *  "the model started"; the audio may follow 0.3–1.2 s later (Gemini
 *  buffers its opening audio). Absent on audio-only turns and on
 *  older snapshots that predate this field. */
export function firstTextMs(
  s: TurnMetricsSnapshot | undefined,
): number | undefined {
  return extraNumber(componentByKind(s, "realtime"), "first_text_ms");
}

/** Realtime: where the TTFB / E2E anchor came from (`ttfb_anchor_source`):
 *  `provider_speech_stopped` | `input_transcript_proxy` | `vp_vad` |
 *  `client_speech_end` | `text_submit` | `turn_start` (no anchor — the
 *  TTFB is ≈ 0 and must not be read as latency). Undefined on older
 *  snapshots. */
export function ttfbAnchorSource(
  s: TurnMetricsSnapshot | undefined,
): string | undefined {
  const v = componentByKind(s, "realtime")?.extras?.["ttfb_anchor_source"];
  return typeof v === "string" && v.length > 0 ? v : undefined;
}

/** Realtime: provider anchor − vp's own end-of-speech stamp (ms) — how long
 *  the provider's VAD / transcription took to notice the user stopped. The
 *  S2S counterpart of the cascade `endpointing_ms` (budget 500 ms). */
export function providerEndpointingMs(
  s: TurnMetricsSnapshot | undefined,
): number | undefined {
  return extraNumber(componentByKind(s, "realtime"), "provider_endpointing_ms");
}

/** Realtime: total assistant audio duration emitted for the turn (ms). */
export function realtimeAudioOutputMs(
  s: TurnMetricsSnapshot | undefined,
): number | undefined {
  return extraNumber(componentByKind(s, "realtime"), "audio_output_ms");
}

/** Realtime: accumulated user input audio committed for the turn (ms). */
export function realtimeInputAudioMs(
  s: TurnMetricsSnapshot | undefined,
): number | undefined {
  return extraNumber(componentByKind(s, "realtime"), "input_audio_ms");
}

/** Realtime: provider output sample rate (Hz). */
export function realtimeSampleRate(
  s: TurnMetricsSnapshot | undefined,
): number | undefined {
  return extraNumber(componentByKind(s, "realtime"), "sample_rate");
}

/** True when the snapshot is from a realtime S2S session (single
 *  fused `kind: "realtime"` component, no stt/llm/tts split). */
export function isRealtimeSnapshot(
  s: TurnMetricsSnapshot | undefined,
): boolean {
  return componentByKind(s, "realtime") !== undefined;
}

// ── Output-throughput helpers ──
//
// Both engines attach one `kind: "output"` component per turn describing
// the *streaming phase* (first → last audio chunk, TTFB excluded). The
// `rtf` convention applies: processing time ÷ audio duration, > 1.0 means
// the server emitted audio slower than it plays — the client will starve
// once its prebuffer drains. Each configured inline post-processor adds a
// `kind: "audio_post"` component (`component_id: "audio_post:<name>"`).

/** Streaming-phase realtime factor of what the client actually received:
 *  wall time first → last chunk handed to the transport (post-chain) ÷
 *  `audio_output_ms`. > 1.0 = slower than realtime. */
export function emitRtf(s: TurnMetricsSnapshot | undefined): number | undefined {
  return extraNumber(componentByKind(s, "output"), "emit_rtf");
}

/** Same as `emitRtf` but measured where the audio was *received* from the
 *  provider/TTS (pre-chain): what the model delivered on its own.
 *  `emitRtf − sourceRtf` is the post-processor chain's contribution. */
export function sourceRtf(s: TurnMetricsSnapshot | undefined): number | undefined {
  return extraNumber(componentByKind(s, "output"), "source_rtf");
}

/** The engine's `chain_overhead_ms` is `emit_wall_ms − source_wall_ms`:
 *  everything between receiving audio and handing it to the transport. That
 *  holds the inline post-processors and also the realtime pacer, which keeps
 *  a provider faster than realtime at playback speed. Split it the way the
 *  backend report does: the processors' share is their summed call time
 *  (each `audio_post` component's `latency_ms`), capped at the difference;
 *  the rest is pacer wait. With no processor the share is 0; with
 *  processors that carry no call-time total it cannot be apportioned and
 *  the whole difference stays the chain's. */
function splitChainOverhead(
  s: TurnMetricsSnapshot | undefined,
): { chain: number; pacer: number | undefined } | undefined {
  const raw = extraNumber(componentByKind(s, "output"), "chain_overhead_ms");
  if (raw === undefined) return undefined;
  const posts = audioPostComponents(s);
  if (posts.length === 0) return { chain: 0, pacer: raw };
  let total = 0;
  for (const p of posts) {
    if (typeof p.latency_ms !== "number") return { chain: raw, pacer: undefined };
    total += p.latency_ms;
  }
  const chain = Math.min(raw, Math.round(total));
  return { chain, pacer: raw - chain };
}

/** Wall time (ms) the inline post-processors added between receiving audio
 *  and forwarding it. 0 when the turn ran no processor. */
export function chainOverheadMs(
  s: TurnMetricsSnapshot | undefined,
): number | undefined {
  return splitChainOverhead(s)?.chain;
}

/** The rest of `emit_wall_ms − source_wall_ms` after the post-processors'
 *  share: mostly the realtime pacer holding audio to playback speed. */
export function pacerWaitMs(
  s: TurnMetricsSnapshot | undefined,
): number | undefined {
  return splitChainOverhead(s)?.pacer;
}

/** Every inline post-processor component of the turn (`kind: "audio_post"`),
 *  in snapshot order. Empty when no processor is configured. */
export function audioPostComponents(
  s: TurnMetricsSnapshot | undefined,
): TurnComponentMetrics[] {
  return s?.components.filter((c) => c.kind === "audio_post") ?? [];
}

/** Short name of an `audio_post:<name>` / `annotator:<name>` component:
 *  its `provider` when set, else the part after the first `:`. */
export function componentShortName(c: TurnComponentMetrics): string {
  if (c.provider) return c.provider;
  const i = c.component_id.indexOf(":");
  return i >= 0 ? c.component_id.slice(i + 1) : c.component_id;
}

/** VAD silence wait (ms) — last speech frame → turn-commit decision. */
export function endpointingMs(
  s: TurnMetricsSnapshot | undefined,
): number | undefined {
  return extraNumber(componentByKind(s, "stt"), "endpointing_ms");
}

/** Authoritative LLM output token count (provider-reported `usage`). */
export function llmTokenCount(
  s: TurnMetricsSnapshot | undefined,
): number | undefined {
  return extraNumber(componentByKind(s, "llm"), "token_count");
}

/** Whitespace-split estimate, used as fallback when the provider does
 *  not report `usage`. */
export function llmTokenEstimate(
  s: TurnMetricsSnapshot | undefined,
): number | undefined {
  return extraNumber(componentByKind(s, "llm"), "token_estimate");
}

/** Real count when available, estimate otherwise. */
export function llmTokens(
  s: TurnMetricsSnapshot | undefined,
): number | undefined {
  return llmTokenCount(s) ?? llmTokenEstimate(s);
}
