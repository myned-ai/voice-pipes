import { describe, it, expect } from 'vitest';
import {
  splitAtInterrupt,
  buildInterruptTooltip,
  normalizeLlmTiming,
  lastPipelineStage,
  formatDuration,
  deriveTurnDisplayState,
  isRealtimeSession,
  formatDeviceBadge,
  firstSentenceStartupMs,
  formatMetadataValue,
} from './utils';
import type { ComponentMetricDescriptor } from '../../../lib/transport/types';

// ── splitAtInterrupt ──────────────────────────────────────────────────────────

describe('splitAtInterrupt', () => {
  it('returns full text as heardPart and empty unheardPart when retainedText is undefined', () => {
    const result = splitAtInterrupt('Hello world', undefined);
    expect(result).toEqual({ heardPart: 'Hello world', unheardPart: '' });
  });

  it('returns full text as heardPart and empty unheardPart when retainedText is empty', () => {
    const result = splitAtInterrupt('Hello world', '');
    expect(result).toEqual({ heardPart: 'Hello world', unheardPart: '' });
  });

  it('splits at exact prefix boundary when retained text is a verbatim prefix', () => {
    const full = 'The quick brown fox jumps over the lazy dog';
    const retained = 'The quick brown fox';
    const { heardPart, unheardPart } = splitAtInterrupt(full, retained);
    expect(heardPart).toBe('The quick brown fox');
    expect(unheardPart).toBe('jumps over the lazy dog');
  });

  it('returns empty unheardPart when the full text was completely heard', () => {
    const full = 'Hello there';
    const { heardPart, unheardPart } = splitAtInterrupt(full, full);
    expect(heardPart).toBe(full.trim());
    expect(unheardPart).toBe('');
  });

  it('falls back to word-count split when retained text is not an exact prefix', () => {
    // Simulates whitespace normalisation changing exact match
    const full = 'one two three four five';
    const retained = 'one  two  three'; // extra spaces → not exact prefix
    const { heardPart, unheardPart } = splitAtInterrupt(full, retained);
    expect(heardPart).toBe('one two three');
    expect(unheardPart).toBe('four five');
  });

  it('handles single-word retained text', () => {
    const { heardPart, unheardPart } = splitAtInterrupt('alpha beta gamma', 'alpha');
    expect(heardPart).toBe('alpha');
    expect(unheardPart).toBe('beta gamma');
  });

  it('handles retained text matching all words (word-count path)', () => {
    const full = 'a b c';
    const { unheardPart } = splitAtInterrupt(full, '  a b c  ');
    expect(unheardPart).toBe('');
  });
});

// ── buildInterruptTooltip ─────────────────────────────────────────────────────

describe('buildInterruptTooltip', () => {
  it('returns default string when summary is undefined', () => {
    expect(buildInterruptTooltip(undefined)).toBe('Interrupted here');
  });

  it('returns default string when summary has no relevant fields', () => {
    expect(buildInterruptTooltip({})).toBe('Interrupted here');
  });

  it('includes formatted playback offset', () => {
    const result = buildInterruptTooltip({ last_played_offset_ms: 2500 });
    expect(result).toContain('Interrupted at 2.5s');
  });

  it('includes RTT when barge_in_rtt_ms is present', () => {
    const result = buildInterruptTooltip({ barge_in_rtt_ms: 123.7 });
    expect(result).toContain('RTT 124ms');
  });

  it('includes server_vad trigger label', () => {
    const result = buildInterruptTooltip({ interrupt_trigger: 'server_vad' });
    expect(result).toContain('trigger: server VAD');
  });

  it('includes client_cancel trigger label', () => {
    const result = buildInterruptTooltip({ interrupt_trigger: 'client_cancel' });
    expect(result).toContain('trigger: client cancel');
  });

  it('includes client playback label when client_interrupted is true', () => {
    const result = buildInterruptTooltip({ client_interrupted: true });
    expect(result).toContain('trigger: client playback');
  });

  it('joins multiple parts with ·', () => {
    const result = buildInterruptTooltip({
      last_played_offset_ms: 1000,
      barge_in_rtt_ms: 50,
      interrupt_trigger: 'server_vad',
    });
    expect(result).toBe('Interrupted at 1.0s · RTT 50ms · trigger: server VAD');
  });
});

// ── normalizeLlmTiming ────────────────────────────────────────────────────────

