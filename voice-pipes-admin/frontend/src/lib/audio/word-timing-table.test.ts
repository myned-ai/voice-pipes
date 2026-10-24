// Tests for WordTimingTable and its helper functions.
//
// All tests are deterministic: no AudioContext, no React, no clocks.

import { describe, expect, it } from "vitest";
import type { CaptionMeta, WordTiming } from "../transport/types";
import {
  estimateSyllables,
  MIN_MS_PER_WORD,
  splitWords,
  timingsForFragment,
  timingsFromChars,
  timingsFromServer,
  timingsFromSyllables,
  WordTimingTable,
} from "./word-timing-table";

describe("splitWords", () => {
  it("splits on whitespace and drops empties", () => {
    expect(splitWords("hello world")).toEqual(["hello", "world"]);
    expect(splitWords("  hello   world  ")).toEqual(["hello", "world"]);
    expect(splitWords("")).toEqual([]);
    expect(splitWords("   ")).toEqual([]);
  });

  it("preserves punctuation as part of the word", () => {
    expect(splitWords("Hello, world!")).toEqual(["Hello,", "world!"]);
  });
});

describe("estimateSyllables", () => {
  it("counts vowel groups as syllables", () => {
    expect(estimateSyllables("hello")).toBe(2);
    expect(estimateSyllables("world")).toBe(1);
    expect(estimateSyllables("beautiful")).toBe(3); // "eau", "i", "u"
  });

  it("returns at least 1 for any non-empty input", () => {
    expect(estimateSyllables("rhythm")).toBe(1);
    expect(estimateSyllables("xyz")).toBe(1);
  });

  it("is case-insensitive", () => {
    expect(estimateSyllables("HELLO")).toBe(2);
  });
});

describe("timingsFromSyllables", () => {
  it("returns empty for empty text or zero duration", () => {
    expect(timingsFromSyllables("", 1000)).toEqual([]);
    expect(timingsFromSyllables("hello", 0)).toEqual([]);
    expect(timingsFromSyllables("hello", -1)).toEqual([]);
  });

  it("produces monotonic offsets that span the full duration", () => {
    const timings = timingsFromSyllables("hello world today", 3000);
    expect(timings).toHaveLength(3);
    expect(timings[0].start_ms).toBe(0);
    expect(timings[2].end_ms).toBeCloseTo(3000, 5);
    for (let i = 1; i < timings.length; i++) {
      expect(timings[i].start_ms).toBeCloseTo(timings[i - 1].end_ms, 5);
    }
  });

  it("weights words by syllable count", () => {
    // "beautiful" (3 syllables) should get ~3x the time of "hi" (1 syllable)
    const timings = timingsFromSyllables("hi beautiful", 4000);
    const hiDuration = timings[0].end_ms - timings[0].start_ms;
    const beautifulDuration = timings[1].end_ms - timings[1].start_ms;
    expect(beautifulDuration).toBeCloseTo(hiDuration * 3, 5);
  });
});

describe("timingsFromChars", () => {
  it("enforces the min-ms-per-word floor", () => {
    // 5 short words, total duration only 100 ms → would flash all words.
    // Floor kicks in: min duration becomes 5 * MIN_MS_PER_WORD = 1250 ms.
    const timings = timingsFromChars("a b c d e", 100);
    expect(timings).toHaveLength(5);
    expect(timings[4].end_ms).toBeCloseTo(5 * MIN_MS_PER_WORD, 5);
  });

  it("respects fragment duration when above the floor", () => {
    const timings = timingsFromChars("hello world", 5000);
    expect(timings[1].end_ms).toBeCloseTo(5000, 5);
  });

  it("returns empty for empty text", () => {
    expect(timingsFromChars("", 1000)).toEqual([]);
  });
});

describe("timingsFromServer", () => {
  it("returns a defensive copy", () => {
    const input: WordTiming[] = [{ word: "hi", start_ms: 0, end_ms: 100 }];
    const out = timingsFromServer(input);
    out[0].end_ms = 999;
    expect(input[0].end_ms).toBe(100); // unchanged
  });
});

