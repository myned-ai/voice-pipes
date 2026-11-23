import { describe, it, expect, vi, afterEach, beforeAll } from 'vitest';
import { render, screen, fireEvent, cleanup, waitFor } from '@testing-library/react';
import { MemoryRouter, Route, Routes } from 'react-router-dom';
import axios from 'axios';
import SessionDetail from '../SessionDetail';
import { ThemeProvider } from '../../../contexts/ThemeProvider';
import { setMockViewport } from '../../../test/viewport';

vi.mock('axios', () => ({
  default: {
    get: vi.fn(),
  },
}));

const mockedGet = vi.mocked(axios.get);

beforeAll(() => {
  Object.defineProperty(HTMLMediaElement.prototype, 'play', {
    configurable: true,
    value: () => Promise.resolve(),
  });
  Object.defineProperty(HTMLMediaElement.prototype, 'load', {
    configurable: true,
    value: () => {},
  });
});

const SAMPLE_SESSION = {
  metadata: {
    pipeline_name: 'support_agent',
    status: 'completed',
    turn_count: 1,
    started_at: '2026-11-22T20:00:00Z',
    ended_at: '2026-11-22T20:00:30Z',
    peak_rss_bytes: 512 * 1024 * 1024,
    avg_turn_cpu_ms: 140,
  },
  verdict: {
    level: 'healthy',
    headline: 'All turns within budget',
    primary_culprit: 'none',
  },
  providers: {
    llm: { provider: 'openai', model: 'gpt-4o-mini' },
    stt: { provider: 'parakeet' },
    tts: { provider: 'kokoro' },
  },
  utterances: [
    {
      utterance_id: 1,
      stt: { text: 'Hello there', latency_ms: 120 },
      llm_output: { text: 'Hi! How can I help you today?' },
      llm_timing: { total_ms: 400, generate_ms: 250, tokens_per_sec: 42.5 },
      tts_timing: { elapsed_ms: 200 },
      summary: {
        ttft_ms: 300,
        ttfb_ms: 500,
        total_ms: 1500,
        e2e_perceived_ms: 550,
      },
    },
  ],
  events: [],
  events_total: 0,
  telemetry: [],
};

function setupMocks() {
  mockedGet.mockImplementation((url: string) => {
    if (url.includes('/report-for-llm')) {
      return Promise.resolve({ data: { data: { anomalies: [] } } });
    }
    return Promise.resolve({ data: { data: SAMPLE_SESSION } });
  });
}

function renderDetail() {
  return render(
    <ThemeProvider>
      <MemoryRouter initialEntries={['/sessions/test-1']}>
        <Routes>
          <Route path="/sessions/:id" element={<SessionDetail />} />
        </Routes>
      </MemoryRouter>
    </ThemeProvider>,
  );
}

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe('SessionDetail responsive layout', () => {
  it('renders inline TelemetryPanel, full header chips, and expanded TurnCard details at 1440px', async () => {
    setMockViewport(1440);
    setupMocks();
    renderDetail();

    await waitFor(() => {
      expect(screen.getByText('Hello there')).toBeInTheDocument();
    });

    // TelemetryPanel is open inline without a modal close button
    expect(screen.getByRole('heading', { name: /Telemetry/i })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Close telemetry panel' })).toBeNull();

    // Header chips and TurnCard timing stats are visible inline
    expect(screen.getByText(/Peak RAM:/)).toBeInTheDocument();
    expect(screen.getByText(/Copy report for LLM/)).toBeInTheDocument();
    expect(screen.getByText('42.5 tok/s')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /Timing & stats/i })).toBeNull();
  });

  it('closes TelemetryPanel by default, opens it as an overlay, and collapses TurnCard timing at 360px', async () => {
    setMockViewport(360);
    setupMocks();
    renderDetail();

    await waitFor(() => {
      expect(screen.getByText('Hello there')).toBeInTheDocument();
    });

    // Telemetry closed by default on < 1280px
    expect(screen.queryByRole('heading', { name: /Telemetry/i })).toBeNull();

    // TurnCard stats are collapsed behind "Timing & stats"
    expect(screen.queryByText('42.5 tok/s')).toBeNull();
    const timingToggle = screen.getByRole('button', { name: /Timing & stats/i });
    fireEvent.click(timingToggle);
    expect(screen.getByText('42.5 tok/s')).toBeInTheDocument();

    // Open TelemetryPanel -> renders in overlay dialog with close button
    fireEvent.click(screen.getByTitle('Show Telemetry Panel'));
    const telemetryDialog = screen.getByRole('dialog', { name: 'Telemetry panel' });
    expect(telemetryDialog).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Close telemetry panel' }));
    expect(screen.queryByRole('dialog', { name: 'Telemetry panel' })).toBeNull();

    // Quick Summary dialog exposes Resource Breakdown & Copy Full Diagnostic
    fireEvent.click(screen.getByText('Quick Summary'));
    expect(screen.getByRole('dialog', { name: 'Session summary' })).toBeInTheDocument();
    expect(screen.getByText('Resource Breakdown')).toBeInTheDocument();
    expect(screen.getByText('Copy Full Diagnostic')).toBeInTheDocument();
  });
});