describe('normalizeLlmTiming', () => {
  it('passes through total_ms when already present', () => {
    const result = normalizeLlmTiming({ payload: { total_ms: 500, generate_ms: 300 } });
    expect(result.total_ms).toBe(500);
    expect(result.generate_ms).toBe(300);
  });

  it('falls back to elapsed_ms for both total_ms and generate_ms', () => {
    const result = normalizeLlmTiming({ payload: { elapsed_ms: 400 } });
    expect(result.total_ms).toBe(400);
    expect(result.generate_ms).toBe(400);
  });

  it('extracts flat prompt_tokens', () => {
    const result = normalizeLlmTiming({ payload: { prompt_tokens: 128 } });
    expect(result.prompt_tokens).toBe(128);
  });

  it('extracts prompt_tokens from nested usage object', () => {
    const result = normalizeLlmTiming({ payload: { usage: { prompt_tokens: 64 } } });
    expect(result.prompt_tokens).toBe(64);
  });

  it('extracts completion_tokens from nested usage as output_tokens', () => {
    const result = normalizeLlmTiming({ payload: { usage: { completion_tokens: 32 } } });
    expect(result.output_tokens).toBe(32);
  });

  it('falls back to legacy tokens field for output_tokens', () => {
    const result = normalizeLlmTiming({ payload: { tokens: 20 } });
    expect(result.output_tokens).toBe(20);
  });

  it('computes tokens_per_sec when not present', () => {
    // 60 tokens in 3000ms = 20 tok/s
    const result = normalizeLlmTiming({ payload: { generate_ms: 3000, output_tokens: 60 } });
    expect(result.tokens_per_sec).toBe(20);
  });

  it('passes through existing tokens_per_sec without recomputing', () => {
    const result = normalizeLlmTiming({ payload: { tokens_per_sec: 99, generate_ms: 1000, output_tokens: 10 } });
    expect(result.tokens_per_sec).toBe(99);
  });

  it('handles an empty payload gracefully', () => {
    const result = normalizeLlmTiming({ payload: {} });
    expect(result.total_ms).toBeUndefined();
    expect(result.output_tokens).toBeUndefined();
    expect(result.tokens_per_sec).toBeUndefined();
  });

  it('handles a missing payload gracefully', () => {
    const result = normalizeLlmTiming({});
    expect(result.total_ms).toBeUndefined();
  });
});

// ── lastPipelineStage ─────────────────────────────────────────────────────────

describe('lastPipelineStage', () => {
  it('returns TTS when tts_frag audio files are present', () => {
    expect(lastPipelineStage({ audio_files: { tts_frag_0: 'a.wav', mic_raw: 'b.wav' } }))
      .toBe('TTS');
  });

  it('returns LLM when llm_output is present but no TTS audio', () => {
    expect(lastPipelineStage({ llm_output: { text: 'hi' } })).toBe('LLM');
  });

  it('returns LLM input when only llm_input is present', () => {
    expect(lastPipelineStage({ llm_input: 'prompt text' })).toBe('LLM input');
  });

  it('returns STT when only stt is present', () => {
    expect(lastPipelineStage({ stt: { text: 'hello' } })).toBe('STT');
  });

  it('returns start when no artifacts are present', () => {
    expect(lastPipelineStage({})).toBe('start');
  });

  it('ignores non-tts_frag audio files', () => {
    expect(lastPipelineStage({ audio_files: { mic_raw: 'a.wav' } })).toBe('start');
  });

  it('returns Realtime when isRealtime=true and no classic artefacts are present', () => {
    expect(lastPipelineStage({}, true)).toBe('Realtime');
  });

  it('still prefers a classic stage label even when isRealtime=true (mixed turns)', () => {
    expect(lastPipelineStage({ stt: { text: 'hi' } }, true)).toBe('STT');
  });
});

// ── isRealtimeSession ───────────────────────────────────────────────

describe('isRealtimeSession', () => {
  const stt: ComponentMetricDescriptor = { component_id: 'stt', kind: 'stt', label: 'STT', fields: [] };
  const llm: ComponentMetricDescriptor = { component_id: 'llm', kind: 'llm', label: 'LLM', fields: [] };
  const tts: ComponentMetricDescriptor = { component_id: 'tts', kind: 'tts', label: 'TTS', fields: [] };
  const realtime: ComponentMetricDescriptor = { component_id: 'realtime', kind: 'realtime', label: 'Realtime S2S', fields: [] };

  it('returns false for undefined metadata', () => {
    expect(isRealtimeSession(undefined)).toBe(false);
  });

  it('returns false when descriptors are empty / missing', () => {
    expect(isRealtimeSession({})).toBe(false);
    expect(isRealtimeSession({ metric_descriptors: [] })).toBe(false);
  });

  it('returns false for the classic stt/llm/tts triple', () => {
    expect(isRealtimeSession({ metric_descriptors: [stt, llm, tts] })).toBe(false);
  });

  it('returns true when any descriptor has kind="realtime"', () => {
    expect(isRealtimeSession({ metric_descriptors: [realtime] })).toBe(true);
  });
});

