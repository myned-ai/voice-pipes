// Helpers for TTS fragment display in the session-detail turn cards.
// Kept out of AudioPlayers.tsx so that file exports only components
// (react-refresh/only-export-components).

import type { Utterance } from './types';

/** First few words of a fragment's text, for chip labels. */
export function fragmentSnippet(text: string, words = 3): string {
  const parts = text.trim().split(/\s+/).filter(Boolean);
  if (parts.length === 0) return '';
  const head = parts.slice(0, words).join(' ');
  return parts.length > words ? `${head}…` : head;
}

/** Per-fragment sentence text keyed by fragment kind (`tts_frag_000`),
 *  as the BFF attaches it to the utterance when a `.txt` sidecar exists.
 *  Realtime deltas have no sidecar, so the result is empty for them. */
export function fragmentTextsFor(utt: Utterance, entries: [string, string][]): Record<string, string> {
  const out: Record<string, string> = {};
  for (const [kind] of entries) {
    const text = utt[kind as `tts_frag_${string}`];
    if (typeof text === 'string' && text.trim()) out[kind] = text;
  }
  return out;
}