describe("timingsForFragment (priority rules)", () => {
  const baseDuration = 2000;

  it("prefers server-supplied word_timings when present", () => {
    const caption: CaptionMeta = {
      text: "hello world",
      word_timings: [
        { word: "hello", start_ms: 0, end_ms: 800 },
        { word: "world", start_ms: 800, end_ms: 2000 },
      ],
    };
    const t = timingsForFragment(caption, baseDuration);
    expect(t).toHaveLength(2);
    expect(t[0].end_ms).toBe(800);
  });

  it("falls back to syllable-weighted when server timings are absent", () => {
    const caption: CaptionMeta = { text: "hello world" };
    const t = timingsForFragment(caption, baseDuration);
    expect(t).toHaveLength(2);
    // Both words are 2 syllables and 1 syllable respectively → weights 2:1
    const helloDur = t[0].end_ms - t[0].start_ms;
    const worldDur = t[1].end_ms - t[1].start_ms;
    expect(helloDur).toBeCloseTo(worldDur * 2, 5);
  });

  it("uses caption.estimated_duration_ms over the param when available", () => {
    const caption: CaptionMeta = {
      text: "hello world",
      estimated_duration_ms: 5000,
    };
    const t = timingsForFragment(caption, baseDuration);
    expect(t[1].end_ms).toBeCloseTo(5000, 5);
  });

  it("falls back to char-weighted when syllable estimator returns empty", () => {
    // splitWords returns empty → both syllable and char paths return [].
    const caption: CaptionMeta = { text: "" };
    const t = timingsForFragment(caption, baseDuration);
    expect(t).toEqual([]);
  });
});

describe("WordTimingTable.append", () => {
  it("starts empty", () => {
    const table = new WordTimingTable();
    expect(table.wordCount).toBe(0);
    expect(table.totalDurationMs).toBe(0);
  });

  it("shifts appended timings by accumulated duration", () => {
    const table = new WordTimingTable();
    table.append([{ word: "hi", start_ms: 0, end_ms: 500 }], 500);
    table.append([{ word: "there", start_ms: 0, end_ms: 700 }], 700);

    expect(table.wordCount).toBe(2);
    expect(table.totalDurationMs).toBe(1200);
    expect(table.timings[0]).toEqual({ word: "hi", start_ms: 0, end_ms: 500 });
    expect(table.timings[1]).toEqual({
      word: "there",
      start_ms: 500,
      end_ms: 1200,
    });
  });

  it("appendCaption convenience uses timingsForFragment", () => {
    const table = new WordTimingTable();
    table.appendCaption(
      {
        text: "hello",
        word_timings: [{ word: "hello", start_ms: 0, end_ms: 800 }],
      },
      800,
    );
    expect(table.wordCount).toBe(1);
    expect(table.timings[0].end_ms).toBe(800);
  });

  it("appendCaption derives fragment span from word_timings, not the chunk-duration arg", () => {
    // Server attaches caption to the FIRST audio chunk of a fragment, so
    // the chunk-duration arg (e.g. 100ms) is much smaller than the actual
    // fragment span (here 2000ms from word_timings).  Without using
    // word_timings.max(end_ms), totalDurationMs would advance by only
    // 100ms per fragment and subsequent fragments would anchor far too
    // early in absolute time — visible as the karaoke highlight rushing
    // ahead of the audio on long multi-fragment turns.
    const table = new WordTimingTable();
    table.appendCaption(
      {
        text: "one two",
        word_timings: [
          { word: "one", start_ms: 0, end_ms: 1000 },
          { word: "two", start_ms: 1000, end_ms: 2000 },
        ],
      },
      100, // chunk duration — intentionally tiny
    );
    table.appendCaption(
      {
        text: "three",
        word_timings: [{ word: "three", start_ms: 0, end_ms: 500 }],
      },
      100,
    );
    expect(table.totalDurationMs).toBe(2500);
    expect(table.timings[2]).toEqual({
      word: "three",
      start_ms: 2000,
      end_ms: 2500,
    });
  });

  it("appendCaption falls back to estimated_duration_ms when no word_timings", () => {
    const table = new WordTimingTable();
    table.appendCaption(
      { text: "hello world", estimated_duration_ms: 1500 },
      50, // chunk duration ignored
    );
    expect(table.totalDurationMs).toBe(1500);
  });

  it("appendCaption is a no-op when text has no words", () => {
    const table = new WordTimingTable();
    table.appendCaption({ text: "" }, 0);
    expect(table.wordCount).toBe(0);
    expect(table.totalDurationMs).toBe(0);
  });

  it("appendCaption synthesizes a span from word count when no authoritative duration is available", () => {
    // Real-world case: most TTS providers (OpenAI, ElevenLabs, ...) do not
    // emit per-word boundaries, and the server does not send
    // `total_fragment_duration_ms`.  The call site passes 0 in that case
    // (`total_fragment_duration_ms ?? 0`), which signals "no authoritative
    // duration" and triggers the MIN_MS_PER_WORD synthesis fallback.
    const table = new WordTimingTable();
    table.appendCaption(
      { text: "one two three four five" }, // 5 words, no timings, no estimate
      0, // 0 = no total_fragment_duration_ms from server
    );
    expect(table.wordCount).toBe(5);
    // 5 words * MIN_MS_PER_WORD (250) = 1250 ms minimum span
    expect(table.totalDurationMs).toBeGreaterThanOrEqual(1250);
  });

  it("appendCaption synthesized span never shrinks below the chunk duration", () => {
    const table = new WordTimingTable();
    table.appendCaption({ text: "hi" }, 5000);
    // Floor is max(5000, 2 * 250) = 5000
    expect(table.totalDurationMs).toBe(5000);
  });
});