// ── formatDuration ────────────────────────────────────────────────────────────

describe('formatDuration', () => {
  it('returns em dash when endedAt is null', () => {
    expect(formatDuration('2024-01-01T00:00:00Z', null)).toBe('—');
  });

  it('formats sub-second durations in milliseconds', () => {
    const start = '2024-01-01T00:00:00.000Z';
    const end = '2024-01-01T00:00:00.500Z';
    expect(formatDuration(start, end)).toBe('500ms');
  });

  it('formats multi-second durations as decimal seconds', () => {
    const start = '2024-01-01T00:00:00.000Z';
    const end = '2024-01-01T00:00:03.500Z';
    expect(formatDuration(start, end)).toBe('3.5s');
  });

  it('formats exactly 1 second as 1.0s', () => {
    const start = '2024-01-01T00:00:00.000Z';
    const end = '2024-01-01T00:00:01.000Z';
    expect(formatDuration(start, end)).toBe('1.0s');
  });
});

// ── Real-world interruption scenario (matches the screenshot) ─────────────────
//
// The session shows a VAD barge-in turn where the LLM had generated a full story
// but the user interrupted after the first sentence ("…lived a curious girl named
// Lila. She").  The server sets retained_text to the heard prefix and the UI must:
//   1. Split correctly so only the unheard tail gets a strikethrough.
//   2. Build a tooltip that includes the RTT and "trigger: server VAD".

const STORY_FULL =
  'Once upon a time, in a small village nestled between two mountains, lived a curious girl named Lila. She ' +
  'loved to explore the woods and dreamt of finding a hidden treasure. ' +
  'One sunny morning, Lila stumbled upon an ancient map tucked inside a hollow tree. ' +
  'It depicted a route leading to a legendary treasure said to grant one wish. ' +
  "Excited, she set off on her adventure, facing challenges along the way—crossing a rickety bridge, " +
  "solving puzzles posed by a wise old owl, and befriending a timid fox who guided her through the thick forest. " +
  "After a long journey, Lila reached a sparkling cave. Inside, she found a chest overflowing with gold coins and jewels. " +
  "But instead of taking the treasure, she made a wish for her village to prosper and thrive. " +
  "As she returned home, she noticed the crops were flourishing, and the village was filled with joy. " +
  "Lila realized the true treasure was the happiness she brought to her community. " +
  "From that day on, she shared her adventures, inspiring others to seek their own paths of kindness and courage. " +
  "And so, the village thrived, all thanks to one curious girl's selfless wish. The end.";

const STORY_RETAINED =
  'Once upon a time, in a small village nestled between two mountains, lived a curious girl named Lila. She';

describe('splitAtInterrupt — real-world VAD barge-in scenario', () => {
  it('splits story text at the exact heard prefix', () => {
    const { heardPart, unheardPart } = splitAtInterrupt(STORY_FULL, STORY_RETAINED);
    expect(heardPart).toBe(STORY_RETAINED.trim());
    expect(unheardPart).toMatch(/^loved to explore/);
  });

  it('heardPart + " " + unheardPart reconstructs the full text', () => {
    const { heardPart, unheardPart } = splitAtInterrupt(STORY_FULL, STORY_RETAINED);
    expect(`${heardPart} ${unheardPart}`).toBe(STORY_FULL);
  });

  it('unheardPart does not contain the heard prefix', () => {
    const { heardPart, unheardPart } = splitAtInterrupt(STORY_FULL, STORY_RETAINED);
    expect(unheardPart).not.toContain(heardPart.slice(0, 20));
  });

  it('produces non-empty unheardPart for a mid-sentence interrupt', () => {
    const { unheardPart } = splitAtInterrupt(STORY_FULL, STORY_RETAINED);
    expect(unheardPart.length).toBeGreaterThan(0);
  });
});

