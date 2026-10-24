// WordTimingTable — pure data structure mapping playback offsets to word positions.
//
// This is the single authoritative source for "how many words have been heard
// at playback offset T?".  It supports three timing sources, in priority order:
//
//   1. Server-supplied `word_timings[]` (forced alignment from the TTS engine).
//   2. Syllable-weighted estimate over the fragment's audio duration.
//   3. Character-weighted fallback with a 250 ms/word floor (prevents flash-reveal
//      when the caption text arrives faster than its audio).
//
// The returned `WordTiming[]` shape is identical regardless of source, so the
// reveal hook is unaware of which producer was used.
//
// Pure module — no React, no DOM, no AudioContext.  Deterministic and trivially
// testable.

import type { CaptionMeta, WordTiming } from "../transport/types";

/** Minimum milliseconds-per-word floor used by the character-weighted fallback. */
export const MIN_MS_PER_WORD = 250;

/** Buffer window (ms) to tolerate trailing silence/VAD delays when checking if the final word was heard. */
export const SILENCE_LENIENCY_THRESHOLD_MS = 200;

/** Tokenize sentence text into non-empty words.  Whitespace and punctuation
 *  are preserved as part of the surrounding word for downstream rendering;
 *  this function returns only the word boundary count and the bare word
 *  strings used for timing math. */
export function splitWords(text: string): string[] {
  return text.split(/\s+/).filter(Boolean);
}

/** English-tuned syllable estimator.  Vowel-group count, with a floor of 1.
 *  Language-specific estimators can be plugged in later via a strategy
 *  parameter; for now this is good enough for English TTS pacing. */
export function estimateSyllables(word: string): number {
  const groups = word.toLowerCase().match(/[aeiouy]+/g);
  return Math.max(1, groups ? groups.length : 1);
}

/** Build word timings using the syllable-weighted strategy.
 *  Each word's duration is proportional to its syllable count.
 *  Offsets are relative to the fragment start (offset = 0 → first word). */
export function timingsFromSyllables(
  text: string,
  fragmentDurationMs: number,
): WordTiming[] {
  const words = splitWords(text);
  if (words.length === 0 || fragmentDurationMs <= 0) return [];

  const syllableCounts = words.map(estimateSyllables);
  const total = syllableCounts.reduce((a, b) => a + b, 0);
  if (total === 0) return [];

  let offsetMs = 0;
  return words.map((word, i) => {
    const fraction = syllableCounts[i] / total;
    const duration = fragmentDurationMs * fraction;
    const startMs = offsetMs;
    offsetMs += duration;
    return { word, start_ms: startMs, end_ms: offsetMs };
  });
}

/** Build word timings using the character-weighted fallback.
 *  Enforces a `MIN_MS_PER_WORD` floor so that fast-arriving text cannot
 *  produce timings that would flash all words to the screen at once. */
export function timingsFromChars(
  text: string,
  fragmentDurationMs: number,
): WordTiming[] {
  const words = splitWords(text);
  if (words.length === 0) return [];

  const effectiveDurationMs = Math.max(
    fragmentDurationMs,
    words.length * MIN_MS_PER_WORD,
  );
  const totalChars = words.reduce((sum, w) => sum + w.length, 0);
  if (totalChars === 0) return [];

  let offsetMs = 0;
  return words.map((word) => {
    const fraction = word.length / totalChars;
    const duration = effectiveDurationMs * fraction;
    const startMs = offsetMs;
    offsetMs += duration;
    return { word, start_ms: startMs, end_ms: offsetMs };
  });
}

/** Adopt server-supplied timings unchanged.  Returned as a defensive copy so
 *  later mutations to the input array cannot retroactively corrupt the table. */
export function timingsFromServer(timings: WordTiming[]): WordTiming[] {
  return timings.map((t) => ({
    word: t.word,
    start_ms: t.start_ms,
    end_ms: t.end_ms,
  }));
}

/** Build a `WordTiming[]` for a single fragment, applying the source-priority
 *  rule.  Offsets are local to the fragment (start at 0); use
 *  {@link WordTimingTable.append} to splice into a per-utterance table. */
export function timingsForFragment(
  caption: CaptionMeta,
  fragmentDurationMs: number,
): WordTiming[] {
  if (caption.word_timings && caption.word_timings.length > 0) {
    return timingsFromServer(caption.word_timings);
  }
  const duration = caption.estimated_duration_ms ?? fragmentDurationMs;
  const syllableTimings = timingsFromSyllables(caption.text, duration);
  if (syllableTimings.length > 0) return syllableTimings;
  return timingsFromChars(caption.text, duration);
}

