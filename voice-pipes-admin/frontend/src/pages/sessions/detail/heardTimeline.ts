// Geometry for the fragment timeline under a turn's audio player.
//
// Two coordinate systems are in play:
//
//   • *ideal* ms — the server track (`tts_concat.wav`): fragments laid end to
//     end, no gaps. `FragmentInfo.offset_ms` lives here, and so does every
//     client-reported underrun (`at_audio_ms`).
//   • *heard* ms — the `as-heard.wav` reconstruction: the same audio with the
//     reported silences spliced in at their `at_audio_ms`, and (for an
//     interrupted turn) nothing past the barge-in point.
//
// The as-heard track is built by `backend/app/routers/audio.py`
// (`get_turn_as_heard`); `idealToHeardMs` and `layoutTimeline` mirror that
// splice exactly so a click on the bar seeks to the sample it depicts. If the
// backend's splice changes, this file must change with it — the backend test
// pins the audio, `heardTimeline.test.ts` pins the geometry.

import type { FragmentInfo } from './types';

export interface PlaybackGap {
  at_audio_ms: number;
  silence_ms: number;
}

/** What the client actually received, as far as the capture knows. */
export interface HeardModel {
  gaps: PlaybackGap[];
  /** Ideal-audio ms delivered before a barge-in cut; `null` when the turn
   *  played to completion. */
  cutMs: number | null;
}

export type TimelineMode = 'server' | 'heard';

export interface TimelineSegment {
  /** Index into the fragment list. */
  idx: number;
  kind: string;
  /** Track coordinates for the current mode. */
  startMs: number;
  endMs: number;
  /** Server mode only: this audio was synthesized but never delivered. */
  afterCut: boolean;
}

export interface TimelineGapBlock {
  /** Track coordinates. Zero-width in server mode (a marker), `silence_ms`
   *  wide in heard mode (the silence itself). */
  startMs: number;
  endMs: number;
  silenceMs: number;
  /** Ideal position, for tooltips. */
  atAudioMs: number;
}

export interface TimelineLayout {
  mode: TimelineMode;
  /** Length of the track being played, in its own coordinates. */
  totalMs: number;
  segments: TimelineSegment[];
  gaps: TimelineGapBlock[];
  /** Track position of the barge-in cut, or `null`. */
  cutMs: number | null;
}

const sortedGaps = (gaps: PlaybackGap[]): PlaybackGap[] =>
  [...gaps].sort((a, b) => a.at_audio_ms - b.at_audio_ms);

/** Map an ideal-audio position to its position on the as-heard track.
 *
 *  A gap reported at `at_audio_ms` is inserted *before* the audio at that
 *  position, so every ideal sample at or after it shifts right by the gap's
 *  silence. Gaps are cumulative. Mirrors the backend splice, which walks the
 *  sorted gap list and emits `ideal[cursor:at] + silence`. */
export function idealToHeardMs(idealMs: number, gaps: PlaybackGap[]): number {
  let shift = 0;
  for (const g of sortedGaps(gaps)) {
    if (g.at_audio_ms <= idealMs) shift += g.silence_ms;
  }
  return idealMs + shift;
}

/** Total ideal length: end of the last fragment. */
export function idealTotalMs(fragments: FragmentInfo[]): number {
  if (fragments.length === 0) return 0;
  const last = fragments[fragments.length - 1];
  return last.offset_ms + last.duration_ms;
}

/** Lay the fragments and gaps out on the chosen track.
 *
 *  Heard mode reproduces the backend: ideal audio clamped to `cutMs` first,
 *  then every gap spliced in — a gap reported past the cut is placed at the
 *  end, so its silence still counts. Fragments that start past the cut are
 *  dropped; one straddling it is truncated. */
