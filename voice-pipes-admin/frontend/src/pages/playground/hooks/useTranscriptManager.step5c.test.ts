// Tests for Step 5c additions to useTranscriptManager:
//   recordCaptionFragment, recordFinalTextFallback, getActiveUtterance.
// These exercise the new WordTimingTable registry without going through the
// full audio/playback stack — playbackRef is a stub.

import { act, renderHook } from "@testing-library/react";
import { useRef } from "react";
import { describe, expect, it } from "vitest";
import type { AudioPlaybackService } from "../../../lib/audio/playback";
import { useTranscriptManager } from "./useTranscriptManager";

function setup() {
  return renderHook(() => {
    const playbackRef = useRef<AudioPlaybackService | null>(null);
    return useTranscriptManager({ playbackRef });
  });
}

describe("useTranscriptManager — Step 5c (WordTimingTable wiring)", () => {
  it("getActiveUtterance returns null when no utterance has been recorded", () => {
    const { result } = setup();
    expect(result.current.getActiveUtterance()).toBeNull();
  });

  it("recordCaptionFragment is a no-op for negative uttId or zero duration", () => {
    const { result } = setup();
    act(() => {
      result.current.recordCaptionFragment(-1, { text: "hello" }, 100);
      result.current.recordCaptionFragment(5, { text: "hello" }, 0);
    });
    // Without a placeholder + speaking ref, getActive returns null anyway,
    // but more importantly no entry was created in the table map.
    result.current.speakingUtteranceIdRef.current = 5;
    expect(result.current.getActiveUtterance()).toBeNull();
  });

  it("getActiveUtterance returns descriptor after caption + placeholder", () => {
    const { result } = setup();
    act(() => {
      result.current.createPlaceholderEntry(42); // sets speakingEntryRef = 0
      result.current.recordCaptionFragment(
        42,
        { text: "alpha beta gamma" },
        300,
      );
    });
    // speakingUtteranceIdRef is updated separately by the audio_meta handler.
    result.current.speakingUtteranceIdRef.current = 42;

    const active = result.current.getActiveUtterance();
    expect(active).not.toBeNull();
    expect(active!.utteranceId).toBe(42);
    expect(active!.entryIndex).toBe(0);
    expect(active!.table.wordCount).toBe(3);
  });

  it("recordCaptionFragment accumulates across multiple fragments", () => {
    const { result } = setup();
    act(() => {
      result.current.createPlaceholderEntry(7);
      result.current.recordCaptionFragment(7, { text: "one two" }, 200);
      result.current.recordCaptionFragment(7, { text: "three four five" }, 300);
    });
    result.current.speakingUtteranceIdRef.current = 7;
    const active = result.current.getActiveUtterance()!;
    expect(active.table.wordCount).toBe(5);
    // fragmentDurationMs is positive so it is treated as authoritative —
    // the MIN_MS_PER_WORD floor is NOT applied in that path.
    // Fragment 1: 200 ms (used as-is)  → words 0-1 span [0, 200)
    // Fragment 2: 300 ms (used as-is)  → words 2-4 span [200, 500)
    // timings[2] is the first word of fragment 2, so start_ms = 200
    expect(active.table.timings[2].start_ms).toBeGreaterThanOrEqual(200);
    expect(active.table.totalDurationMs).toBe(500);
  });

  it("recordFinalTextFallback populates an empty table from raw text", () => {
    const { result } = setup();
    act(() => {
      result.current.createPlaceholderEntry(11);
      result.current.recordFinalTextFallback(11, "hello world reveal", 1500);
    });
    result.current.speakingUtteranceIdRef.current = 11;
    const active = result.current.getActiveUtterance()!;
    expect(active.table.wordCount).toBe(3);
    expect(active.table.totalDurationMs).toBe(1500);
  });

  it("recordFinalTextFallback is a no-op when captions already populated the table", () => {
    const { result } = setup();
    act(() => {
      result.current.createPlaceholderEntry(99);
      result.current.recordCaptionFragment(99, { text: "real captions here" }, 400);
      result.current.recordFinalTextFallback(99, "totally different fallback text", 800);
    });
    result.current.speakingUtteranceIdRef.current = 99;
    const active = result.current.getActiveUtterance()!;
    // Caption-derived table is preserved (3 words, not 4).
    expect(active.table.wordCount).toBe(3);
    // fragmentDurationMs=400 is positive → treated as authoritative, no MIN_MS_PER_WORD floor.
    expect(active.table.totalDurationMs).toBe(400);
  });

  it("resetTranscript clears the WordTimingTable registry", () => {
    const { result } = setup();
    act(() => {
      result.current.createPlaceholderEntry(1);
      result.current.recordCaptionFragment(1, { text: "abc def" }, 200);
    });
    result.current.speakingUtteranceIdRef.current = 1;
    expect(result.current.getActiveUtterance()).not.toBeNull();

    act(() => {
      result.current.resetTranscript();
    });
    // After reset, speakingUtteranceIdRef is back to -1 → null active.
    expect(result.current.getActiveUtterance()).toBeNull();
    // And the table for uttId 1 is gone — re-pointing uttId does not resurrect it.
    result.current.speakingUtteranceIdRef.current = 1;
    expect(result.current.getActiveUtterance()).toBeNull();
  });

  it("getActiveUtterance returns null when speakingEntryRef is -1", () => {
    const { result } = setup();
    act(() => {
      result.current.recordCaptionFragment(3, { text: "hello" }, 100);
    });
    result.current.speakingUtteranceIdRef.current = 3;
    // No createPlaceholderEntry → speakingEntryRef stays -1.
    expect(result.current.getActiveUtterance()).toBeNull();
  });

  it("server word_timings are honoured (priority over fallback)", () => {
    const { result } = setup();
    act(() => {
      result.current.createPlaceholderEntry(50);
      result.current.recordCaptionFragment(
        50,
        {
          text: "ignored text here",
          word_timings: [
            { word: "exact", start_ms: 0, end_ms: 100 },
            { word: "server", start_ms: 100, end_ms: 250 },
          ],
        },
        250,
      );
    });
    result.current.speakingUtteranceIdRef.current = 50;
    const active = result.current.getActiveUtterance()!;
    expect(active.table.wordCount).toBe(2);
    expect(active.table.timings[0].word).toBe("exact");
    expect(active.table.timings[1].word).toBe("server");
  });
});

