// Pure utility functions extracted from SessionDetail.tsx.
// Being side-effect-free, these are straightforward to unit-test in isolation.

import type { DeviceInfo, LlmTiming, SessionEvent, SessionMetadata, TurnDisplayState, TurnSummary, Utterance } from './types';

// ── Text / interruption helpers ──────────────────────────────────────────────

/**
 * Split an agent's full response at the interruption point.
 *
 * Returns `{ heardPart, unheardPart }`.  When there was no interruption
 * (retainedText is undefined/empty) `unheardPart` is an empty string and
 * `heardPart` equals the full text.
 *
 * Strategy:
 *  1. Exact prefix match — preferred when the server retained text verbatim.
 *  2. Word-count fallback — when whitespace normalisation or trimming means
 *     the retained text is not an exact prefix of the full text.
 */
export function splitAtInterrupt(
  fullText: string,
  retainedText: string | undefined,
): { heardPart: string; unheardPart: string } {
  if (!retainedText) return { heardPart: fullText, unheardPart: '' };

  const trimmed = retainedText.trim();

  if (fullText.startsWith(trimmed)) {
    return {
      heardPart: trimmed,
      unheardPart: fullText.slice(trimmed.length).trimStart(),
    };
  }

  // Fallback: split by word count
  const heardCount = trimmed.split(/\s+/).filter(Boolean).length;
  const allWords = fullText.trim().split(/\s+/);
  return {
    heardPart: allWords.slice(0, heardCount).join(' '),
    unheardPart: allWords.slice(heardCount).join(' '),
  };
}

/**
 * Build the ✂ tooltip title for an interrupted agent turn.
 * Combines playback offset, RTT, and trigger type into a readable string.
 */
export function buildInterruptTooltip(summary: TurnSummary | undefined): string {
  if (!summary) return 'Interrupted here';

  const parts: string[] = [];
  if (summary.last_played_offset_ms != null) {
    parts.push(`Interrupted at ${(summary.last_played_offset_ms / 1000).toFixed(1)}s`);
  }
  if (summary.barge_in_rtt_ms != null) {
    parts.push(`RTT ${Math.round(summary.barge_in_rtt_ms)}ms`);
  }
  if (summary.interrupt_trigger === 'server_vad') {
    parts.push('trigger: server VAD');
  } else if (summary.interrupt_trigger === 'client_cancel') {
    parts.push('trigger: client cancel');
  } else if (summary.client_interrupted) {
    parts.push('trigger: client playback');
  }

  return parts.join(' · ') || 'Interrupted here';
}

// ── LLM event normalisation ──────────────────────────────────────────────────

/**
 * Normalise an `llm_timing` event payload into a consistent `LlmTiming` shape.
 *
 * The Rust worker emits `elapsed_ms` + nested `usage.{prompt,completion}_tokens`,
 * while the Python / OpenAI workers emit `total_ms` + flat `prompt_tokens`.
 * This function reconciles both variants so callers only need to handle one shape.
 */
export function normalizeLlmTiming(event: SessionEvent): LlmTiming {
  const raw = (event.payload ?? {}) as Record<string, unknown>;
  const usage = (raw.usage ?? {}) as Record<string, unknown>;

  const promptTokens =
    (raw.prompt_tokens as number | null) ??
    (usage.prompt_tokens as number | null) ??
    null;
  const outputTokens =
    (raw.output_tokens as number | null) ??
    (usage.completion_tokens as number | null) ??
    (usage.output_tokens as number | null) ??
    (raw.tokens as number | null) ??
    null;
  const totalMs =
    (raw.total_ms as number | null) ?? (raw.elapsed_ms as number | null) ?? null;
  const generateMs =
    (raw.generate_ms as number | null) ?? (raw.elapsed_ms as number | null) ?? null;
  const tokensPerSec =
    (raw.tokens_per_sec as number | null) ??
    (outputTokens != null && generateMs != null && generateMs > 0
      ? Math.round((outputTokens / (generateMs / 1000)) * 10) / 10
      : null);

  return {
    ...raw,
    total_ms: totalMs ?? undefined,
    generate_ms: generateMs ?? undefined,
    prompt_tokens: promptTokens ?? undefined,
    output_tokens: outputTokens ?? undefined,
    tokens_per_sec: tokensPerSec ?? undefined,
  } as LlmTiming;
}

