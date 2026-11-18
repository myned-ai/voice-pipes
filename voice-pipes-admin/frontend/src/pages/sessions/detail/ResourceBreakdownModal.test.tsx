import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent } from '@testing-library/react';
import { ResourceBreakdownModal } from './ResourceBreakdownModal';
import type { SessionData } from './types';

afterEach(cleanup);

describe('ResourceBreakdownModal', () => {
  const mockData: SessionData = {
    metadata: {
      peak_rss_bytes: 1991.5 * 1024 * 1024,
      container_peak_memory_bytes: 2210.6 * 1024 * 1024,
      avg_turn_cpu_ms: 185,
    },
    providers: {
      stt: { provider: 'parakeet-tdt', model: 'parakeet-tdt', device: { kind: 'cuda', source: 'probed' } },
      tts: { provider: 'openai', model: 'VibeVoice-Realtime-0.5B', device: { kind: 'cuda', source: 'probed' } },
      llm: { provider: 'openai-compat', model: 'gemma-4-E2B-it' },
    },
    utterances: [
      {
        utterance_id: 1,
        resource_usage: {
          rss_bytes: 1980 * 1024 * 1024,
          rss_delta_bytes: 0,
          cpu_percent: 12,
          cpu_time_ms: 150,
        },
      },
      {
        utterance_id: 2,
        resource_usage: {
          rss_bytes: 1980.5 * 1024 * 1024,
          rss_delta_bytes: 0.5 * 1024 * 1024,
          cpu_percent: 15,
          cpu_time_ms: 180,
        },
      },
    ],
  };

  it('renders null when isOpen is false', () => {
    const { container } = render(
      <ResourceBreakdownModal
        isOpen={false}
        onClose={vi.fn()}
        data={mockData}
        sessionId="dc720a1a-test"
      />
    );
    expect(container.firstChild).toBeNull();
  });

  it('renders modal header, measured KPIs, and recorded components without guessed MB', () => {
    render(
      <ResourceBreakdownModal
        isOpen={true}
        onClose={vi.fn()}
        data={mockData}
        sessionId="dc720a1a-test"
      />
    );

    expect(screen.getByText('System Resources & Turn Memory Profile')).toBeInTheDocument();
    expect(screen.getByText('1991.5 MB')).toBeInTheDocument();
    expect(screen.getByText('2211 MB')).toBeInTheDocument();
    expect(screen.getByText('185 ms')).toBeInTheDocument();
    expect(screen.getByText('+0.50 MB')).toBeInTheDocument();

    // Recorded Components
    expect(screen.getByText('Speech-to-Text (STT)')).toBeInTheDocument();
    expect(screen.getByText('Language Model (LLM)')).toBeInTheDocument();
    expect(screen.getByText('Text-to-Speech (TTS)')).toBeInTheDocument();
    expect(screen.getByText('VibeVoice-Realtime-0.5B')).toBeInTheDocument();

    // Must not render synthetic component heap guesses
    expect(screen.queryByText('Dynamic Inference Tensors')).toBeNull();
    expect(screen.queryByText('~620')).toBeNull();
    expect(screen.queryByText('~1100')).toBeNull();
  });

  it('reports missing metrics as — instead of guessing models, devices, or 0 drift', () => {
    const emptyData: SessionData = {
      metadata: {},
      utterances: [{ utterance_id: 1 }],
    };

    render(
      <ResourceBreakdownModal
        isOpen={true}
        onClose={vi.fn()}
        data={emptyData}
        sessionId="empty-session"
      />
    );

    expect(screen.queryByText('+0.00 MB')).toBeNull();
    expect(screen.queryByText('Parakeet TDT')).toBeNull();
    expect(screen.queryByText('VibeVoice')).toBeNull();
    expect(screen.queryByText('cuda')).toBeNull();
    expect(screen.getAllByText('Not recorded').length).toBeGreaterThanOrEqual(3);
  });

  it('renders turn stability table with turn rows and delta badges', () => {
    render(
      <ResourceBreakdownModal
        isOpen={true}
        onClose={vi.fn()}
        data={mockData}
        sessionId="dc720a1a-test"
      />
    );

    expect(screen.getByText('Turn-by-Turn Memory & CPU Profile')).toBeInTheDocument();
    expect(screen.getByText('Turn #1')).toBeInTheDocument();
    expect(screen.getByText('Turn #2')).toBeInTheDocument();
    expect(screen.getByText('flat')).toBeInTheDocument();
    expect(screen.getByText('+0.50M')).toBeInTheDocument();
  });

  it('calls onClose when close button is clicked', () => {
    const onClose = vi.fn();
    render(
      <ResourceBreakdownModal
        isOpen={true}
        onClose={onClose}
        data={mockData}
        sessionId="dc720a1a-test"
      />
    );

    const closeBtn = screen.getByTitle('Close (Esc)');
    fireEvent.click(closeBtn);
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('calls onClose when Escape key is pressed', () => {
    const onClose = vi.fn();
    render(
      <ResourceBreakdownModal
        isOpen={true}
        onClose={onClose}
        data={mockData}
        sessionId="dc720a1a-test"
      />
    );

    fireEvent.keyDown(window, { key: 'Escape' });
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('derives avg turn CPU from turn rows when metadata avg_turn_cpu_ms is null and marks dropped turns', () => {
    const partialData: SessionData = {
      ...mockData,
      metadata: {
        ...mockData.metadata,
        avg_turn_cpu_ms: undefined,
      },
      utterances: [
        ...(mockData.utterances ?? []),
        {
          utterance_id: 3,
        },
      ],
    };

    render(
      <ResourceBreakdownModal
        isOpen={true}
        onClose={vi.fn()}
        data={partialData}
        sessionId="dc720a1a-test"
      />
    );

    expect(screen.getByText('165 ms')).toBeInTheDocument();
    expect(screen.getByText('Dropped')).toBeInTheDocument();
  });

  it('treats ~1% ONNX arena warmup and negative reclamation as Memory Stable, and flags >2% growth as Drift Detected', () => {
    const mb = 1024 * 1024;
    const onnxWarmup: SessionData = {
      metadata: {},
      utterances: [
        { utterance_id: 1, resource_usage: { rss_bytes: 11260 * mb } },
        { utterance_id: 2, resource_usage: { rss_bytes: 11373 * mb } },
      ],
    };
    const { rerender } = render(
      <ResourceBreakdownModal isOpen={true} onClose={vi.fn()} data={onnxWarmup} sessionId="onnx-stable" />
    );
    expect(screen.getByText('Memory Stable')).toBeInTheDocument();
    expect(screen.getByText('Turn 1 → final (+1.0%)')).toBeInTheDocument();

    const reclaimed: SessionData = {
      metadata: {},
      utterances: [
        { utterance_id: 1, resource_usage: { rss_bytes: 100 * mb } },
        { utterance_id: 2, resource_usage: { rss_bytes: 50 * mb } },
      ],
    };
    rerender(
      <ResourceBreakdownModal isOpen={true} onClose={vi.fn()} data={reclaimed} sessionId="reclaimed" />
    );
    expect(screen.getByText('Memory Stable')).toBeInTheDocument();

    const realGrowth: SessionData = {
      metadata: {},
      utterances: [
        { utterance_id: 1, resource_usage: { rss_bytes: 11260 * mb } },
        { utterance_id: 2, resource_usage: { rss_bytes: 11600 * mb } },
      ],
    };
    rerender(
      <ResourceBreakdownModal isOpen={true} onClose={vi.fn()} data={realGrowth} sessionId="real-drift" />
    );
    expect(screen.getByText('Drift Detected')).toBeInTheDocument();
  });
});
