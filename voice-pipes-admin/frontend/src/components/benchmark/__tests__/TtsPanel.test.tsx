/**
 * Component tests for `TtsPanel` and the TTS-aware bits of `VariantCard`.
 *
 * Covers the three fixture shapes: all-`None`, mixed (legacy E2E + new
 * TTS fields), and TTS-only payloads, plus the WER-quality regression badge.
 */

import { describe, expect, it } from 'vitest';
import { render, screen } from '@testing-library/react';

import TtsPanel from '../TtsPanel';
import VariantCard from '../VariantCard';
import type { VariantSummary } from '../types';

function emptySummary(): VariantSummary {
  return {
    scenarios_run: 0,
    scenarios_passed: 0,
    scenarios_failed: 0,
    avg_e2e_latency_ms: null,
    p50_e2e_latency_ms: null,
    p90_e2e_latency_ms: null,
    p99_e2e_latency_ms: null,
    avg_ttft_ms: null,
    avg_ttfb_ms: null,
  };
}

describe('TtsPanel', () => {
  it('renders nothing when no TTS fields are populated', () => {
    const { container } = render(<TtsPanel summary={emptySummary()} isDark={false} />);
    expect(container.firstChild).toBeNull();
  });

  it('renders the panel and core fields when TTS metrics are present', () => {
    const summary: VariantSummary = {
      ...emptySummary(),
      avg_tts_ttfb_ms: 80,
      p50_tts_ttfb_ms: 75,
      p90_tts_ttfb_ms: 120,
      p99_tts_ttfb_ms: 200,
      avg_tts_synthesis_ms: 350,
      avg_tts_audio_duration_s: 1.42,
      avg_tts_rtf: 0.25,
      avg_tts_chars_per_second: 18.4,
      tts_failure_rate: 0,
      cold_tts_synthesis_ms: 800,
      warm_tts_synthesis_ms: 200,
    };

    render(<TtsPanel summary={summary} isDark={true} />);

    expect(screen.getByTestId('tts-panel')).toBeTruthy();
    // Format checks: ms < 1000 → "N ms", seconds → "N.NN s".
    expect(screen.getByText('75 ms')).toBeTruthy();
    expect(screen.getByText('120 ms')).toBeTruthy();
    expect(screen.getByText('1.42 s')).toBeTruthy();
    expect(screen.getByText('0.25')).toBeTruthy();
    expect(screen.getByText('18.4 ch/s')).toBeTruthy();
    expect(screen.getByText('800 ms')).toBeTruthy();
    expect(screen.getByText('0.0%')).toBeTruthy();
  });

  it('marks a non-zero failure rate in red', () => {
    const summary: VariantSummary = {
      ...emptySummary(),
      avg_tts_synthesis_ms: 100,
      tts_failure_rate: 0.25,
    };

    render(<TtsPanel summary={summary} isDark={false} />);
    const cell = screen.getByText('25.0%');
    expect(cell.className).toMatch(/text-red-400/);
  });
});

describe('VariantCard', () => {
  const baseSummary: VariantSummary = {
    scenarios_run: 5,
    scenarios_passed: 5,
    scenarios_failed: 0,
    avg_e2e_latency_ms: 800,
    p50_e2e_latency_ms: 750,
    p90_e2e_latency_ms: 900,
    p99_e2e_latency_ms: 1100,
    avg_ttft_ms: 250,
    avg_ttfb_ms: 320,
  };

  it('hides the TTS panel for non-TTS experiments', () => {
    render(<VariantCard variantName="baseline" summary={baseSummary} isDark={false} />);
    expect(screen.queryByTestId('tts-panel')).toBeNull();
  });

  it('shows the TTS panel for a TTS-only payload', () => {
    const ttsOnly: VariantSummary = {
      ...baseSummary,
      avg_tts_synthesis_ms: 200,
      avg_tts_rtf: 0.18,
      cold_tts_synthesis_ms: 800,
      warm_tts_synthesis_ms: 150,
    };
    render(<VariantCard variantName="piper" summary={ttsOnly} isDark={true} />);
    expect(screen.getByTestId('tts-panel')).toBeTruthy();
  });

  it('flags WER above the 0.25 threshold as a regression', () => {
    const bad: VariantSummary = { ...baseSummary, avg_wer: 0.31 };
    render(<VariantCard variantName="piper" summary={bad} isDark={false} />);
    const badge = screen.getByTestId('wer-badge');
    expect(badge.textContent).toMatch(/regression/i);
    expect(badge.className).toMatch(/text-red-400/);
  });

  it('shows WER below the threshold without a regression flag', () => {
    const ok: VariantSummary = { ...baseSummary, avg_wer: 0.1 };
    render(<VariantCard variantName="piper" summary={ok} isDark={false} />);
    const badge = screen.getByTestId('wer-badge');
    expect(badge.textContent).not.toMatch(/regression/i);
    expect(badge.className).not.toMatch(/text-red-400/);
  });
});
