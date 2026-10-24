/**
 * Side-by-side TTS entry comparison.
 *
 * Pivots entries by `entry_id` across every variant that recorded
 * results for a `tts_only_corpus` scenario. Each row shows the input
 * text once and one audio player + headline metric per variant — so
 * a reviewer can A/B listen to the same utterance across providers.
 */

import { useState } from 'react';
import { Play, Snowflake, AlertTriangle } from 'lucide-react';
import { API_BASE } from '../../config/api';
import type { TtsEntryResult } from './types';

interface Props {
  experimentId: string;
  scenarioId: string;
  /** Already filtered to the active scenario. */
  variants: { variantId: string; variantName: string; entries: TtsEntryResult[] }[];
  isDark: boolean;
}
function fmtMs(ms: number | null | undefined): string {
  if (ms == null) return '—';
  if (ms < 1000) return `${Math.round(ms)} ms`;
  return `${(ms / 1000).toFixed(2)} s`;
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

export default function TtsCompareTable({
  experimentId,
  scenarioId,
  variants,
  isDark,
}: Props) {
  // Active audio key per cell (`${entryId}::${variantId}`).
  const [active, setActive] = useState<Set<string>>(new Set());

  if (variants.length < 2) return null;

  // Union of entry ids across all variants, in the order they appear in
  // the first non-empty variant.
  const seen = new Set<string>();
  const orderedIds: string[] = [];
  for (const v of variants) {
    for (const e of v.entries) {
      if (!seen.has(e.entry_id)) {
        seen.add(e.entry_id);
        orderedIds.push(e.entry_id);
      }
    }
  }

  if (!orderedIds.length) return null;

  // Map<entryId, Map<variantId, TtsEntryResult>>
  const matrix = new Map<string, Map<string, TtsEntryResult>>();
  for (const v of variants) {
    for (const e of v.entries) {
      if (!matrix.has(e.entry_id)) matrix.set(e.entry_id, new Map());
      matrix.get(e.entry_id)!.set(v.variantId, e);
    }
  }

  // Pull the canonical input text from whichever variant has it.
  const textFor = (entryId: string): string => {
    for (const v of variants) {
      const e = v.entries.find((x) => x.entry_id === entryId);
      if (e) return e.text;
    }
    return entryId;
  };

  const toggle = (key: string) => {
    setActive((prev) => {
      const next = new Set(prev);
      if (next.has(key)) {
        next.delete(key);
      } else {
        next.add(key);
      }
      return next;
    });
  };

  return (
    <div data-testid="tts-compare-table" className="overflow-x-auto">
      <table
        className={`w-full text-xs border-collapse ${
          isDark ? 'text-slate-300' : 'text-gray-700'
        }`}
      >
        <thead>
          <tr
            className={`text-left border-b ${
              isDark ? 'text-slate-400 border-slate-700' : 'text-gray-500 border-gray-200'
            }`}
          >
            <th className="py-2 px-2 font-medium">Entry</th>
            {variants.map((v) => (
              <th key={v.variantId} className="py-2 px-2 font-medium">
                {v.variantName}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {orderedIds.map((entryId) => {
            const text = textFor(entryId);
            return (
              <tr
                key={entryId}
                className={`align-top border-b ${
                  isDark ? 'border-slate-800' : 'border-gray-100'
                }`}
              >
                <td className="py-3 px-2">
                  <div className="font-mono text-[11px]">{entryId}</div>
                  <div className="max-w-xs whitespace-normal break-words mt-1">{text}</div>
                </td>
                {variants.map((v) => {
                  const e = matrix.get(entryId)?.get(v.variantId);
                  if (!e) {
                    return (
                      <td
                        key={v.variantId}
                        className={`py-3 px-2 ${isDark ? 'text-slate-600' : 'text-gray-300'}`}
                      >
                        —
                      </td>
                    );
                  }
                  const failed = e.error != null;
                  const cellKey = `${entryId}::${v.variantId}`;
                  return (
                    <td key={v.variantId} className="py-3 px-2">
                      {failed ? (
                        <div className="text-red-400 inline-flex items-center gap-1">
                          <AlertTriangle size={11} />
                          <span className="truncate max-w-[10rem]" title={e.error ?? ''}>
                            {e.error}
                          </span>
                        </div>
                      ) : (
                        <>
                          <div className="font-mono text-[11px]">
                            synth {fmtMs(e.synthesis_ms)} · TTFB {fmtMs(e.ttfb_ms)}
                            {e.cold_start && (
                              <span title="Cold start" className="ml-1">
                                <Snowflake size={10} className="inline text-sky-400" />
                              </span>
                            )}
                          </div>
                          {e.audio_available ? (
                            active.has(cellKey) ? (
                              <audio
                                controls
                                autoPlay
                                src={audioUrl(experimentId, v.variantId, scenarioId, entryId)}
                                className="h-7 max-w-[12rem] mt-1"
                                data-testid={`tts-compare-audio-${entryId}-${v.variantId}`}
                              />
                            ) : (
                              <button
                                type="button"
                                onClick={() => toggle(cellKey)}
                                className={`mt-1 inline-flex items-center gap-1 px-2 py-0.5 rounded text-[11px] ${
                                  isDark
                                    ? 'bg-indigo-500/15 text-indigo-300 hover:bg-indigo-500/25'
                                    : 'bg-indigo-50 text-indigo-600 hover:bg-indigo-100'
                                }`}
                                aria-label={`Play ${entryId} on ${v.variantName}`}
                              >
                                <Play size={10} /> Play
                              </button>
                            )
                          ) : null}
                        </>
                      )}
                    </td>
                  );
                })}
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}
