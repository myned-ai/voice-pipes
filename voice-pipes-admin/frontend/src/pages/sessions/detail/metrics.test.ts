import { describe, it, expect } from 'vitest';
import { computeSessionMetrics } from './metrics';
import type { SessionEvent, Utterance } from './types';

// Minimal utterance helpers ─────────────────────────────────────────────────

const completedUtt = (id: number, summary: Utterance['summary'] = {}): Utterance => ({
  utterance_id: id,
  summary: { ttft_ms: 100, ttfb_ms: 200, ...summary },
});

const event = (
  type: string,
  payload: Record<string, unknown>,
): SessionEvent => ({ event_type: type, payload });

// ── tts_detail / tts_timing aggregation ─────────────────────────────────────

describe('computeSessionMetrics — TTS aggregation', () => {
  it('aggregates audio + RTF from tts_detail events', () => {
    const utterances = [completedUtt(1), completedUtt(2)];
    const events: SessionEvent[] = [
      event('tts_detail', { audio_duration_s: 1.5, rtf: 0.4 }),
      event('tts_detail', { audio_duration_s: 2.5, rtf: 0.6 }),
    ];

    const m = computeSessionMetrics(utterances, events);
    expect(m.totalAudioSeconds).toBe(4.0);
    expect(m.avgRtf).toBe(0.5);
  });

  it('aggregates audio + RTF from tts_timing events', () => {
    const utterances = [completedUtt(1)];
    const events: SessionEvent[] = [
      event('tts_timing', { audio_duration_s: 3.0, rtf: 0.3 }),
    ];

    const m = computeSessionMetrics(utterances, events);
    expect(m.totalAudioSeconds).toBe(3.0);
    expect(m.avgRtf).toBe(0.3);
  });

  it('aggregates from a mixture of tts_detail and tts_timing events', () => {
    // Both event types must contribute to TTS aggregates.
    // Some providers emit only tts_detail, others only tts_timing, others both.
    const utterances = [completedUtt(1), completedUtt(2)];
    const events: SessionEvent[] = [
      event('tts_detail', { audio_duration_s: 1.0, rtf: 0.2 }),
      event('tts_timing', { audio_duration_s: 4.0, rtf: 0.8 }),
    ];

    const m = computeSessionMetrics(utterances, events);
    expect(m.totalAudioSeconds).toBe(5.0);
    expect(m.avgRtf).toBe(0.5);
  });

  it('returns 0 / null when no TTS events exist', () => {
    const m = computeSessionMetrics([completedUtt(1)], []);
    expect(m.totalAudioSeconds).toBe(0);
    expect(m.avgRtf).toBeNull();
  });

  it('ignores TTS events with missing audio_duration_s', () => {
    const utterances = [completedUtt(1)];
    const events: SessionEvent[] = [
      event('tts_detail', { rtf: 0.5 }),
      event('tts_detail', { audio_duration_s: 2.0, rtf: 0.5 }),
    ];

    const m = computeSessionMetrics(utterances, events);
    expect(m.totalAudioSeconds).toBe(2.0);
    expect(m.avgRtf).toBe(0.5);
  });

  it('prefers canonical turn snapshot audio duration over raw events to prevent double counting', () => {
    const snapUtt = (id: number, audioMs: number, rtf: number): Utterance => ({
      utterance_id: id,
      summary: { ttft_ms: 100, ttfb_ms: 200 },
      metric_snapshot: {
        utterance_id: id,
        total_ms: 1000,
        interrupted: false,
        components: [
          {
            component_id: 'tts:piper',
            kind: 'tts',
            started_at_ms: 100,
            latency_ms: 250,
            extras: {
              audio_duration_ms: audioMs,
              rtf,
            },
          },
        ],
      },
    });

    const utterances = [snapUtt(1, 2000, 0.4), snapUtt(2, 3000, 0.6)];
    // Events contain both tts_detail AND tts_timing (which previously caused 2x double counting)
    const events: SessionEvent[] = [
      event('tts_detail', { audio_duration_s: 2.0, rtf: 0.4 }),
      event('tts_timing', { audio_duration_s: 2.0, rtf: 0.4 }),
      event('tts_detail', { audio_duration_s: 3.0, rtf: 0.6 }),
      event('tts_timing', { audio_duration_s: 3.0, rtf: 0.6 }),
    ];

    const m = computeSessionMetrics(utterances, events);
    // Must be 5.0s (2.0 + 3.0) from snapshots, NOT 10.0s from double-counted events!
    expect(m.totalAudioSeconds).toBe(5.0);
    expect(m.avgRtf).toBe(0.5);
  });

  it('falls back to realtime snapshot audio_output_ms when no TTS events exist', () => {
    // Realtime S2S sessions (Gemini Live / OpenAI Realtime) emit no
    // tts_detail/tts_timing events; per-turn audio duration lives in the
    // `kind: "realtime"` snapshot extra `audio_output_ms` (milliseconds).
    const realtimeUtt = (id: number, audioOutputMs: number): Utterance => ({
      utterance_id: id,
      summary: {},
      metric_snapshot: {
        utterance_id: id,
        total_ms: audioOutputMs + 500,
        interrupted: false,
        components: [
          {
            component_id: 'realtime',
            kind: 'realtime',
            started_at_ms: 0,
            extras: { audio_output_ms: audioOutputMs },
          },
        ],
      },
    } as unknown as Utterance);

    const m = computeSessionMetrics([realtimeUtt(1, 4000), realtimeUtt(2, 3000)], []);
    expect(m.totalAudioSeconds).toBe(7.0);
  });
});