describe("WordTimingTable.updateLastFragmentDuration", () => {
  it("updates the duration of the most recently appended fragment", () => {
    const table = new WordTimingTable();
    table.appendCaption({ text: "first fragment" }, 500);
    table.appendCaption({ text: "second fragment" }, 500); // 2 words * 250ms = 500ms
    
    expect(table.wordCount).toBe(4);
    expect(table.totalDurationMs).toBe(1000);
    
    // Server finally sends the real duration for the second fragment via 0-sample chunk
    // 2000ms duration. The second fragment started at 500ms, so it should end at 2500ms.
    table.updateLastFragmentDuration(2000);
    
    expect(table.wordCount).toBe(4); // No duplicated words
    expect(table.totalDurationMs).toBe(2500); // Duration updated
    expect(table.timings[3].end_ms).toBe(2500); // Timings stretched
  });

  it("is a no-op if no fragment was appended", () => {
    const table = new WordTimingTable();
    table.updateLastFragmentDuration(2000);
    expect(table.totalDurationMs).toBe(0);
  });
});

describe("WordTimingTable.wordsHeardAtMs", () => {
  function buildTable(): WordTimingTable {
    const table = new WordTimingTable();
    table.append(
      [
        { word: "one", start_ms: 0, end_ms: 300 },
        { word: "two", start_ms: 300, end_ms: 600 },
        { word: "three", start_ms: 600, end_ms: 900 },
      ],
      900,
    );
    return table;
  }

  it("returns 0 (or the floor) for nowMs <= 0", () => {
    const table = buildTable();
    expect(table.wordsHeardAtMs(0)).toBe(0);
    expect(table.wordsHeardAtMs(-50)).toBe(0);
    expect(table.wordsHeardAtMs(-50, 2)).toBe(2);
  });

  it("counts words whose start_ms <= nowMs", () => {
    const table = buildTable();
    expect(table.wordsHeardAtMs(50)).toBe(1); // "one" started at 0
    expect(table.wordsHeardAtMs(300)).toBe(2); // "two" started at 300
    expect(table.wordsHeardAtMs(601)).toBe(3); // "three" started at 600
    expect(table.wordsHeardAtMs(99999)).toBe(3);
  });

  it("respects the ratchet floor (never goes backwards)", () => {
    const table = buildTable();
    expect(table.wordsHeardAtMs(50, 2)).toBe(2);
    expect(table.wordsHeardAtMs(50, 3)).toBe(3);
  });

  it("static variant matches the instance method", () => {
    const table = buildTable();
    expect(WordTimingTable.wordsHeardAtMs(table.timings, 350, 0)).toBe(2);
    expect(WordTimingTable.wordsHeardAtMs(table.timings, 350, 5)).toBe(5);
  });

  it("returns the floor for an empty table", () => {
    const table = new WordTimingTable();
    expect(table.wordsHeardAtMs(1000)).toBe(0);
    expect(table.wordsHeardAtMs(1000, 4)).toBe(4);
  });
});
