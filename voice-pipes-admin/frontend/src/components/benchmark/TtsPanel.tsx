/**
 * TTS metrics panel — surfaces synthesis-only fields collected by
 * `TtsOnlyCorpus` scenarios (and any pipeline run carrying TTS metrics).
 *
 * Rendered as a child of `VariantCard`. Returns `null` when every field
 * is nullish so non-TTS-focused experiments are unaffected.
 */

import { Volume2 } from 'lucide-react';
import type { VariantSummary } from './types';

interface Props {
  summary: VariantSummary;
  isDark: boolean;
}

function fmtMs(ms: number | null | undefined): string {
  if (ms == null) return '—';
  if (ms < 1000) return `${Math.round(ms)} ms`;
  return `${(ms / 1000).toFixed(1)} s`;
}

function fmtRtf(rtf: number | null | undefined): string {
  if (rtf == null) return '—';
  return rtf.toFixed(2);
}

function fmtSeconds(s: number | null | undefined): string {
  if (s == null) return '—';
  return `${s.toFixed(2)} s`;
}

function fmtCps(cps: number | null | undefined): string {
  if (cps == null) return '—';
  return `${cps.toFixed(1)} ch/s`;
}

function fmtPct(rate: number | null | undefined): string {
  if (rate == null) return '—';
  return `${(rate * 100).toFixed(1)}%`;
}

function hasAnyTtsMetric(s: VariantSummary): boolean {
  return (
    s.avg_tts_ttfb_ms != null ||
    s.p50_tts_ttfb_ms != null ||
    s.p90_tts_ttfb_ms != null ||
    s.p99_tts_ttfb_ms != null ||
    s.avg_tts_synthesis_ms != null ||
    s.avg_tts_audio_duration_s != null ||
    s.avg_tts_rtf != null ||
    s.avg_tts_chars_per_second != null ||
    s.tts_failure_rate != null ||
    s.cold_tts_synthesis_ms != null ||
    s.warm_tts_synthesis_ms != null
  );
}

export default function TtsPanel({ summary, isDark }: Props) {
  if (!hasAnyTtsMetric(summary)) return null;

  const failureBad = (summary.tts_failure_rate ?? 0) > 0;
  const failureClass = failureBad
    ? 'text-red-400'
    : isDark
      ? 'text-slate-200'
      : 'text-gray-800';

  return (
    <div
      data-testid="tts-panel"
      className={`mt-3 pt-3 border-t ${isDark ? 'border-slate-700' : 'border-gray-200'}`}
    >
      <div className={`flex items-center gap-2 mb-2 text-xs font-semibold uppercase tracking-wide ${
        isDark ? 'text-slate-400' : 'text-gray-500'
      }`}>
        <Volume2 size={12} />
        <span>TTS</span>
      </div>
      <div className="grid grid-cols-2 gap-y-2 gap-x-10">
        <Row label="P50 TTFB" value={fmtMs(summary.p50_tts_ttfb_ms)} isDark={isDark} />
        <Row label="P90 TTFB" value={fmtMs(summary.p90_tts_ttfb_ms)} isDark={isDark} />
        <Row label="P99 TTFB" value={fmtMs(summary.p99_tts_ttfb_ms)} isDark={isDark} />
        <Row label="Avg synth" value={fmtMs(summary.avg_tts_synthesis_ms)} isDark={isDark} />
        <Row label="RTF" value={fmtRtf(summary.avg_tts_rtf)} isDark={isDark} />
        <Row label="Audio dur" value={fmtSeconds(summary.avg_tts_audio_duration_s)} isDark={isDark} />
        <Row label="Chars/sec" value={fmtCps(summary.avg_tts_chars_per_second)} isDark={isDark} />
        <Row label="Cold synth" value={fmtMs(summary.cold_tts_synthesis_ms)} isDark={isDark} />
        <Row label="Warm synth" value={fmtMs(summary.warm_tts_synthesis_ms)} isDark={isDark} />
        <Row
          label="Failures"
          value={fmtPct(summary.tts_failure_rate)}
          isDark={isDark}
          valueClassName={failureClass}
        />
      </div>
    </div>
  );
}

function Row({
  label,
  value,
  isDark,
  valueClassName,
}: {
  label: string;
  value: string;
  isDark: boolean;
  valueClassName?: string;
}) {
  return (
    <div className={`flex items-center text-xs ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
      <span>{label}</span>
      <span
        className={`ml-auto font-mono font-medium ${
          valueClassName ?? (isDark ? 'text-slate-200' : 'text-gray-800')
        }`}
      >
        {value}
      </span>
    </div>
  );
}