export function layoutTimeline(
  fragments: FragmentInfo[],
  mode: TimelineMode,
  heard: HeardModel,
): TimelineLayout {
  const total = idealTotalMs(fragments);
  const gaps = sortedGaps(heard.gaps);

  if (mode === 'server') {
    const cut = heard.cutMs !== null ? Math.min(heard.cutMs, total) : null;
    return {
      mode,
      totalMs: total,
      segments: fragments.map((f, idx) => ({
        idx,
        kind: f.kind,
        startMs: f.offset_ms,
        endMs: f.offset_ms + f.duration_ms,
        afterCut: cut !== null && f.offset_ms >= cut,
      })),
      gaps: gaps.map(g => {
        const at = Math.min(g.at_audio_ms, total);
        return { startMs: at, endMs: at, silenceMs: g.silence_ms, atAudioMs: g.at_audio_ms };
      }),
      cutMs: cut,
    };
  }

  // Heard mode. The playable ideal audio is [0, clampedTotal); gap positions
  // are clamped into that range exactly as the backend clamps `at` to
  // `total_frames`.
  const clampedTotal = heard.cutMs !== null ? Math.min(heard.cutMs, total) : total;
  const clampedGaps = gaps.map(g => ({ ...g, at_audio_ms: Math.min(g.at_audio_ms, clampedTotal) }));
  const toHeard = (ms: number) => idealToHeardMs(ms, clampedGaps);

  const segments: TimelineSegment[] = [];
  fragments.forEach((f, idx) => {
    if (f.offset_ms >= clampedTotal) return;
    const idealEnd = Math.min(f.offset_ms + f.duration_ms, clampedTotal);
    segments.push({
      idx,
      kind: f.kind,
      startMs: toHeard(f.offset_ms),
      // A gap sitting exactly at this fragment's end belongs to the *next*
      // fragment's start, so measure the end from just inside the fragment.
      endMs: toHeard(f.offset_ms) + (idealEnd - f.offset_ms) + gapsStrictlyInside(clampedGaps, f.offset_ms, idealEnd),
      afterCut: false,
    });
  });

  const gapBlocks: TimelineGapBlock[] = [];
  let shift = 0;
  for (const g of clampedGaps) {
    const start = g.at_audio_ms + shift;
    gapBlocks.push({ startMs: start, endMs: start + g.silence_ms, silenceMs: g.silence_ms, atAudioMs: g.at_audio_ms });
    shift += g.silence_ms;
  }

  return {
    mode,
    totalMs: clampedTotal + shift,
    segments,
    gaps: gapBlocks,
    cutMs: heard.cutMs !== null ? clampedTotal + shift : null,
  };
}

/** Silence inserted strictly inside (start, end) of a fragment — gaps that
 *  split it. A gap on the leading edge is already in `toHeard(start)`; one
 *  on the trailing edge is emitted after the fragment's last sample, so it
 *  is the next segment's problem (or trailing silence). */
function gapsStrictlyInside(gaps: PlaybackGap[], startMs: number, endMs: number): number {
  let sum = 0;
  for (const g of gaps) {
    if (g.at_audio_ms > startMs && g.at_audio_ms < endMs) sum += g.silence_ms;
  }
  return sum;
}

/** Which fragment is under a track position (binary search on segment
 *  starts). Returns `null` inside a gap block or past the end. */
export function segmentAt(layout: TimelineLayout, trackMs: number): TimelineSegment | null {
  const segs = layout.segments;
  let lo = 0;
  let hi = segs.length - 1;
  let found: TimelineSegment | null = null;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    if (segs[mid].startMs <= trackMs) {
      found = segs[mid];
      lo = mid + 1;
    } else {
      hi = mid - 1;
    }
  }
  if (!found || trackMs >= found.endMs) return null;
  return found;
}

/** The gap block under a track position, if any. */
export function gapAt(layout: TimelineLayout, trackMs: number): TimelineGapBlock | null {
  return layout.gaps.find(g => g.endMs > g.startMs && trackMs >= g.startMs && trackMs < g.endMs) ?? null;
}

export function formatMs(ms: number): string {
  return ms >= 1000 ? `${(ms / 1000).toFixed(2)} s` : `${Math.round(ms)} ms`;
}