// ── Turn / session helpers ───────────────────────────────────────────────────

/**
 * Determine the last pipeline stage that produced artifacts for a turn.
 * Used to describe terminated turns in the UI ("Terminated after STT" etc.).
 *
 * `isRealtime` (optional) flips the function into S2S mode: realtime
 * sessions collapse stt+llm+tts into a single fused stage and don't
 * produce `tts_frag_*` captures or separate llm output objects, so we
 * surface the synthetic `'Realtime'` stage label instead of falling
 * through to `'start'`.
 */
export function lastPipelineStage(
  utt: Pick<Utterance, 'audio_files' | 'llm_output' | 'llm_input' | 'stt'>,
  isRealtime: boolean = false,
): string {
  if (utt.audio_files && Object.keys(utt.audio_files).some(k => k.startsWith('tts_frag'))) {
    return 'TTS';
  }
  if (utt.llm_output) return 'LLM';
  if (utt.llm_input) return 'LLM input';
  if (utt.stt) return 'STT';
  if (isRealtime) return 'Realtime';
  return 'start';
}

/**
 * True when this session was driven by a realtime S2S provider
 * (OpenAI Realtime, Gemini Live). Detected via the persisted
 * `metric_descriptors` registry rather than a brittle field on
 * `metadata`, so any future provider that registers a `"realtime"`
 * component descriptor automatically lights up the right UI path.
 */
export function isRealtimeSession(metadata: SessionMetadata | undefined): boolean {
  const descriptors = metadata?.metric_descriptors;
  if (!descriptors || descriptors.length === 0) return false;
  return descriptors.some(d => d.kind === 'realtime');
}

/**
 * Format a human-readable session duration from two ISO timestamps.
 * Returns '—' when endedAt is null (session still in progress).
 */
export function formatDuration(startedAt: string, endedAt: string | null): string {
  if (!endedAt) return '—';
  const ms = new Date(endedAt).getTime() - new Date(startedAt).getTime();
  if (ms < 1000) return `${ms}ms`;
  return `${(ms / 1000).toFixed(1)}s`;
}

const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];

/** "2 min ago" + an absolute second form ("14:32:08" today, "Jul 4, 18:47" otherwise). */
export function formatStarted(iso: string): { rel: string; abs: string } {
  const d = new Date(iso);
  const now = new Date();
  const diffMs = now.getTime() - d.getTime();
  const pad = (n: number) => String(n).padStart(2, '0');
  const isToday = d.toDateString() === now.toDateString();
  const yesterday = new Date(now);
  yesterday.setDate(now.getDate() - 1);
  const isYesterday = d.toDateString() === yesterday.toDateString();

  const abs = isToday
    ? `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`
    : `${MONTHS[d.getMonth()]} ${d.getDate()}, ${pad(d.getHours())}:${pad(d.getMinutes())}`;

  let rel: string;
  if (diffMs < 60_000) rel = 'just now';
  else if (diffMs < 3_600_000) rel = `${Math.floor(diffMs / 60_000)} min ago`;
  else if (isToday) rel = `${Math.floor(diffMs / 3_600_000)} h ago`;
  else if (isYesterday) rel = 'Yesterday';
  else if (d.getFullYear() === now.getFullYear()) rel = `${MONTHS[d.getMonth()]} ${d.getDate()}`;
  else rel = `${MONTHS[d.getMonth()]} ${d.getDate()}, ${d.getFullYear()}`;

  return { rel, abs };
}

// ── Session-metadata rendering ───────────────────────────────────────────────

/** Matches an ISO-8601 timestamp, with or without fractional seconds / offset. */
const ISO_TIMESTAMP_RE =
  /^\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}(\.\d+)?(Z|[+-]\d{2}:?\d{2})?$/;

/** True for values that carry no information and should be omitted entirely
 *  (null/undefined, blank strings, empty arrays, empty objects). */
export function isEmptyMetadataValue(value: unknown): boolean {
  if (value === null || value === undefined) return true;
  if (typeof value === 'string') return value.trim() === '';
  if (Array.isArray(value)) return value.length === 0;
  if (typeof value === 'object') return Object.keys(value as object).length === 0;
  return false;
}