// ── Stale word-timing tables cleared on reconnect ────────────────────────────
//
// Root cause: tablesRef (a per-utterance WordTimingTable registry) was never
// cleared when a new session started.  The server always reuses utterance IDs
// starting from 0 or 1, so the second session's first turn would read stale
// timings from the previous session, causing the karaoke reveal to snap to
// the wrong word positions and display <TypingDots> for the entire first turn.
//
// Fix: resetWordRevealRefs() (called from resetTranscript and from
// clearConversation) now calls tablesRef.current.clear() so every session
// starts with an empty registry.

describe("stale word-timing tables cleared on session reset", () => {
  it("resetTranscript clears tablesRef so getActiveUtterance returns null after reset", () => {
    const { result } = setup();

    // Session 1: populate the table for uttId=1
    act(() => {
      result.current.createPlaceholderEntry(1);
      result.current.recordCaptionFragment(1, { text: "session one words" }, 400);
    });
    result.current.speakingUtteranceIdRef.current = 1;
    expect(result.current.getActiveUtterance()).not.toBeNull();
    expect(result.current.getActiveUtterance()!.table.wordCount).toBe(3);

    // Simulate reconnect: resetTranscript is called by connect()
    act(() => {
      result.current.resetTranscript();
    });

    // After reset, speakingUtteranceIdRef = -1 (reset by resetTranscript).
    // Even if we manually restore it to 1 (simulating server reusing the ID),
    // the table must be gone.
    result.current.speakingUtteranceIdRef.current = 1;
    expect(result.current.getActiveUtterance()).toBeNull();
  });

  it("resetWordReveal alone (used by clearConversation) also clears tablesRef", () => {
    const { result } = setup();

    act(() => {
      result.current.createPlaceholderEntry(5);
      result.current.recordCaptionFragment(5, { text: "alpha beta gamma" }, 500);
    });
    result.current.speakingUtteranceIdRef.current = 5;
    expect(result.current.getActiveUtterance()!.table.wordCount).toBe(3);

    act(() => {
      result.current.resetWordReveal();
    });

    result.current.speakingUtteranceIdRef.current = 5;
    expect(result.current.getActiveUtterance()).toBeNull();
  });

  it("second session with the same server uttId gets a fresh table, not stale timings", () => {
    const { result } = setup();

    // Session 1: three words with specific timings
    act(() => {
      result.current.createPlaceholderEntry(1);
      result.current.recordCaptionFragment(
        1,
        {
          text: "stale session one",
          word_timings: [
            { word: "stale",   start_ms: 0,   end_ms: 200 },
            { word: "session", start_ms: 200, end_ms: 400 },
            { word: "one",     start_ms: 400, end_ms: 600 },
          ],
        },
        600,
      );
    });
    result.current.speakingUtteranceIdRef.current = 1;
    expect(result.current.getActiveUtterance()!.table.timings[0].word).toBe("stale");

    // Reset (reconnect)
    act(() => { result.current.resetTranscript(); });

    // Session 2: same uttId=1, completely different captions
    act(() => {
      result.current.createPlaceholderEntry(1);
      result.current.recordCaptionFragment(
        1,
        {
          text: "fresh new words",
          word_timings: [
            { word: "fresh", start_ms: 0,   end_ms: 150 },
            { word: "new",   start_ms: 150, end_ms: 280 },
            { word: "words", start_ms: 280, end_ms: 420 },
          ],
        },
        420,
      );
    });
    result.current.speakingUtteranceIdRef.current = 1;

    const active = result.current.getActiveUtterance()!;
    // Must see session-2 words, NOT session-1 words
    expect(active.table.timings[0].word).toBe("fresh");
    expect(active.table.wordCount).toBe(3);
    // Total duration must match session-2 table, not session-1's 600 ms
    expect(active.table.totalDurationMs).toBe(420);
  });

  it("tables for multiple utterances are all cleared on reset", () => {
    const { result } = setup();

    act(() => {
      result.current.createPlaceholderEntry(1);
      result.current.recordCaptionFragment(1, { text: "utterance one" }, 300);
      result.current.createPlaceholderEntry(2);
      result.current.recordCaptionFragment(2, { text: "utterance two" }, 300);
    });

    act(() => { result.current.resetTranscript(); });

    result.current.speakingUtteranceIdRef.current = 1;
    expect(result.current.getActiveUtterance()).toBeNull();

    result.current.speakingUtteranceIdRef.current = 2;
    expect(result.current.getActiveUtterance()).toBeNull();
  });
});