describe('buildInterruptTooltip — real-world VAD barge-in scenario', () => {
  // Mirrors the header chips visible in the screenshot:
  // RTT 115ms, TTFT 5796ms, TTFB 6113ms — server_vad triggered
  const screenshotSummary = {
    interrupted: true,
    interrupt_trigger: 'server_vad' as const,
    barge_in_rtt_ms: 115,
    last_played_offset_ms: 6113,
  };

  it('tooltip contains the playback offset from the screenshot', () => {
    const tip = buildInterruptTooltip(screenshotSummary);
    expect(tip).toContain('Interrupted at 6.1s');
  });

  it('tooltip contains the RTT from the screenshot', () => {
    const tip = buildInterruptTooltip(screenshotSummary);
    expect(tip).toContain('RTT 115ms');
  });

  it('tooltip contains the server VAD trigger label', () => {
    const tip = buildInterruptTooltip(screenshotSummary);
    expect(tip).toContain('trigger: server VAD');
  });

  it('tooltip parts are joined with ·', () => {
    const tip = buildInterruptTooltip(screenshotSummary);
    expect(tip).toBe('Interrupted at 6.1s · RTT 115ms · trigger: server VAD');
  });
});

// ── deriveTurnDisplayState ───────────────────────────────────────────────────

describe('deriveTurnDisplayState', () => {
  it('returns "completed" when summary is undefined', () => {
    expect(deriveTurnDisplayState(undefined)).toBe('completed');
  });

  it('returns "completed" when no interruption flags are set', () => {
    expect(deriveTurnDisplayState({ ttfb_ms: 120 })).toBe('completed');
  });

  it('returns "interrupted" for server_vad barge-in', () => {
    expect(
      deriveTurnDisplayState({
        interrupted: true,
        interrupt_trigger: 'server_vad',
      }),
    ).toBe('interrupted');
  });

  it('returns "interrupted" for client_cancel trigger even without interrupted flag', () => {
    expect(
      deriveTurnDisplayState({ interrupt_trigger: 'client_cancel' }),
    ).toBe('interrupted');
  });

  it('returns "playback_barge_in" when only client_interrupted is set', () => {
    expect(
      deriveTurnDisplayState({ client_interrupted: true }),
    ).toBe('playback_barge_in');
  });

  it('returns "interrupted" when both server interrupted and client_interrupted are set', () => {
    // Server interruption takes precedence over client playback barge-in.
    expect(
      deriveTurnDisplayState({
        interrupted: true,
        interrupt_trigger: 'server_vad',
        client_interrupted: true,
      }),
    ).toBe('interrupted');
  });

  it('returns "terminated" even when other interruption flags are also set', () => {
    // terminated is the highest-precedence state.
    expect(
      deriveTurnDisplayState({
        terminated: true,
        interrupted: true,
        client_interrupted: true,
        interrupt_trigger: 'server_vad',
      }),
    ).toBe('terminated');
  });

  it('returns "terminated" for a bare terminated turn', () => {
    expect(deriveTurnDisplayState({ terminated: true })).toBe('terminated');
  });
});

// ── formatDeviceBadge ─────────────────────────────────────────────────────────

describe('formatDeviceBadge', () => {
  it('returns null when no device is reported', () => {
    expect(formatDeviceBadge(undefined)).toBeNull();
    expect(formatDeviceBadge(null)).toBeNull();
    // kind is required; a kindless object is not a badge.
    expect(formatDeviceBadge({ detail: 'x' } as never)).toBeNull();
  });

  it('marks a confirmed GPU with the gpu tone', () => {
    const b = formatDeviceBadge({ kind: 'cuda', detail: '28/28 layers', source: 'probe:/props' });
    expect(b).not.toBeNull();
    expect(b!.label).toBe('cuda');
    expect(b!.tone).toBe('gpu');
    expect(b!.title).toContain('28/28 layers');
    expect(b!.title).toContain('source: probe:/props');
  });

  it('marks CPU with the muted cpu tone', () => {
    const b = formatDeviceBadge({ kind: 'cpu', source: 'static' });
    expect(b!.label).toBe('cpu');
    expect(b!.tone).toBe('cpu');
  });

  it('renders an env-requested GPU as unconfirmed (label gets ? and amber tone)', () => {
    // A requested GPU setting is not proof it was used.
    const b = formatDeviceBadge({ kind: 'cuda', source: 'env-requested' });
    expect(b!.label).toBe('cuda?');
    expect(b!.tone).toBe('unconfirmed');
  });

  it('suppresses the ngl detail for an env-requested device', () => {
    // `ngl=99` is the requested GPU-layer config, not confirmed usage — it must
    // not appear in the tooltip. Source provenance still shows.
    const b = formatDeviceBadge({ kind: 'cuda', detail: 'ngl=99', source: 'env-requested' });
    expect(b!.title).not.toContain('ngl');
    expect(b!.title).toContain('source: env-requested');
    expect(b!.title).toBe('cuda · source: env-requested');
  });

  it('renders an unknown device as a muted device? badge', () => {
    const b = formatDeviceBadge({ kind: 'unknown', source: 'unknown' });
    expect(b!.label).toBe('device?');
    expect(b!.tone).toBe('unknown');
  });
});