/** "Aug 29, 2026, 22:11:31" in the viewer's local timezone. */
function formatAbsoluteTimestamp(d: Date): string {
  const pad = (n: number) => String(n).padStart(2, '0');
  return (
    `${MONTHS[d.getMonth()]} ${d.getDate()}, ${d.getFullYear()}, ` +
    `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`
  );
}

export interface MetadataDisplayValue {
  /** Primary text for the value column. */
  text: string;
  /** Muted suffix — provenance ("config"), relative time ("18 min ago"), … */
  hint?: string;
  /** Hover title carrying the raw/exact value the display form rounded away. */
  title?: string;
  /** True when `text` is a multi-line JSON blob needing a <pre> block. */
  isBlock?: boolean;
}

/**
 * Render one `session.metadata` entry for the Session Metadata panel.
 *
 * Returns `null` when the entry carries nothing worth a row — the panel skips
 * it rather than printing `null` / `[]` / `{}`.
 *
 * Beyond that it applies a few presentation rules, in order:
 *  - ISO timestamps → local "Aug 29, 2026, 22:11:31" + a relative hint when
 *    they're within the last day, with the raw string kept in the tooltip.
 *  - `{ id, source }` model descriptors → the bare id + a muted source, instead
 *    of a three-line JSON blob.
 *  - `*_sample_rate` numbers → "48 kHz", raw Hz in the tooltip.
 *  - arrays of scalars → "a, b, c"; anything else object-shaped → pretty JSON.
 */
export function formatMetadataValue(key: string, value: unknown): MetadataDisplayValue | null {
  if (isEmptyMetadataValue(value)) return null;

  if (typeof value === 'string') {
    if (ISO_TIMESTAMP_RE.test(value.trim())) {
      const d = new Date(value);
      if (!Number.isNaN(d.getTime())) {
        const diffMs = Date.now() - d.getTime();
        // Only worth a relative hint while it's still recent; beyond a day the
        // absolute form already reads fine on its own.
        const hint =
          diffMs >= 0 && diffMs < 86_400_000 ? formatStarted(value).rel : undefined;
        return { text: formatAbsoluteTimestamp(d), hint, title: value };
      }
    }
    return { text: value };
  }

  if (typeof value === 'number') {
    if (key.endsWith('sample_rate')) {
      return { text: `${Math.round(value / 100) / 10} kHz`, title: `${value} Hz` };
    }
    return { text: String(value) };
  }

  if (typeof value === 'boolean') return { text: value ? 'yes' : 'no' };

  if (Array.isArray(value)) {
    if (value.every(v => typeof v === 'string' || typeof v === 'number' || typeof v === 'boolean')) {
      return { text: value.join(', ') };
    }
    return { text: JSON.stringify(value, null, 2), isBlock: true };
  }

  if (typeof value === 'object') {
    const obj = value as Record<string, unknown>;
    // `{"id": "...", "source": "config"}` model/device descriptors: the id is
    // the answer, the source is provenance — no reason to spend three lines.
    if (typeof obj.id === 'string' && obj.id !== '') {
      const rest = Object.keys(obj).filter(k => k !== 'id' && k !== 'source');
      if (rest.length === 0) {
        return {
          text: obj.id,
          hint: typeof obj.source === 'string' ? obj.source : undefined,
        };
      }
    }
    return { text: JSON.stringify(value, null, 2), isBlock: true };
  }

  return { text: String(value) };
}

/**
 * Single source of truth for the "LLM first-sentence startup" time (ms),
 * whose per-character rate (`startupMs / first_sentence_chars`) is shown both in
 * the pipeline-timeline header and the per-turn stats footer. They previously
 * used different numerators (`ttfb − tts_internal_ttfb` vs `ttft`) and so
 * disagreed for the same turn. Both now call this.
 *
 * Numerator = time until the first sentence was ready for TTS: preferably
 * `ttfb − tts_internal_ttfb` (first audio minus TTS-internal startup), falling
 * back to `ttft` / first-token when that split isn't captured.
 */
