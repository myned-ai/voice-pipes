import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor } from '@testing-library/react';
import { MemoryRouter } from 'react-router-dom';
import axios from 'axios';
import Dashboard from '../Dashboard';
import type { AnalyticsData, Utterance } from './types';

vi.mock('axios');

function utterance(i: number, pipeline: string): Utterance {
  return {
    session_id: `sess-${pipeline}-${i % 2}`,
    utterance_id: i,
    timestamp: new Date(Date.UTC(2026, 9, 5, 10, i)).toISOString(),
    total_ms: 900 + i * 50,
    ttfb_ms: 400,
    ttft_ms: 300 + i,
    endpointing_ms: 200,
    stt_provider: i % 2 ? 'google' : 'assemblyai',
    tts_provider: i % 2 ? 'openai' : 'elevenlabs',
    pipeline_name: pipeline,
    tokens_per_sec: 40,
    first_token_ms: 300,
    prompt_tokens: 100,
    output_tokens: 20,
    generate_ms: 500,
    model: i % 2 ? 'gpt-a' : 'gpt-b',
    stt_audio_duration_s: 2,
    stt_session_duration_s: 3,
    stt_speech_model: 'chirp',
    tts_chars: 40,
    tts_audio_duration_s: 2,
    tts_voice: 'v1',
    client_ttfa_ms: 700,
    client_receive_to_play_ms: 50,
    emit_rtf: 0.5,
    audio_underruns: i === 0 ? 1 : 0,
    barge_in_rtt_ms: 120,
    had_error: i === 3,
    error_components: i === 3 ? ['tts'] : [],
    llm_cost_usd: 0.002,
  };
}

const analytics: AnalyticsData = {
  kpi: {
    total_sessions: 4,
    total_utterances: 8,
    avg_latency_ms: 1100,
    p95_latency_ms: 1300,
    avg_tokens_per_sec: 40,
    failed_sessions: 1,
    failed_turns: 1,
    error_rate_pct: 12.5,
    spend_total_usd: 1.5,
    spend_by_provider: {
      openai: { llm: 1, stt: 0, tts: 0.2, total: 1.2, monthly_cap_usd: 1, cap_pct: 120 },
      google: { llm: 0, stt: 0.3, tts: 0, total: 0.3, monthly_cap_usd: null, cap_pct: null },
    },
  },
  utterances: [0, 1, 2, 3].flatMap((i) => [utterance(i, 'alpha'), utterance(i, 'beta')]),
  provider_changes: [],
  failed_sessions: [
    {
      session_id: 'sess-failed',
      timestamp: '2026-10-05T10:00:00Z',
      pipeline_name: 'alpha',
      stt_provider: 'google',
      tts_provider: 'openai',
      errors_by_component: { tts: 1 },
      errors_total: 1,
      turn_count: 2,
      failed_turn_count: 1,
      last_error: 'tts timed out',
      reason: 'errors_recorded',
    },
  ],
  anomaly_sessions: [
    { session_id: 'sess-anom', pipeline_name: 'beta', timestamp: '2026-10-05T10:00:00Z', anomalies_total: 2, kind_counts: { slow: 2 } },
  ],
  available_pipelines: ['alpha', 'beta'],
};

class NoopResizeObserver {
  observe() {}
  unobserve() {}
  disconnect() {}
}

describe('Dashboard', () => {
  beforeEach(() => {
    vi.stubGlobal('ResizeObserver', NoopResizeObserver);
    // Every section visible except live health, which opens its own stream.
    localStorage.setItem('dashboard.hiddenSections', JSON.stringify(['health']));
    vi.mocked(axios.get).mockImplementation((url: string) => {
      if (url.endsWith('/api/v1/issues')) {
        return Promise.resolve({ data: { data: { issues: [] } } });
      }
      return Promise.resolve({ data: { data: analytics } });
    });
    vi.mocked(axios.post).mockResolvedValue({
      data: {
        data: {
          scope: 'all',
          cleared_at: 1700000000,
          rollups_removed: 4,
          issues_removed: 1,
          captures_deleted: 0,
        },
      },
    });
  });

  afterEach(() => {
    cleanup();
    localStorage.clear();
    vi.unstubAllGlobals();
    vi.mocked(axios.get).mockReset();
    vi.mocked(axios.post).mockReset();
  });

  it('renders simplified v1 view by default (no production bloat, lite=true)', async () => {
    render(
      <MemoryRouter>
        <Dashboard />
      </MemoryRouter>,
    );

    expect((await screen.findAllByText('Sessions')).length).toBeGreaterThan(0);
    expect(screen.getAllByText('Turns').length).toBeGreaterThan(0);
    expect(screen.getByText('Error Rate')).toBeTruthy();
    expect(screen.getByText('Avg Latency')).toBeTruthy();
    expect(screen.getByText('tts timed out')).toBeTruthy();
    expect(screen.getByText('Per-pipeline comparison')).toBeTruthy();

    // Production-only sections should NOT be rendered in v1 simplified mode
    expect(screen.queryByText('Provider over monthly cap')).toBeNull();
    expect(screen.queryByText('Audio Quality (client-perceived)')).toBeNull();
    expect(screen.queryByText('End-to-End Latency')).toBeNull();
    expect(screen.queryByText('LLM Performance')).toBeNull();
    expect(screen.queryByText('Token Usage')).toBeNull();

    // Should only make 1 analytics request (with lite: true) and no /api/v1/issues poll
    expect(axios.get).toHaveBeenCalledTimes(1);
    expect(axios.get).toHaveBeenCalledWith(
      expect.stringContaining('/api/v1/analytics'),
      expect.objectContaining({
        params: expect.objectContaining({ days: 1, lite: true }),
      }),
    );
  });

  it('renders every section when productionMode is true', async () => {
    render(
      <MemoryRouter>
        <Dashboard productionMode={true} />
      </MemoryRouter>,
    );

    expect((await screen.findAllByText('Sessions')).length).toBeGreaterThan(0);
    expect(screen.getByText('Provider over monthly cap')).toBeTruthy();
    expect(screen.getByText('Audio Quality (client-perceived)')).toBeTruthy();
    expect(screen.getByText('tts timed out')).toBeTruthy();
    expect(screen.getByText('End-to-End Latency')).toBeTruthy();
    expect(screen.getByText('LLM Performance')).toBeTruthy();
    expect(screen.getByText('Token Usage')).toBeTruthy();
    expect(screen.getAllByText('alpha').length).toBeGreaterThan(0);
  });

  it('opens Clear Metrics modal and submits with delete_captures checkbox', async () => {
    render(
      <MemoryRouter>
        <Dashboard />
      </MemoryRouter>,
    );

    await screen.findAllByText('Sessions');
    fireEvent.click(screen.getByRole('button', { name: /Clear Metrics/i }));

    expect(screen.getByRole('dialog')).toBeTruthy();
    const checkbox = screen.getByRole('checkbox');
    expect((checkbox as HTMLInputElement).checked).toBe(false);

    fireEvent.click(checkbox);
    expect((checkbox as HTMLInputElement).checked).toBe(true);

    fireEvent.click(screen.getByRole('button', { name: /Confirm Clear/i }));

    await waitFor(() => {
      expect(axios.post).toHaveBeenCalledWith(
        expect.stringContaining('/api/v1/analytics/clear'),
        expect.objectContaining({
          pipeline: null,
          delete_captures: true,
        }),
      );
    });
  });
});