describe('firstSentenceStartupMs', () => {
  it('prefers ttfb − tts_internal_ttfb when both are present', () => {
    // Timeline & footer must agree: this is the canonical numerator.
    expect(firstSentenceStartupMs({ ttfbMs: 2790, ttsInternalTtfbMs: 2548, ttftMs: 198 })).toBe(242);
  });

  it('falls back to ttft when the tts-internal split is missing', () => {
    expect(firstSentenceStartupMs({ ttftMs: 198, llmFirstTokenMs: 150 })).toBe(198);
  });

  it('falls back to first-token when ttft is absent', () => {
    expect(firstSentenceStartupMs({ llmFirstTokenMs: 150 })).toBe(150);
  });

  it('never returns a negative startup', () => {
    expect(firstSentenceStartupMs({ ttfbMs: 100, ttsInternalTtfbMs: 300 })).toBe(0);
  });

  it('returns undefined when nothing is known', () => {
    expect(firstSentenceStartupMs({})).toBeUndefined();
  });
});

// ── formatMetadataValue ───────────────────────────────────────────────────────

describe('formatMetadataValue', () => {
  it('drops values that carry no information', () => {
    expect(formatMetadataValue('llm_device', null)).toBeNull();
    expect(formatMetadataValue('llm_device', undefined)).toBeNull();
    expect(formatMetadataValue('tags', [])).toBeNull();
    expect(formatMetadataValue('extra', {})).toBeNull();
    expect(formatMetadataValue('note', '   ')).toBeNull();
  });

  it('keeps falsy-but-meaningful values', () => {
    expect(formatMetadataValue('turn_count', 0)?.text).toBe('0');
    expect(formatMetadataValue('persistent_session', false)?.text).toBe('no');
  });

  it('renders ISO timestamps as local date + time with the raw value in the tooltip', () => {
    const iso = '2026-08-29T20:11:31.238439725+00:00';
    const out = formatMetadataValue('start_time', iso);
    const d = new Date(iso);
    expect(out?.title).toBe(iso);
    // Local formatting — assert on the parsed components, not a fixed string,
    // so the test doesn't depend on the runner's timezone.
    expect(out?.text).toContain(String(d.getFullYear()));
    expect(out?.text).toMatch(/^[A-Z][a-z]{2} \d{1,2}, \d{4}, \d{2}:\d{2}:\d{2}$/);
  });

  it('adds a relative hint for recent timestamps only', () => {
    const recent = new Date(Date.now() - 5 * 60_000).toISOString();
    expect(formatMetadataValue('ended_at', recent)?.hint).toBe('5 min ago');
    expect(formatMetadataValue('ended_at', '2020-01-02T03:04:05Z')?.hint).toBeUndefined();
  });

  it('leaves non-timestamp strings alone', () => {
    expect(formatMetadataValue('pipeline_name', 'custom-realtime')).toEqual({ text: 'custom-realtime' });
  });

  it('flattens {id, source} model descriptors', () => {
    const out = formatMetadataValue('llm_model', { id: 'gemini-2.5-flash-native-audio-latest', source: 'config' });
    expect(out).toEqual({ text: 'gemini-2.5-flash-native-audio-latest', hint: 'config' });
  });

  it('falls back to a JSON block for richer objects', () => {
    const out = formatMetadataValue('llm_model', { id: 'x', source: 'config', quantization: 'q4' });
    expect(out?.isBlock).toBe(true);
    expect(out?.text).toContain('quantization');
  });

  it('formats sample rates in kHz and keeps the raw Hz in the tooltip', () => {
    expect(formatMetadataValue('input_sample_rate', 48000)).toEqual({ text: '48 kHz', title: '48000 Hz' });
    expect(formatMetadataValue('output_sample_rate', 22050)?.text).toBe('22.1 kHz');
  });

  it('joins scalar arrays and JSON-blocks object arrays', () => {
    expect(formatMetadataValue('tags', ['prod', 'eu'])?.text).toBe('prod, eu');
    const out = formatMetadataValue('hooks', [{ name: 'a' }]);
    expect(out?.isBlock).toBe(true);
  });
});
