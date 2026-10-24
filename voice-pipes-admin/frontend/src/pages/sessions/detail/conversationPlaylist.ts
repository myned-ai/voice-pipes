// Playlist for the session-level Conversation Playback: what to play, in
// what order, and how long to wait before each entry.
//
// Why the waits are explicit: an S2S session keeps the mic open, so a turn's
// `mic_raw` spans the whole inter-turn window (the agent's reply included)
// and is mostly silence — 19.5 s of clip for 0.5 s of speech on a measured
// session, 59 s of nothing for a typed turn. Playing those raw made the
// "pauses" between turns many times longer than the real call. So the user
// side plays the trimmed `user_speech.wav`, turns with no speech are skipped,
// and the pause before an agent reply is the latency the user actually
// experienced (`e2e_perceived_ms`), labelled as such.

import type { Utterance } from './types';

export interface PlaylistEntry {
  url: string;
  turnId: number;
  side: 'user' | 'ai';
  label: string;
  /** Silence to hold before this entry starts, ms. */
  leadInMs: number;
  /** What the lead-in represents, for the status line. */
  leadInReason: 'response latency' | 'turn gap' | null;
}

/** A measured response latency longer than this is held at the cap — the
 *  point is to feel the real gap, not to sit through a 20 s stall. */
export const LEAD_IN_CAP_MS = 8000;
/** Gap before a user turn (the real one is unknown: the capture does not
 *  say when the user started relative to the previous reply's end). */
export const TURN_GAP_MS = 350;

export function buildPlaylist(utterances: Utterance[], sessionId: string, apiBase: string): PlaylistEntry[] {
  const audio = (file: string) => `${apiBase}/api/v1/sessions/${encodeURIComponent(sessionId)}/audio/${file}`;
  const list: PlaylistEntry[] = [];

  for (const utt of utterances) {
    const af = utt.audio_files || {};
    const id = utt.utterance_id;

    // ── User side ──
    // `user_speech === null` is a positive finding — no speech — so skip.
    // Absent means an older BFF: fall back to the raw clip as before.
    let userAdded = false;
    if (utt.user_speech !== null) {
      const hasMic = !!(af.mic_raw || af.mic_16k);
      if (utt.user_speech && hasMic) {
        list.push({
          url: `${apiBase}/api/v1/sessions/${encodeURIComponent(sessionId)}/audio/turn/${id}/user_speech.wav`,
          turnId: id,
          side: 'user',
          label: `Turn ${id} — User`,
          leadInMs: list.length === 0 ? 0 : TURN_GAP_MS,
          leadInReason: list.length === 0 ? null : 'turn gap',
        });
        userAdded = true;
      } else if (utt.user_speech === undefined && hasMic) {
        list.push({
          url: audio(af.mic_raw ?? af.mic_16k!),
          turnId: id,
          side: 'user',
          label: `Turn ${id} — User`,
          leadInMs: list.length === 0 ? 0 : TURN_GAP_MS,
          leadInReason: list.length === 0 ? null : 'turn gap',
        });
        userAdded = true;
      }
    }

    // ── Agent side ── one entry per turn, the server-stitched concat: the
    // backend short-circuits to the single file when there is one fragment,
    // and chaining many tiny fragments through `<audio>.src` swaps clicks.
    const hasTts = Object.keys(af).some(k => k.startsWith('tts_frag'));
    if (hasTts) {
      const e2e = utt.summary?.e2e_perceived_ms;
      const latency = userAdded && e2e != null && e2e > 0 ? Math.min(e2e, LEAD_IN_CAP_MS) : null;
      list.push({
        url: `${apiBase}/api/v1/sessions/${encodeURIComponent(sessionId)}/audio/turn/${id}/tts_concat.wav`,
        turnId: id,
        side: 'ai',
        label: `Turn ${id} — AI`,
        leadInMs: latency ?? (list.length === 0 ? 0 : TURN_GAP_MS),
        leadInReason: latency !== null ? 'response latency' : list.length === 0 ? null : 'turn gap',
      });
    }
  }
  return list;
}