export function firstSentenceStartupMs(opts: {
  ttfbMs?: number;
  ttsInternalTtfbMs?: number;
  ttftMs?: number;
  llmFirstTokenMs?: number;
}): number | undefined {
  const { ttfbMs, ttsInternalTtfbMs, ttftMs, llmFirstTokenMs } = opts;
  if (ttfbMs != null && ttsInternalTtfbMs != null) {
    return Math.max(0, ttfbMs - ttsInternalTtfbMs);
  }
  return ttftMs ?? llmFirstTokenMs;
}

// ── Turn display state ───────────────────────────────────────────────────────

/**
 * Derive the single-valued UI display state for a turn from its summary flags.
 *
 * Multiple boolean flags can co-occur on a `TurnSummary` (e.g. a turn may be
 * both `terminated` and `client_interrupted`). The UI needs exactly one state
 * per turn, so we apply a fixed precedence:
 *
 *   terminated  >  interrupted  >  playback_barge_in  >  completed
 *
 * - `terminated`: server aborted the turn (failure path).
 * - `interrupted`: server-side barge-in stopped LLM/TTS generation.
 *   Detected via `interrupted === true` OR an explicit `interrupt_trigger` of
 *   `server_vad` / `client_cancel` (server-mediated cancellation).
 * - `playback_barge_in`: client-only interruption — `client_interrupted === true`
 *   without any server-side interruption. The turn completed on the server but
 *   the client stopped audio playback early. This is NOT a failure.
 * - `completed`: no interruption flags set.
 */
export function deriveTurnDisplayState(
  summary: TurnSummary | undefined,
): TurnDisplayState {
  if (!summary) return 'completed';

  if (summary.terminated) return 'terminated';

  const serverInterrupted =
    summary.interrupted === true ||
    summary.interrupt_trigger === 'server_vad' ||
    summary.interrupt_trigger === 'client_cancel';
  if (serverInterrupted) return 'interrupted';

  if (summary.client_interrupted) return 'playback_barge_in';

  return 'completed';
}

// ── Device badge ─────────────────────────────────────────────────────────────

/** Semantic colour bucket for a device badge; the component maps it to theme
 *  classes. `unconfirmed` = a GPU that was requested but not verified. */
export type DeviceTone = 'gpu' | 'cpu' | 'unconfirmed' | 'unknown';

export interface DeviceBadge {
  /** Short label, e.g. `cuda`, `cpu`, `gpu?` (unconfirmed), `device?` (unknown). */
  label: string;
  tone: DeviceTone;
  /** Hover text with kind, detail, and provenance. */
  title: string;
}

/**
 * Format a component's resolved {@link DeviceInfo} into a badge, or `null` when
 * there's nothing to show (no device reported). Pure — returns a semantic tone
 * rather than CSS so it's trivial to unit-test and theme-agnostic.
 *
 * Honesty rules: a `source: 'env-requested'` device is rendered as *unconfirmed*
 * (label gets a `?`, amber tone) because a requested GPU setting is not proof it
 * was used; `kind: 'unknown'` renders as a muted `device?` rather than guessing.
 */
export function formatDeviceBadge(device?: DeviceInfo | null): DeviceBadge | null {
  if (!device || !device.kind) return null;
  const kind = device.kind.toLowerCase();
  const isGpu = kind === 'cuda' || kind === 'gpu' || kind === 'metal';
  const unconfirmed = device.source === 'env-requested';

  let tone: DeviceTone;
  if (kind === 'unknown') tone = 'unknown';
  else if (unconfirmed) tone = 'unconfirmed';
  else if (isGpu) tone = 'gpu';
  else tone = 'cpu';

  const label = (kind === 'unknown' ? 'device?' : kind) + (unconfirmed ? '?' : '');
  // Suppress `detail` for env-requested devices: it carries the requested
  // config (e.g. `ngl=99`, the GPU-layer count), not confirmed usage, so it's
  // noise in the tooltip. Genuinely-probed details (e.g. `28/28 layers`) stay.
  const detail = unconfirmed ? null : device.detail;
  const title = [device.kind, detail, device.source ? `source: ${device.source}` : null]
    .filter(Boolean)
    .join(' · ');
  return { label, tone, title };
}