// ── Output throughput ─────────────

describe('computeSessionMetrics — output throughput', () => {
  const outputUtt = (id: number, emit: number, interrupted = false): Utterance => ({
    utterance_id: id,
    summary: {},
    metric_snapshot: {
      utterance_id: id,
      total_ms: 5000,
      interrupted,
      components: [
        { component_id: 'realtime', kind: 'realtime', started_at_ms: 0, extras: { audio_output_ms: 3000 } },
        { component_id: 'output', kind: 'output', started_at_ms: 0, extras: { emit_rtf: emit, source_rtf: 0.5, chain_overhead_ms: 10, audio_output_ms: 3000 } },
      ],
    },
  } as unknown as Utterance);

  it('aggregates avg / max emit_rtf and counts turns slower than realtime (interrupted turns included)', () => {
    const m = computeSessionMetrics([outputUtt(1, 0.5), outputUtt(2, 1.67, true), outputUtt(3, 1.2), completedUtt(4)], []);
    expect(m.avgEmitRtf).toBe(1.12);
    expect(m.maxEmitRtf).toBe(1.67);
    expect(m.turnsOverRealtime).toBe(2);
  });

  it('is null / 0 for captures without an output component', () => {
    const m = computeSessionMetrics([completedUtt(1), completedUtt(2)], []);
    expect(m.avgEmitRtf).toBeNull();
    expect(m.maxEmitRtf).toBeNull();
    expect(m.turnsOverRealtime).toBe(0);
  });

  it('a turn exactly at 1.0 is not over realtime', () => {
    const m = computeSessionMetrics([outputUtt(1, 1.0)], []);
    expect(m.turnsOverRealtime).toBe(0);
    expect(m.maxEmitRtf).toBe(1);
  });
});

// ── S2S provider endpointing ─────────

describe('computeSessionMetrics — provider endpointing', () => {
  const rtUtt = (id: number, extras: Record<string, number | string>, summary: Utterance['summary'] = {}): Utterance => ({
    utterance_id: id,
    summary,
    metric_snapshot: {
      utterance_id: id,
      total_ms: 3000,
      interrupted: false,
      components: [{ component_id: 'realtime', kind: 'realtime', started_at_ms: 0, extras }],
    },
  });

  it('averages provider_endpointing_ms from the summary, falling back to the realtime snapshot extra', () => {
    const m = computeSessionMetrics([
      rtUtt(1, { ttfb_ms: 1623, provider_endpointing_ms: 666, ttfb_anchor_source: 'input_transcript_proxy' }),
      rtUtt(2, { ttfb_ms: 900 }, { provider_endpointing_ms: 334 }),
      rtUtt(3, { ttfb_ms: 800 }), // no value — not counted
      rtUtt(4, { ttfb_ms: 800, provider_endpointing_ms: 9999 }, { terminated: true }), // terminated — excluded
    ], []);
    expect(m.avgProviderEndpointingMs).toBe(500);
  });

  it('is null for cascade sessions and older S2S captures', () => {
    expect(computeSessionMetrics([completedUtt(1), completedUtt(2)], []).avgProviderEndpointingMs).toBeNull();
    expect(computeSessionMetrics([rtUtt(1, { ttfb_ms: 120, audio_output_ms: 1500 })], []).avgProviderEndpointingMs).toBeNull();
  });
});

// ── Turn count semantics ────────────────────────────────────────────────────

describe('computeSessionMetrics — turn counts', () => {
  it('counts only summary.terminated as terminated (not client_interrupted)', () => {
    const utterances: Utterance[] = [
      completedUtt(1),
      completedUtt(2, { client_interrupted: true }), // playback barge-in → still completed
      completedUtt(3, { terminated: true }),
    ];
    const m = computeSessionMetrics(utterances, []);
    expect(m.terminatedTurns).toBe(1);
    expect(m.completedTurns).toBe(2);
    expect(m.totalTurns).toBe(3);
  });

  it('separates server, client_cancel, and playback barge-ins', () => {
    const utterances: Utterance[] = [
      completedUtt(1, { interrupted: true, interrupt_trigger: 'server_vad' }),
      completedUtt(2, { interrupted: true, interrupt_trigger: 'client_cancel' }),
      completedUtt(3, { client_interrupted: true }),
      completedUtt(4),
    ];
    const m = computeSessionMetrics(utterances, []);
    expect(m.serverBargeIns).toBe(1);
    expect(m.clientBargeIns).toBe(1);
    expect(m.playbackBargeIns).toBe(1);
  });
});