/** A `WordTimingTable` accumulates per-utterance word timings as fragments
 *  arrive.  The table is monotonic: appended timings are shifted to start
 *  immediately after the previous fragment's end so that the entire utterance
 *  shares a single contiguous timeline.
 *
 *  All offsets are absolute from the start of the utterance's audio (i.e. the
 *  first sample of the first audio chunk for the utterance). */
export class WordTimingTable {
  /** Flat list of word timings across all appended fragments, with
   *  monotonically increasing offsets. */
  readonly timings: WordTiming[] = [];

  /** Total accumulated duration (ms) of all appended fragments. */
  totalDurationMs = 0;

  /** Absolute playback offset where the most-recently appended fragment
   *  began, or `null` if no fragment has been appended yet.  Used by
   *  `appendCaption` to retroactively rescale the previous fragment when
   *  a new fragment arrives at a known later anchor. */
  private lastFragmentStartMs: number | null = null;

  /** Index in `timings` of the first word of the most-recently appended
   *  fragment.  Used together with `lastFragmentStartMs` to rescale that
   *  fragment's word timings without touching earlier fragments. */
  private lastFragmentFirstWordIdx = -1;

  /** Append a fragment's timings, shifted by the current accumulated duration.
   *  The fragment's local offsets (0..fragmentDurationMs) become absolute
   *  utterance offsets. */
  append(fragmentTimings: WordTiming[], fragmentDurationMs: number): void {
    const offset = this.totalDurationMs;
    this.lastFragmentStartMs = offset;
    this.lastFragmentFirstWordIdx = this.timings.length;
    for (const t of fragmentTimings) {
      this.timings.push({
        word: t.word,
        start_ms: t.start_ms + offset,
        end_ms: t.end_ms + offset,
      });
    }
    this.totalDurationMs += fragmentDurationMs;
  }

  /** Stretch the most-recently appended fragment's word timings linearly so
   *  it ends at `newEndMs` instead of its current synthesized end.  No-op
   *  if `newEndMs` is not strictly greater than the current end (we never
   *  shrink — that could leave words behind the playback cursor).
   *
   *  Called by `appendCaption` when a new fragment arrives with an anchor
   *  later than what we predicted for the previous one — the typical case
   *  when the server doesn't send `word_timings` and we synthesized too
   *  short a span using `MIN_MS_PER_WORD`.  Also called externally as more
   *  audio chunks of the in-flight fragment arrive: the playback timeline
   *  is the source of truth for "how long will this fragment really be?",
   *  so we keep extending the fragment's word distribution as the real
   *  audio piles up.  This is what keeps the karaoke marker word-by-word
   *  smooth on long, server-paced fragments instead of racing-then-sitting. */
  stretchLastFragmentTo(newEndMs: number): void {
    if (this.lastFragmentStartMs == null) return;
    if (this.lastFragmentFirstWordIdx < 0) return;
    if (newEndMs <= this.totalDurationMs) return;

    const start = this.lastFragmentStartMs;
    const oldSpan = this.totalDurationMs - start;
    const newSpan = newEndMs - start;
    if (oldSpan <= 0) {
      // Synthesized to zero span — just bump the cursor; no words to rescale.
      this.totalDurationMs = newEndMs;
      return;
    }
    const scale = newSpan / oldSpan;
    for (let i = this.lastFragmentFirstWordIdx; i < this.timings.length; i++) {
      const t = this.timings[i];
      t.start_ms = start + (t.start_ms - start) * scale;
      t.end_ms = start + (t.end_ms - start) * scale;
    }
    this.totalDurationMs = newEndMs;
  }

  /** Updates the duration of the most recently appended fragment without adding new words.
   *  Used when a metadata-only chunk arrives at the end of a stream with the true duration. */
  updateLastFragmentDuration(durationMs: number): void {
    if (this.lastFragmentStartMs == null) return;
    const newEndMs = this.lastFragmentStartMs + durationMs;
    this.stretchLastFragmentTo(newEndMs);
  }

