/**
 * Per-entry table for `tts_only_corpus` scenarios.
 *
 * Each row shows the input text, bucket/class chips, latency + RTF
 * measurements, cold/warm flag, the round-trip transcript with WER,
 * and an inline audio player that streams the worker-host WAV via the
 * orchestrator's audio endpoint.
 */

import { useState } from 'react';
import { AlertTriangle, Play, Snowflake, Flame } from 'lucide-react';
import { API_BASE } from '../../config/api';
import type { TtsEntryResult } from './types';

interface Props {
  experimentId: string;
  variantId: string;
  scenarioId: string;
  entries: TtsEntryResult[];
  isDark: boolean;
}

function fmtMs(ms: number | null | undefined): string {
  if (ms == null) return '—';
  if (ms < 1000) return `${Math.round(ms)} ms`;
  return `${(ms / 1000).toFixed(2)} s`;
}

function fmtRtf(rtf: number | null | undefined): string {
  if (rtf == null) return '—';
  return rtf.toFixed(2);
}

function fmtPct(rate: number | null | undefined): string {
  if (rate == null) return '—';
  return `${(rate * 100).toFixed(1)}%`;
}

function audioUrl(
  experimentId: string,
  variantId: string,
  scenarioId: string,
  entryId: string,
): string {
  return `${API_BASE}/api/v1/benchmark/experiments/${encodeURIComponent(
    experimentId,
  )}/variants/${encodeURIComponent(variantId)}/scenarios/${encodeURIComponent(
    scenarioId,
  )}/entries/${encodeURIComponent(entryId)}/audio`;
}

