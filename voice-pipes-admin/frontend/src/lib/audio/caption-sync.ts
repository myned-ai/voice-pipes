// CaptionSync — word-by-word caption highlighting synchronized to audio playback.

import type { CaptionMeta, WordTiming } from "../transport/types";

/** Estimate word timings proportionally from character count. */
function estimateWordTimings(text: string, totalDurationMs: number): WordTiming[] {
  const words = text.split(/\s+/).filter(Boolean);
  const totalChars = words.reduce((sum, w) => sum + w.length, 0);
  if (totalChars === 0 || words.length === 0) return [];

  let offsetMs = 0;
  return words.map((word) => {
    const fraction = word.length / totalChars;
    const duration = Math.round(fraction * totalDurationMs);
    const timing: WordTiming = { word, start_ms: offsetMs, end_ms: offsetMs + duration };
    offsetMs += duration;
    return timing;
  });
}

interface ScheduledSentence {
  text: string;
  words: WordTiming[];
  /** AudioContext time (seconds) when the first chunk of this sentence plays. */
  audioStartTime: number;
  /** Total audio duration for this sentence (seconds). */
  audioDurationSec: number;
  /** Index of the last highlighted word (-1 = none yet). */
  lastHighlighted: number;
  /** Whether the sentence has been fully revealed. */
  completed: boolean;
}

export class CaptionSync {
  private sentences: ScheduledSentence[] = [];
  private rafId: number | null = null;
  private audioCtx: AudioContext | null = null;
  /** End time of the last scheduled AudioBufferSourceNode — used for underrun detection. */
  private lastScheduledEndTime = 0;

  /** Fires when a new sentence starts playing. */
  onSentenceStart: ((text: string, sentenceIdx: number) => void) | null = null;
  /** Fires when the highlighted word changes. */
  onWordHighlight: ((sentenceIdx: number, wordIdx: number, word: string) => void) | null = null;
  /** Fires when a sentence has been fully spoken. */
  onSentenceComplete: ((sentenceIdx: number) => void) | null = null;

  /** Set the AudioContext to track playback position against. */
  setAudioContext(ctx: AudioContext | null): void {
    this.audioCtx = ctx;
  }

  /** Update the end time of the last scheduled audio buffer (for underrun clamping). */
  updateLastScheduledEndTime(endTime: number): void {
    if (endTime > this.lastScheduledEndTime) {
      this.lastScheduledEndTime = endTime;
    }
  }

  /** Register a new sentence when a caption-bearing audio chunk is scheduled.
   *  @param caption  The CaptionMeta from the audio chunk
   *  @param audioStartTime  AudioContext.currentTime when the first chunk plays
   *  @param audioDurationSec  Total scheduled duration for this sentence's audio */
  addSentence(caption: CaptionMeta, audioStartTime: number, audioDurationSec: number): void {
    const durationMs = audioDurationSec * 1000;
    const words =
      caption.word_timings && caption.word_timings.length > 0
        ? caption.word_timings
        : estimateWordTimings(caption.text, caption.estimated_duration_ms ?? durationMs);

    const idx = this.sentences.length;
    this.sentences.push({
      text: caption.text,
      words,
      audioStartTime,
      audioDurationSec,
      lastHighlighted: -1,
      completed: false,
    });

    this.onSentenceStart?.(caption.text, idx);
    this.startLoop();
  }

  /** Extend the duration of the last sentence (when subsequent chunks for the same
   *  sentence arrive and we learn the true total duration). */
  extendLastSentence(additionalDurationSec: number): void {
    const last = this.sentences[this.sentences.length - 1];
    if (!last) return;
    last.audioDurationSec += additionalDurationSec;
    // Re-estimate word timings with the updated duration
    if (!last.words.some((w) => w.start_ms > 0 || w.end_ms > 0)) return;
    const durationMs = last.audioDurationSec * 1000;
    const reEstimated = estimateWordTimings(last.text, durationMs);
    if (reEstimated.length === last.words.length) {
      last.words = reEstimated;
    }
  }

  /** rAF loop: highlight words based on current playback position. */
  private tick = (): void => {
    if (!this.audioCtx) {
      this.rafId = requestAnimationFrame(this.tick);
      return;
    }

    let now = this.audioCtx.currentTime;

    // Underrun detection: if currentTime has advanced past the last scheduled
    // buffer end, clamp so captions don't race ahead into silence.
    if (this.lastScheduledEndTime > 0 && now > this.lastScheduledEndTime) {
      now = this.lastScheduledEndTime;
    }

    for (let i = 0; i < this.sentences.length; i++) {
      const s = this.sentences[i];
      if (s.completed) continue;

      const elapsedMs = (now - s.audioStartTime) * 1000;
      if (elapsedMs < 0) continue; // not started yet

      // Find which word should be highlighted
      let activeIdx = -1;
      for (let w = s.words.length - 1; w >= 0; w--) {
        if (s.words[w].start_ms <= elapsedMs) {
          activeIdx = w;
          break;
        }
      }

      if (activeIdx !== s.lastHighlighted && activeIdx >= 0) {
        s.lastHighlighted = activeIdx;
        this.onWordHighlight?.(i, activeIdx, s.words[activeIdx].word);
      }

      // Check if sentence is fully played
      if (elapsedMs >= s.audioDurationSec * 1000) {
        s.completed = true;
        this.onSentenceComplete?.(i);
      }
    }

    this.rafId = requestAnimationFrame(this.tick);
  };

  private startLoop(): void {
    if (this.rafId != null) return;
    this.rafId = requestAnimationFrame(this.tick);
  }

  /** Get all sentence texts accumulated so far. */
  getSentences(): string[] {
    return this.sentences.map((s) => s.text);
  }

  /** Returns the text of every sentence that has started playing at the current
   *  AudioContext time.  Call this BEFORE flush() — flush clears the sentence list.
   *
   *  Used on interruption to trim the transcript entry to only what the user
   *  actually heard.  Returns null when no AudioContext is available (text-only
   *  mode, or called before the first audio chunk played). */
  getPlayedText(): string | null {
    if (!this.audioCtx) return null;
    const now = this.audioCtx.currentTime;
    const played = this.sentences.filter((s) => s.audioStartTime <= now);
    if (played.length === 0) return null;
    return played.map((s) => s.text).join(" ");
  }

  /** Stop highlighting and clear state (barge-in / flush). */
  flush(): void {
    if (this.rafId != null) {
      cancelAnimationFrame(this.rafId);
      this.rafId = null;
    }
    this.sentences = [];
    this.lastScheduledEndTime = 0;
  }
}
