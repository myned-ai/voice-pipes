import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent } from '@testing-library/react';
import { SessionSummaryDialog } from './SessionSummaryDialog';
import type { SessionData, ReportAnomaly } from './types';

afterEach(cleanup);

const mockSessionData: SessionData = {
  metadata: {
    pipeline_name: 'minimal',
    started_at: '2026-10-18T19:38:09.581Z',
    ended_at: '2026-10-18T19:38:35.844Z',
    turn_count: 2,
    status: 'completed',
  },
  providers: {
    stt: {
      provider: 'parakeet-tdt',
      model: 'parakeet-tdt',
      mode: 'batch',
      device: { kind: 'cpu', source: 'config' },
    },
    llm: {
      provider: 'SmolLM2-360M-Instruct-Q4_K_M',
      model: 'models/smollm2-360m/SmolLM2-360M-Instruct-Q4_K_M.gguf',
      device: { kind: 'cpu', source: 'config' },
    },
    tts: {
      provider: 'piper',
      model: 'en_US-lessac-medium',
      device: { kind: 'cpu', source: 'config' },
    },
  },
  verdict: {
    level: 'healthy',
    primary_culprit: 'none',
    headline: 'All turns operated within target thresholds.',
    supporting_anomaly_kinds: [],
    unstable_components: [],
    critical_path_p50_ms: 3416,
    critical_path_p90_ms: 4459,
  },
  session_metrics: {
    completedTurns: 2,
    terminatedTurns: 0,
    totalTurns: 2,
    avgTtftMs: 2104,
    avgTtfbMs: 178,
    avgEndpointingMs: 1119,
    avgTokensPerSec: 16.1,
    avgBargeInRttMs: null,
    serverBargeIns: 0,
    clientBargeIns: 0,
    playbackBargeIns: 0,
    totalPromptTokens: 94,
    totalOutputTokens: 26,
    totalAudioSeconds: 5.71,
    avgRtf: 0.09,
    turnsWithUnderruns: 0,
    totalUnderruns: 0,
    eventsAggregated: 57,
  },
};

describe('SessionSummaryDialog', () => {
  it('does not render when isOpen is false', () => {
    const onClose = vi.fn();
    render(
      <SessionSummaryDialog
        isOpen={false}
        onClose={onClose}
        data={mockSessionData}
        sessionId="test-session-123"
      />
    );
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  it('renders verdict, KPIs, and provider summary when isOpen is true', () => {
    const onClose = vi.fn();
    render(
      <SessionSummaryDialog
        isOpen={true}
        onClose={onClose}
        data={mockSessionData}
        sessionId="test-session-123"
      />
    );

    expect(screen.getByRole('dialog')).toBeTruthy();
    expect(screen.getByText('Session Summary & Key Facts')).toBeTruthy();
    expect(screen.getByText('minimal')).toBeTruthy();
    expect(screen.getByText('All turns operated within target thresholds.')).toBeTruthy();

    // Check KPIs
    expect(screen.getAllByText(/4\.46/).length).toBeGreaterThanOrEqual(1); // Critical-Path p90
    expect(screen.getByText('2104 ms')).toBeTruthy(); // TTFT
    expect(screen.getByText('178 ms')).toBeTruthy(); // TTFB
    expect(screen.getByText('1119 ms')).toBeTruthy(); // Endpointing
    expect(screen.getByText('16.1 tok/s')).toBeTruthy(); // Tokens/sec

    // Check providers
    expect(screen.getByText('STT: parakeet-tdt')).toBeTruthy();
    expect(screen.getByText('LLM: SmolLM2-360M-Instruct-Q4_K_M')).toBeTruthy();
    expect(screen.getByText('TTS: piper')).toBeTruthy();
  });

  it('calls onClose when close button is clicked', () => {
    const onClose = vi.fn();
    render(
      <SessionSummaryDialog
        isOpen={true}
        onClose={onClose}
        data={mockSessionData}
        sessionId="test-session-123"
      />
    );

    const closeBtn = screen.getByTitle('Close summary');
    fireEvent.click(closeBtn);
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('calls onClose when Escape key is pressed', () => {
    const onClose = vi.fn();
    render(
      <SessionSummaryDialog
        isOpen={true}
        onClose={onClose}
        data={mockSessionData}
        sessionId="test-session-123"
      />
    );

    fireEvent.keyDown(window, { key: 'Escape' });
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('renders anomaly flags when present', () => {
    const onClose = vi.fn();
    const anomalies: Record<string, ReportAnomaly[]> = {
      '2': [
        {
          utt: 2,
          kind: 'llm.ttft_high',
          level: 'warn',
          observed: 2764,
          target: 300,
          severity: 'severe',
          on_critical_path: true,
          detail: 'Time to first token exceeded target',
        },
      ],
    };

    render(
      <SessionSummaryDialog
        isOpen={true}
        onClose={onClose}
        data={mockSessionData}
        sessionId="test-session-123"
        anomaliesByUtt={anomalies}
      />
    );

    expect(screen.getByText('Operational Findings (1 flag)')).toBeTruthy();
    expect(screen.getByText('llm.ttft_high')).toBeTruthy();
    expect(screen.getByText('Time to first token exceeded target')).toBeTruthy();
  });
});