export default function TtsEntriesTable({
  experimentId,
  variantId,
  scenarioId,
  entries,
  isDark,
}: Props) {
  const [activeAudio, setActiveAudio] = useState<string | null>(null);

  if (!entries.length) return null;

  const hasWer = entries.some((e) => e.wer != null);
  // Workers that predate the speech stats report neither field.
  const hasSpeech = entries.some((e) => e.speech_ratio != null || e.silent);

  return (
    <div data-testid="tts-entries-table" className="overflow-x-auto">
      <table
        className={`w-full text-xs border-collapse ${
          isDark ? 'text-slate-300' : 'text-gray-700'
        }`}
      >
        <thead>
          <tr
            className={`text-left ${
              isDark ? 'text-slate-400 border-slate-700' : 'text-gray-500 border-gray-200'
            } border-b`}
          >
            <Th>Entry</Th>
            <Th>Text</Th>
            <Th align="right">TTFB</Th>
            <Th align="right">Synth</Th>
            <Th align="right">RTF</Th>
            <Th align="center">Cold</Th>
            {hasSpeech && <Th align="right">Speech</Th>}
            {hasWer && <Th align="right">WER</Th>}
            <Th>Status</Th>
            <Th align="center">Audio</Th>
          </tr>
        </thead>
        <tbody>
          {entries.map((e) => {
            const failed = e.error != null;
            const rowKey = e.entry_id;
            return (
              <tr
                key={rowKey}
                className={`align-top border-b ${
                  isDark ? 'border-slate-800' : 'border-gray-100'
                }`}
              >
                <Td>
                  <div className="font-mono">{e.entry_id}</div>
                  <div className="flex gap-1 mt-1">
                    <Chip label={e.bucket} isDark={isDark} />
                    <Chip label={e.class} isDark={isDark} />
                  </div>
                </Td>
                <Td>
                  <div className="max-w-md whitespace-normal break-words">{e.text}</div>
                  {e.transcript && (
                    <details className="mt-1 text-[11px]">
                      <summary
                        className={`cursor-pointer ${
                          isDark ? 'text-slate-500' : 'text-gray-400'
                        }`}
                      >
                        transcript
                      </summary>
                      <div
                        className={`mt-1 italic ${
                          isDark ? 'text-slate-400' : 'text-gray-500'
                        }`}
                      >
                        {e.transcript}
                      </div>
                    </details>
                  )}
                </Td>
                <Td align="right" mono>
                  {fmtMs(e.ttfb_ms)}
                </Td>
                <Td align="right" mono>
                  {fmtMs(e.synthesis_ms)}
                </Td>
                <Td align="right" mono>
                  {fmtRtf(e.rtf)}
                </Td>
                <Td align="center">
                  {e.cold_start ? (
                    <span title="Cold start">
                      <Snowflake size={12} className="inline text-sky-400" />
                    </span>
                  ) : (
                    <span title="Warm">
                      <Flame size={12} className="inline text-amber-400 opacity-60" />
                    </span>
                  )}
                </Td>
                {hasSpeech && (
                  <Td align="right" mono>
                    {e.silent ? (
                      <span
                        className="text-red-400 font-semibold"
                        title={`No speech energy in the audio (peak ${e.peak_amplitude ?? '?'})`}
                      >
                        silent
                      </span>
                    ) : e.speech_ratio != null ? (
                      <span title="Share of 100 ms windows carrying speech energy">
                        {fmtPct(e.speech_ratio)}
                      </span>
                    ) : (
                      '—'
                    )}
                  </Td>
                )}
                {hasWer && (
                  <Td align="right" mono>
                    {e.wer != null ? (
                      <span
                        className={
                          e.wer > 0.25
                            ? 'text-red-400 font-semibold'
                            : isDark
                              ? 'text-slate-200'
                              : 'text-gray-800'
                        }
                      >
                        {fmtPct(e.wer)}
                      </span>
                    ) : (
                      '—'
                    )}
                  </Td>
                )}
                <Td>
                  {failed ? (
                    <span className="inline-flex items-center gap-1 text-red-400">
                      <AlertTriangle size={11} />
                      <span className="truncate max-w-[12rem]" title={e.error ?? ''}>
                        {e.error}
                      </span>
                    </span>
                  ) : e.silent ? (
                    <span className="inline-flex items-center gap-1 text-red-400">
                      <AlertTriangle size={11} />
                      no speech
                    </span>
                  ) : (
                    <span className={isDark ? 'text-emerald-400' : 'text-emerald-600'}>ok</span>
                  )}
                </Td>
                <Td align="center">
                  {e.audio_available ? (
                    activeAudio === rowKey ? (
                      <audio
                        controls
                        autoPlay
                        src={audioUrl(experimentId, variantId, scenarioId, e.entry_id)}
                        className="h-7 max-w-[12rem]"
                        data-testid={`tts-audio-${e.entry_id}`}
                      />
                    ) : (
                      <button
                        type="button"
                        onClick={() => setActiveAudio(rowKey)}
                        className={`inline-flex items-center gap-1 px-2 py-0.5 rounded text-[11px] ${
                          isDark
                            ? 'bg-indigo-500/15 text-indigo-300 hover:bg-indigo-500/25'
                            : 'bg-indigo-50 text-indigo-600 hover:bg-indigo-100'
                        }`}
                        aria-label={`Play audio for ${e.entry_id}`}
                      >
                        <Play size={10} /> Play
                      </button>
                    )
                  ) : (
                    <span className={isDark ? 'text-slate-600' : 'text-gray-300'}>—</span>
                  )}
                </Td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}

function Th({
  children,
  align = 'left',
}: {
  children: React.ReactNode;
  align?: 'left' | 'right' | 'center';
}) {
  const a = align === 'right' ? 'text-right' : align === 'center' ? 'text-center' : 'text-left';
  return <th className={`py-2 px-2 font-medium ${a}`}>{children}</th>;
}

function Td({
  children,
  align = 'left',
  mono = false,
}: {
  children: React.ReactNode;
  align?: 'left' | 'right' | 'center';
  mono?: boolean;
}) {
  const a = align === 'right' ? 'text-right' : align === 'center' ? 'text-center' : 'text-left';
  return <td className={`py-2 px-2 ${a} ${mono ? 'font-mono' : ''}`}>{children}</td>;
}

function Chip({ label, isDark }: { label: string; isDark: boolean }) {
  return (
    <span
      className={`inline-block px-1.5 py-0.5 rounded text-[10px] uppercase tracking-wide ${
        isDark ? 'bg-slate-700 text-slate-300' : 'bg-gray-100 text-gray-600'
      }`}
    >
      {label}
    </span>
  );
}
