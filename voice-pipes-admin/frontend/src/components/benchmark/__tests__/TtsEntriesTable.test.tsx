/**
 * Component tests for `TtsEntriesTable` and `TtsCompareTable`.
 *
 * Pin the per-entry table contract: bucket/class chips render, cold
 * marker shown, audio Play button only shown when `audio_available`,
 * compare table only renders with ≥2 variants, and audio URL contains
 * the four ids the backend route expects.
 */

import { describe, expect, it } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';

import TtsEntriesTable from '../TtsEntriesTable';
import TtsCompareTable from '../TtsCompareTable';
import { buildCompareVariants } from '../ttsCompareHelpers';
import type { TtsEntryResult, VariantResults } from '../types';

function entry(over: Partial<TtsEntryResult> = {}): TtsEntryResult {
  return {
    entry_id: 'e1',
    text: 'Hello world',
    bucket: 'short',
    class: 'plain',
    ttfb_ms: 80,
    synthesis_ms: 320,
    audio_duration_ms: 1500,
    sample_rate: 22050,
    chars: 11,
    rtf: 0.21,
    cold_start: true,
    provider: 'piper',
    audio_path: '/tmp/e1.wav',
    audio_available: true,
    ...over,
  };
}

describe('TtsEntriesTable', () => {
  it('renders nothing for an empty entry list', () => {
    const { container } = render(
      <TtsEntriesTable
        experimentId="exp1"
        variantId="v1"
        scenarioId="s1"
        entries={[]}
        isDark={false}
      />,
    );
    expect(container.firstChild).toBeNull();
  });

  it('renders bucket/class chips and a Play button when audio is available', () => {
    render(
      <TtsEntriesTable
        experimentId="exp1"
        variantId="v1"
        scenarioId="s1"
        entries={[entry()]}
        isDark={false}
      />,
    );
    expect(screen.getByText('short')).toBeInTheDocument();
    expect(screen.getByText('plain')).toBeInTheDocument();
    expect(screen.getByLabelText(/play audio for e1/i)).toBeInTheDocument();
  });

  it('hides the Play button when audio is unavailable', () => {
    render(
      <TtsEntriesTable
        experimentId="exp1"
        variantId="v1"
        scenarioId="s1"
        entries={[entry({ audio_available: false })]}
        isDark={false}
      />,
    );
    expect(screen.queryByLabelText(/play audio for e1/i)).not.toBeInTheDocument();
  });

  it('shows error message instead of "ok" when synthesis failed', () => {
    render(
      <TtsEntriesTable
        experimentId="exp1"
        variantId="v1"
        scenarioId="s1"
        entries={[entry({ error: 'provider crashed', audio_available: false })]}
        isDark={false}
      />,
    );
    expect(screen.getByText(/provider crashed/)).toBeInTheDocument();
    expect(screen.queryByText('ok')).not.toBeInTheDocument();
  });

  it('flags an entry whose audio carried no speech, and shows the speech share otherwise', () => {
    render(
      <TtsEntriesTable
        experimentId="exp1"
        variantId="v1"
        scenarioId="s1"
        entries={[
          entry({ entry_id: 'e1', silent: true, speech_ratio: 0, peak_amplitude: 282 }),
          entry({ entry_id: 'e2', silent: false, speech_ratio: 0.41, peak_amplitude: 3300 }),
        ]}
        isDark={false}
      />,
    );
    expect(screen.getByText('silent')).toBeInTheDocument();
    expect(screen.getByText('no speech')).toBeInTheDocument();
    expect(screen.getByText('41.0%')).toBeInTheDocument();
    expect(screen.getAllByText('ok')).toHaveLength(1);
  });

  it('mounts an audio element with the correct src after Play click', () => {
    render(
      <TtsEntriesTable
        experimentId="exp1"
        variantId="v1"
        scenarioId="s1"
        entries={[entry()]}
        isDark={false}
      />,
    );
    fireEvent.click(screen.getByLabelText(/play audio for e1/i));
    const audio = screen.getByTestId('tts-audio-e1') as HTMLAudioElement;
    expect(audio.src).toContain(
      '/api/v1/benchmark/experiments/exp1/variants/v1/scenarios/s1/entries/e1/audio',
    );
  });
});

describe('TtsCompareTable', () => {
  it('renders nothing with fewer than 2 variants', () => {
    const { container } = render(
      <TtsCompareTable
        experimentId="exp1"
        scenarioId="s1"
        variants={[{ variantId: 'v1', variantName: 'piper', entries: [entry()] }]}
        isDark={false}
      />,
    );
    expect(container.firstChild).toBeNull();
  });

  it('renders one column per variant with the union of entry ids', () => {
    render(
      <TtsCompareTable
        experimentId="exp1"
        scenarioId="s1"
        variants={[
          { variantId: 'v1', variantName: 'piper', entries: [entry({ entry_id: 'e1' })] },
          {
            variantId: 'v2',
            variantName: 'pocket',
            entries: [entry({ entry_id: 'e1' }), entry({ entry_id: 'e2' })],
          },
        ]}
        isDark={false}
      />,
    );
    expect(screen.getByText('piper')).toBeInTheDocument();
    expect(screen.getByText('pocket')).toBeInTheDocument();
    // Both entries appear as rows.
    expect(screen.getAllByText(/^e1$/).length).toBeGreaterThan(0);
    expect(screen.getAllByText(/^e2$/).length).toBeGreaterThan(0);
  });
});

describe('buildCompareVariants', () => {
  it('drops variants with no entries for the given scenario', () => {
    const variants: VariantResults[] = [
      {
        variant_id: 'v1',
        variant_name: 'piper',
        scenarios: [
          {
            scenario_id: 's1',
            scenario_name: 'tts',
            session_ids: [],
            status: 'passed',
            metrics: {
              e2e_latency_ms: null,
              ttft_ms: null,
              ttfb_ms: null,
              stt_text: null,
              response_text: null,
              audio_duration_s: null,
            },
            latency_samples: [],
            tts_entries: [entry()],
          },
        ],
        summary: {
          scenarios_run: 1,
          scenarios_passed: 1,
          scenarios_failed: 0,
          avg_e2e_latency_ms: null,
          p50_e2e_latency_ms: null,
          p90_e2e_latency_ms: null,
          p99_e2e_latency_ms: null,
          avg_ttft_ms: null,
          avg_ttfb_ms: null,
        },
      },
      {
        variant_id: 'v2',
        variant_name: 'silent',
        scenarios: [
          {
            scenario_id: 's1',
            scenario_name: 'tts',
            session_ids: [],
            status: 'error',
            metrics: {
              e2e_latency_ms: null,
              ttft_ms: null,
              ttfb_ms: null,
              stt_text: null,
              response_text: null,
              audio_duration_s: null,
            },
            latency_samples: [],
            tts_entries: null,
          },
        ],
        summary: {
          scenarios_run: 1,
          scenarios_passed: 0,
          scenarios_failed: 1,
          avg_e2e_latency_ms: null,
          p50_e2e_latency_ms: null,
          p90_e2e_latency_ms: null,
          p99_e2e_latency_ms: null,
          avg_ttft_ms: null,
          avg_ttfb_ms: null,
        },
      },
    ];
    const out = buildCompareVariants(variants, 's1');
    expect(out).toHaveLength(1);
    expect(out[0].variantId).toBe('v1');
  });
});