  /** Convenience: build fragment timings from a `CaptionMeta` and append.
   *
   *  `fragmentDurationMs` from the caller is the duration of the *first audio
   *  chunk* the caption was sideband-attached to (server attaches caption to
   *  one chunk per fragment).  That number is almost always too small —
   *  fragments span many chunks.  We therefore prefer, in order:
   *
   *   1. `max(end_ms)` of server `word_timings` — the true fragment span
   *      derived from forced alignment.
   *   2. `caption.estimated_duration_ms` — server's own duration estimate.
   *   3. The chunk-duration argument as last-resort fallback.
   *
   *  Without this preference the table's `totalDurationMs` only advances by
   *  the first-chunk duration per fragment, so subsequent fragments are
   *  anchored too early in absolute time and the karaoke highlight rushes
   *  ahead of the audio (very visible on long multi-fragment turns). */
  appendCaption(
    caption: CaptionMeta,
    fragmentDurationMs: number,
    anchorMs?: number,
  ): void {
    // If the caller supplied an absolute anchor (the playback offset where
    // this fragment's audio actually starts), use it to retroactively fix
    // the previous fragment.  When the previous fragment was synthesized
    // with MIN_MS_PER_WORD it was almost always shorter than the real span
    // (~250 ms/word vs real ~350-400 ms/word), so the previous words got
    // placed earlier than the audio actually played them and the karaoke
    // marker ran ahead.  Stretching the previous fragment to end exactly
    // at this anchor keeps the marker locked to the audio.
    if (typeof anchorMs === "number" && anchorMs > this.totalDurationMs) {
      this.stretchLastFragmentTo(anchorMs);
    }

    let effectiveDurationMs = fragmentDurationMs;
    // The orchestrator now accurately pre-calculates the exact total duration of the
    // audio fragment and sends it via total_fragment_duration_ms. If it's provided,
    // we must treat it as authoritative and bypass min-word-duration fallbacks.
    let hasAuthoritativeDuration = fragmentDurationMs > 0;

    if (caption.word_timings && caption.word_timings.length > 0) {
      let maxEnd = 0;
      for (const t of caption.word_timings) {
        if (t.end_ms > maxEnd) maxEnd = t.end_ms;
      }
      if (maxEnd > 0) {
        effectiveDurationMs = maxEnd;
        hasAuthoritativeDuration = true;
      }
    } else if (
      typeof caption.estimated_duration_ms === "number" &&
      caption.estimated_duration_ms > 0
    ) {
      effectiveDurationMs = caption.estimated_duration_ms;
      hasAuthoritativeDuration = true;
    }

    // No authoritative source for fragment duration: most TTS providers
    // (OpenAI, ElevenLabs, ...) do not emit per-word boundaries, and if
    // the server didn't send total_fragment_duration_ms, we received 0.
    // Synthesize a plausible total from the word count using the
    // MIN_MS_PER_WORD floor so the karaoke reveal advances at a human-readable pace.
    if (!hasAuthoritativeDuration) {
      const words = splitWords(caption.text);
      if (words.length === 0) return;
      effectiveDurationMs = Math.max(
        fragmentDurationMs,
        words.length * MIN_MS_PER_WORD,
      );
    }

    if (effectiveDurationMs <= 0) return;
    const local = timingsForFragment(caption, effectiveDurationMs);
    if (local.length === 0) return;
    this.append(local, effectiveDurationMs);
  }

  /** Number of words in the table. */
  get wordCount(): number {
    return this.timings.length;
  }

  /** Return the count of words considered "heard" at playback offset
   *  `nowMs`, i.e. words whose `start_ms <= nowMs`.  The result is clamped
   *  to never go below `ratchetFloor`, guaranteeing the reveal can only
   *  ever advance.
   *
   *  Returns `0` for negative or zero `nowMs` (no audio yet) — unless
   *  `ratchetFloor` is higher, in which case the floor wins. */
  wordsHeardAtMs(nowMs: number, ratchetFloor = 0): number {
    if (this.timings.length === 0) return ratchetFloor;
    if (nowMs <= 0) return ratchetFloor;
    if (nowMs >= this.totalDurationMs - 50) return this.timings.length;

    // Linear scan is fine: utterances rarely exceed a few hundred words and
    // the call is on the RAF hot path where allocation matters more than
    // log-N cleverness.  Switch to binary search if profiling proves it.
    let count = 0;
    for (const t of this.timings) {
      if (t.start_ms <= nowMs) count++;
      else break;
    }
    return Math.max(ratchetFloor, count);
  }

  /** Static convenience used by the reveal hook so the call site does not
   *  need to instantiate a class to apply the ratchet rule. */
  static wordsHeardAtMs(
    timings: WordTiming[],
    nowMs: number,
    ratchetFloor = 0,
  ): number {
    if (timings.length === 0) return ratchetFloor;
    if (nowMs <= 0) return ratchetFloor;
    if (timings.length > 0 && nowMs >= timings[timings.length - 1].end_ms - 50) {
      return timings.length;
    }

    let count = 0;
    for (const t of timings) {
      if (t.start_ms <= nowMs) count++;
      else break;
    }
    return Math.max(ratchetFloor, count);
  }
}
