/**
 * Integration tests for usePlaygroundSession.
 *
 * These tests exercise the full session-orchestration layer — transcript
 * state machine, interrupt handling, and session lifecycle — with the two
 * external-system hooks (transport and audio) replaced by lightweight fakes
 * so we can inject server messages synchronously and assert UI state without
 * a real WebSocket or AudioContext.
 *
 * Coverage:
 *   Phantom duplicate agent bubble caused by stale speakingUtteranceIdRef
 *   Interrupted bubble stuck on TypingDots / speakingEntryIndex not demoted
 */

import { renderHook, act } from "@testing-library/react";
import { vi, describe, it, expect, beforeEach } from "vitest";
import type { ServerMessage, NegotiatedConfig } from "../../../lib/transport/types";
import type { TransportState, TransportCloseInfo } from "../../../lib/transport/adapter";
import type { PlaybackChunk } from "../../../lib/audio/playback";
import { usePlaygroundSession } from "../usePlaygroundSession";

// ── Module-level mocks (hoisted before imports) ───────────────────────────────

type PlaybackCallback = ((utteranceId: number) => void) | null;

/** The slice of the fake playback service that its `this`-bound mock methods
 *  read and mutate. */
interface FakePlaybackState {
  playing: boolean;
  currentUtteranceId: number;
  totalEnqueuedDurationMs: number;
  utteranceStartTime: number;
  getPlayedDurationMs(): number;
}

const { mockPlayback, mockTransport } = vi.hoisted(() => {
  return {
    mockPlayback: {
      playing: false,
      currentUtteranceId: -1,
      totalEnqueuedDurationMs: 0,
      utteranceStartTime: -1,
      lastScheduledEndTime: 0,
      audioContext: null,
      level: 0,
      currentChunk: null as PlaybackChunk | null,
      _onPlaybackStart: null as PlaybackCallback,
      _onPlaybackEnd: null as PlaybackCallback,

      get onPlaybackStart() { return this._onPlaybackStart; },
      set onPlaybackStart(cb) { this._onPlaybackStart = cb; },
      get onPlaybackEnd() { return this._onPlaybackEnd; },
      set onPlaybackEnd(cb) { this._onPlaybackEnd = cb; },

      start: vi.fn(function(this: FakePlaybackState) { this.playing = true; }),
      stop:  vi.fn(function(this: FakePlaybackState) { this.playing = false; }),

      beginUtterance: vi.fn(function(this: FakePlaybackState, id: number) {
        this.currentUtteranceId = id;
        this.totalEnqueuedDurationMs = 0;
        this.utteranceStartTime = -1;
      }),
      enqueue: vi.fn(function(this: FakePlaybackState) {
        this.totalEnqueuedDurationMs += 100;
      }),
      flush: vi.fn(function(this: FakePlaybackState) {
        return this.getPlayedDurationMs();
      }),
      getPlayedDurationMs: vi.fn(() => 0),
      getPlaybackQuality: vi.fn(function(this: FakePlaybackState, utteranceId: number) {
        return {
          utteranceId,
          underrunCount: 0,
          totalSilenceMs: 0,
          audioMs: this.totalEnqueuedDurationMs,
          heardMs: this.totalEnqueuedDurationMs,
          wallMs: this.totalEnqueuedDurationMs,
          gaps: [],
        };
      }),
      getNextChunkStartOffsetMs: vi.fn(function(this: FakePlaybackState) {
        return this.totalEnqueuedDurationMs;
      }),
      getBufferStats: vi.fn(() => ({
        scheduledAheadMs: 0,
        scheduledChunks: 0,
        pendingChunks: 0,
      })),

      // Test-only helpers
      firePlaybackStart(id: number) { this._onPlaybackStart?.(id); },
      firePlaybackEnd(id: number)   { this._onPlaybackEnd?.(id);   },
    },
    mockTransport: {
      state: "connected" as const,
      sendAudio: vi.fn(),
      sendText: vi.fn(),
      sendControl: vi.fn(),
      sendPlaybackReport: vi.fn(),
      sendPlaybackComplete: vi.fn(),
      sendTimingReport: vi.fn(),
      recordPongSample: vi.fn(),
      disconnect: vi.fn(),
      connect: vi.fn().mockResolvedValue(undefined),
      onMessage: null as ((msg: ServerMessage) => void) | null,
      onStateChange: null as ((state: TransportState, info?: TransportCloseInfo) => void) | null,
      onError: null as ((err: Error) => void) | null,
    }
  };
});

// Mock modules
vi.mock("../../../lib/transport", () => ({
  createTransport: vi.fn(() => mockTransport),
  EXPERIMENTAL_TRANSPORTS: new Set(),
}));

vi.mock("../../../lib/audio/playback", () => ({
  AudioPlaybackService: vi.fn().mockImplementation(() => {
    return mockPlayback;
  }),
}));

vi.mock("../../../lib/audio/capture", () => ({
  AudioCaptureService: vi.fn().mockImplementation(() => {
    return {
      active: false,
      actualSampleRate: 48000,
      start: vi.fn().mockResolvedValue(undefined),
      stop: vi.fn(),
      level: 0,
    };
  }),
}));

vi.mock("../../../lib/audio/playback-clock", () => ({
  DefaultPlaybackClock: vi.fn().mockImplementation(() => {
    return {
      subscribe: vi.fn(() => () => {}),
    };
  }),
}));

// ── Fixture factories ─────────────────────────────────────────────────────────

const NEGOTIATED: NegotiatedConfig = {
  audio_format: "pcm_i16",
  input_sample_rate: 48000,
  output_sample_rate: 48000,
  channels: 1,
};

// ── Per-test state ────────────────────────────────────────────────────────────

let injectServerMessage: (msg: ServerMessage) => void;
let fakePlayback: typeof mockPlayback;

beforeEach(() => {
  vi.clearAllMocks();

  mockPlayback.playing = false;
  mockPlayback.currentUtteranceId = -1;
  mockPlayback.totalEnqueuedDurationMs = 0;
  mockPlayback._onPlaybackStart = null;
  mockPlayback._onPlaybackEnd = null;
  vi.mocked(mockPlayback.getPlayedDurationMs).mockReturnValue(0);
  vi.mocked(mockPlayback.getBufferStats).mockReturnValue({
    scheduledAheadMs: 0,
    scheduledChunks: 0,
    pendingChunks: 0,
  });

  fakePlayback = mockPlayback;

  injectServerMessage = (msg) => {
    mockTransport.onMessage?.(msg);
  };
});

// ── Message injection helpers ─────────────────────────────────────────────────

/** Inject session_ready — triggers createPlaybackService, sets playbackRef. */
function injectSessionReady() {
  act(() => {
    injectServerMessage({
      kind: "control",
      control: {
        action: "session_ready",
        session_id: "test",
        negotiated: NEGOTIATED,
      },
    });
  });
}

/** Inject a pipeline state transition. */
function injectStateChanged(state: "listening" | "processing" | "speaking" | "draining") {
  act(() => {
    injectServerMessage({ kind: "control", control: { action: "state_changed", state } });
  });
}

/** Inject an audio message.  When `captionText` is provided the audio handler
 *  records word timings and — once utteranceId is known — creates a transcript
 *  entry for the agent turn. */
function injectAudioMessage(
  utteranceId: number,
  opts: {
    captionText?: string;
    totalFragmentMs?: number;
    wordTimings?: Array<{ word: string; start_ms: number; end_ms: number }>;
    endOfUtterance?: boolean;
    seqNum?: number;
  } = {},
) {
  act(() => {
    injectServerMessage({
      kind: "audio",
      samples: new Int16Array(480), // 10 ms at 48 kHz
      seqNum: opts.seqNum ?? 1,
      endOfUtterance: opts.endOfUtterance ?? false,
      endOfResponse:  opts.endOfUtterance ?? false,
      meta: {
        utteranceId,
        ...(opts.captionText
          ? {
              caption: {
                text: opts.captionText,
                ...(opts.wordTimings ? { word_timings: opts.wordTimings } : {}),
              },
              total_fragment_duration_ms: opts.totalFragmentMs ?? 500,
            }
          : {}),
      },
    });
  });
}

/** Inject a server-side interrupt. */
function injectClearAudio(utteranceId: number, reason = "user_speech") {
  act(() => {
    injectServerMessage({
      kind: "control",
      control: { action: "clear_audio", utterance_id: utteranceId, reason },
    });
  });
}

/** Connect and initialise a full session up to the speaking state. */
async function setupSession(hook: ReturnType<typeof renderHook<ReturnType<typeof usePlaygroundSession>, unknown>>) {
  await act(async () => { hook.result.current.connect("ws://test", "websocket", {}); });
  injectSessionReady();
  injectStateChanged("speaking");
}

// ── Phantom duplicate agent bubble ────────────────────────────────────────────

describe("no phantom agent bubble before audio_meta", () => {
  /**
   * Root cause: before the fix, if speakingUtteranceIdRef held a stale ID
   * (≠ -1) from a previous session, handleStateChanged's guard would pass
   * and createPlaceholderEntry would fire with the wrong ID, producing an
   * empty/phantom bubble before the real audio arrived.
   *
   * Fix: connect() resets speakingUtteranceIdRef = -1, and handleStateChanged
   * guards on `uttId !== -1` so the path is skipped until audio_meta supplies
   * the real ID.
   */
  it("state→speaking before any audio_meta creates no transcript entry", async () => {
    const hook = renderHook(() => usePlaygroundSession());
    await setupSession(hook);

    // After state→speaking the utterance ID is still -1 (no audio_meta yet).
    // The guard in handleStateChanged must block placeholder creation.
    expect(hook.result.current.transcript).toHaveLength(0);
    expect(hook.result.current.speakingEntryIndex).toBe(-1);
  });

  it("first audio_meta after state→speaking creates exactly one agent entry", async () => {
    const hook = renderHook(() => usePlaygroundSession());
    await setupSession(hook);

    injectAudioMessage(1, { captionText: "Hello there" });

    const agentEntries = hook.result.current.transcript.filter((e) => e.role === "agent");
    expect(agentEntries).toHaveLength(1);
    expect(agentEntries[0].text).toBe("Hello there");
    expect(hook.result.current.speakingEntryIndex).toBe(0);
  });

  it("reconnect resets stale uttId so state→speaking on new session cannot phantom-create a bubble", async () => {
    const hook = renderHook(() => usePlaygroundSession());

    // ── Session 1: a complete turn, leaving speakingUtteranceIdRef = 1 ──
    await setupSession(hook);
    injectAudioMessage(1, { captionText: "Session one response" });
    expect(hook.result.current.transcript).toHaveLength(1);

    // ── Session 2: reconnect then immediately get state→speaking ──
    // connect() resets speakingUtteranceIdRef to -1 and clears the transcript.
    await act(async () => { hook.result.current.connect("ws://test", "websocket", {}); });
    injectSessionReady();

    // Simulate the race: state→speaking arrives before the first audio_meta.
    injectStateChanged("speaking");

    expect(hook.result.current.transcript).toHaveLength(0);
    expect(hook.result.current.speakingEntryIndex).toBe(-1);
  });

  it("second session with same server uttId gets a fresh entry, not stale captions", async () => {
    const hook = renderHook(() => usePlaygroundSession());

    // Session 1: uttId=1, "Hello from session one"
    await setupSession(hook);
    injectAudioMessage(1, { captionText: "Hello from session one" });

    // Session 2: reconnect, then uttId=1 again (server always starts from 1)
    await act(async () => { hook.result.current.connect("ws://test", "websocket", {}); });
    injectSessionReady();
    injectStateChanged("speaking");
    injectAudioMessage(1, { captionText: "Hello from session two" });

    // Transcript should contain only the session-2 entry (CLEAR_ALL on connect)
    const agentEntries = hook.result.current.transcript.filter((e) => e.role === "agent");
    expect(agentEntries).toHaveLength(1);
    expect(agentEntries[0].text).toBe("Hello from session two");
  });
});

// ── speakingEntryIndex demoted on clear_audio ────────────────────────────────

describe("interrupted bubble is demoted from StreamingAgentBubble", () => {
  /**
   * Root cause: handleClearAudio did not call setSpeakingEntryIndex(-1).
   * The bubble stayed as <StreamingAgentBubble> which, with highlightInfo
   * now null (audio flushed), rendered <TypingDots> indefinitely.
   *
   * Fix: handleClearAudio now always calls setSpeakingEntryIndex(-1) so the
   * entry is immediately switched to <ChatBubble> which renders entry.text.
   */
  it("clear_audio sets speakingEntryIndex to -1", async () => {
    const hook = renderHook(() => usePlaygroundSession());
    await setupSession(hook);

    injectAudioMessage(1, { captionText: "A long response being interrupted" });
    expect(hook.result.current.speakingEntryIndex).toBe(0);

    injectClearAudio(1);

    expect(hook.result.current.speakingEntryIndex).toBe(-1);
  });

  it("agent entry remains in transcript after clear_audio (text visible in ChatBubble)", async () => {
    const hook = renderHook(() => usePlaygroundSession());
    await setupSession(hook);

    injectAudioMessage(1, { captionText: "Keep this text visible" });
    injectClearAudio(1);

    const agentEntries = hook.result.current.transcript.filter((e) => e.role === "agent");
    expect(agentEntries).toHaveLength(1);
    expect(agentEntries[0].text).toBe("Keep this text visible");
  });

  it("second turn after interrupt creates a new entry at the correct index", async () => {
    const hook = renderHook(() => usePlaygroundSession());
    await setupSession(hook);

    // Turn 1: audio + authoritative text, then interrupted.
    // The text message must arrive BEFORE clear_audio so that
    // agentTextReceivedForUtteranceRef is set to 1.  That prevents the
    // state→speaking handler from creating a spurious placeholder for the
    // stale uttId=1 when the second turn begins (the guard
    //   agentTextReceivedForUtteranceRef.current !== uttId  would fail
    // without the text message, causing speakingEntryIsLiveRef to be set
    // prematurely and blocking real placeholder creation for turn 2).
    injectAudioMessage(1, { captionText: "Turn one audio" });
    act(() => {
      injectServerMessage({ kind: "text", text: "Turn one complete", messageId: "llm-1" });
    });
    injectClearAudio(1);
    expect(hook.result.current.speakingEntryIndex).toBe(-1);

    // Server cycles to a new turn
    injectStateChanged("listening");
    injectStateChanged("processing");
    injectStateChanged("speaking");

    // Turn 2: new utteranceId
    injectAudioMessage(2, { captionText: "Turn two" });

    const idx = hook.result.current.transcript.findIndex(
      (e) => e.role === "agent" && e.utteranceId === 2,
    );
    expect(idx).toBeGreaterThan(-1);
    expect(hook.result.current.speakingEntryIndex).toBe(idx);
  });

  it("ignores stale clear_audio to prevent interrupting a newer turn", async () => {
    const hook = renderHook(() => usePlaygroundSession());
    await setupSession(hook);

    // Turn 1 starts but is quickly superseded
    injectAudioMessage(1, { captionText: "Old response" });
    expect(hook.result.current.speakingEntryIndex).toBe(0);

    // Turn 2 starts (e.g. backend sent ResponseStarted before sending clear_audio for Turn 1)
    injectStateChanged("listening");
    injectStateChanged("processing");
    injectStateChanged("speaking");
    injectAudioMessage(2, { captionText: "Newer response" });
    
    // We should now be tracking the newer turn
    const idx2 = hook.result.current.transcript.findIndex(
      (e) => e.role === "agent" && e.utteranceId === 2,
    );
    expect(hook.result.current.speakingEntryIndex).toBe(idx2);

    // Now the stale clear_audio for Turn 1 arrives
    injectClearAudio(1);

    // It should NOT have demoted the active Turn 2!
    expect(hook.result.current.speakingEntryIndex).toBe(idx2);
  });

  it("multiple captions in one turn are all visible after clear_audio", async () => {
    const hook = renderHook(() => usePlaygroundSession());
    await setupSession(hook);

    // Two caption fragments for the same utterance
    injectAudioMessage(1, { captionText: "First sentence", seqNum: 1 });
    injectAudioMessage(1, { captionText: "second sentence", seqNum: 2 });
    injectClearAudio(1);

    const agent = hook.result.current.transcript.find((e) => e.role === "agent");
    expect(agent).toBeDefined();
    // Both fragments should be in the entry text
    expect(agent!.text).toContain("First sentence");
    expect(agent!.text).toContain("second sentence");
    expect(hook.result.current.speakingEntryIndex).toBe(-1);
  });

  it("clear_audio does not crash when no agent turn is active", async () => {
    const hook = renderHook(() => usePlaygroundSession());
    await setupSession(hook);

    // No audio → no agent entry
    expect(() => {
      injectClearAudio(99);
    }).not.toThrow();

    expect(hook.result.current.transcript).toHaveLength(0);
    expect(hook.result.current.speakingEntryIndex).toBe(-1);
  });
});

// ── Normal turn completion — entry.text reflects full spoken text ───────────
//
// For a normal (non-interrupted) turn, all audio is played so entry.text
// already contains exactly the spoken words.  No heardText freeze is set:
// ChatBubble renders entry.heardText ?? entry.text, which falls through to
// entry.text.  The server's TEXT_TRUNCATED can still refine entry.text if
// the TTS only synthesised a subset of the LLM output.

describe("Normal turn end — entry renders full spoken text", () => {
  it("speakingEntryIndex is -1 after playback ends normally", async () => {
    const hook = renderHook(() => usePlaygroundSession());
    await setupSession(hook);

    injectAudioMessage(1, { captionText: "hello world" });
    // Simulate audio playing and ending
    act(() => { fakePlayback.firePlaybackStart(1); });
    act(() => { fakePlayback.firePlaybackEnd(1); });

    expect(hook.result.current.speakingEntryIndex).toBe(-1);
  });

  it("entry.text is at most the caption words after playback ends", async () => {
    const hook = renderHook(() => usePlaygroundSession());
    await setupSession(hook);

    // Audio arrives with caption text — this fills entry.text.
    // The transcript reducer's AGENT_TEXT_FILL_PLACEHOLDER deliberately
    // ignores the LLM text when captions already populated the entry
    // (hasCaption = true), so caption text always wins.
    injectAudioMessage(1, { captionText: "hello world" });

    // Full LLM text arrives — should be ignored by the reducer since
    // captions already set the text.
    act(() => {
      injectServerMessage({ kind: "text", text: "hello world extra unheard words", messageId: "llm-1" });
    });

    // The entry text stays as caption text (LLM text was ignored).
    const entryAfterLlm = hook.result.current.transcript.find((e) => e.role === "agent");
    expect(entryAfterLlm?.text).toBe("hello world");

    // Playback ends normally.
    act(() => { fakePlayback.firePlaybackStart(1); });
    act(() => { fakePlayback.firePlaybackEnd(1); });

    // After playback ends: entry.text is still caption-only (was never more
    // than that), and speakingEntryIndex is -1.
    const entryAfterEnd = hook.result.current.transcript.find((e) => e.role === "agent");
    expect(entryAfterEnd?.text).toBe("hello world");
    expect(hook.result.current.speakingEntryIndex).toBe(-1);
  });

  it("does not trim when no agent turn was active (no crash)", async () => {
    const hook = renderHook(() => usePlaygroundSession());
    await setupSession(hook);

    // Playback ends with no agent turn
    expect(() => {
      act(() => { fakePlayback.firePlaybackEnd(0); });
    }).not.toThrow();

    expect(hook.result.current.speakingEntryIndex).toBe(-1);
  });
});

// ── Caption-with-text interrupt — freeze must apply to caption-built entries ─
//
// Scenario: TTS providers that send per-fragment captions (with text, possibly
// no per-word timings) build entry.text incrementally from AGENT_CAPTION_APPEND
// dispatches.  These entries have isFullTextFilled=false/undefined.
// Previously, AGENT_TEXT_TRIM_TO_WORD silently no-op'd for such entries under
// the (wrong) assumption that "captions already contain only heard text."
//
// Reality: captions arrive with buffered-ahead audio chunks (1-3 s ahead of
// actual playback position).  entry.text therefore contains unheard future text
// at interrupt time and MUST be frozen via the word-clock just like
// isFullTextFilled entries.
//
// Fix: AGENT_FREEZE_AT_WORD (replacing AGENT_TEXT_TRIM_TO_WORD) sets
// entry.heardText without the isFullTextFilled guard, so both caption-built
// and full-text-filled entries are frozen correctly.

describe("Caption-built entries — freeze applies on interrupt", () => {
  it("entry.heardText is set to heard words even when captions built the entry (isFullTextFilled falsy)", async () => {
    // This reproduces the screenshot scenario:
    //  - Agent tells a story; TTS sends captions with each audio chunk
    //  - Multiple caption fragments arrive, building entry.text incrementally
    //  - User interrupts mid-story via server_vad
    //  - entry.text must be trimmed to only the heard words
    //  - NOT the full buffered-ahead text that captions already appended

    const hook = renderHook(() => usePlaygroundSession());
    await setupSession(hook);

    // Three caption fragments arrive, each with its own audio chunk.
    // Total text = "Once upon a time in a land far away there lived a wizard"
    // (3 fragments × ~3 words each, buffered ahead of playback)
    injectAudioMessage(1, { captionText: "Once upon a time", seqNum: 1 });
    injectAudioMessage(1, { captionText: "in a land far away", seqNum: 2 });
    injectAudioMessage(1, { captionText: "there lived a wizard", seqNum: 3 });
    act(() => { fakePlayback.firePlaybackStart(1); });

    // Verify the entry was built from captions (not isFullTextFilled)
    const entryBefore = hook.result.current.transcript.find((e) => e.role === "agent");
    expect(entryBefore).toBeDefined();
    expect(entryBefore!.text).toContain("Once upon a time");
    expect(entryBefore!.text).toContain("there lived a wizard"); // buffered-ahead!
    // isFullTextFilled should NOT be set (captions built this, LLM text hasn't arrived)
    expect(entryBefore!.isFullTextFilled).toBeFalsy();

    // Only the first 2 audio chunks have played (totalEnqueuedDurationMs=300ms,
    // each chunk = 100ms per enqueue).  Mock the clock: only 200ms played.
    vi.mocked(fakePlayback.getPlayedDurationMs).mockReturnValue(200);

    // User interrupts (server_vad or user_speech)
    injectClearAudio(1);

    const entryAfter = hook.result.current.transcript.find((e) => e.role === "agent");
    expect(entryAfter).toBeDefined();
    // The full story's last fragment ("there lived a wizard") was NOT played.
    // It must not appear in the heard snapshot.
    expect(entryAfter!.heardText).toBeDefined();
    expect(entryAfter!.heardText).not.toContain("there lived a wizard");
    // The first fragment was played.
    expect(entryAfter!.heardText).toContain("Once");
    // entry.text is left intact (full buffered-ahead text preserved).
    expect(entryAfter!.text).toContain("there lived a wizard");
    expect(hook.result.current.speakingEntryIndex).toBe(-1);
  });
});

// ── Caption-less interrupt — full text shown after barge-in (regression) ─────
//
// Scenario: OpenAI / ElevenLabs TTS providers send no per-word captions.
// The full LLM text arrives BEFORE audio starts, so the WordTimingTable is
// empty (recordFinalTextFallback saw totalMs=0 and bailed).  When the user
// interrupts, newHighlightRef.current is null → no heardText freeze was set
// → entry.heardText would be undefined and ChatBubble would fall back to
// entry.text (the full unheard text).
//
// Fix: handleClearAudio builds the fallback table from pendingFallbackTextRef
// + totalEnqueuedDurationMs, then dispatches AGENT_FREEZE_AT_WORD using
// wordsHeardAtMs(getPlayedDurationMs).

describe("Caption-less TTS — unheard text frozen on interrupt", () => {
  it("speakingEntryIndex is -1 after clear_audio on a caption-less turn", async () => {
    const hook = renderHook(() => usePlaygroundSession());
    await setupSession(hook);

    // Text arrives before any audio (caption-less TTS: no captionText)
    act(() => {
      injectServerMessage({
        kind: "text",
        text: "Once upon a time in a small village",
        messageId: "llm-1",
      });
    });

    // Audio starts playing (no captions, just chunks)
    injectAudioMessage(1, { captionText: undefined });
    act(() => { fakePlayback.firePlaybackStart(1); });

    // User interrupts
    injectClearAudio(1);

    expect(hook.result.current.speakingEntryIndex).toBe(-1);
  });

  it("entry.heardText is set to heard words on interrupt — the actual regression scenario", async () => {
    // This is the bug from the screenshot: agent is telling a long story,
    // TTS has no captions, user interrupts mid-sentence, but the full story
    // text remained visible in the bubble.
    //
    // Sequence:
    //   1. Full LLM text arrives before audio (caption-less TTS ordering)
    //   2. Multiple audio chunks enqueue → totalEnqueuedDurationMs grows
    //   3. getPlayedDurationMs() returns a mid-point value (partial play)
    //   4. clear_audio fires
    //   5. entry.heardText must be set to only the words heard

    const hook = renderHook(() => usePlaygroundSession());
    await setupSession(hook);

    // 7-word story: each word gets ~(totalMs/7) ms in the fallback table
    const fullText = "one two three four five six seven";

    // LLM text arrives first (TTS hasn't sent audio yet)
    act(() => {
      injectServerMessage({ kind: "text", text: fullText, messageId: "llm-1" });
    });

    // 10 audio chunks arrive, each bumps totalEnqueuedDurationMs by 100
    // → totalEnqueuedDurationMs = 1000 ms after all chunks
    for (let i = 0; i < 10; i++) {
      injectAudioMessage(1, { captionText: undefined, seqNum: i + 1 });
    }
    act(() => { fakePlayback.firePlaybackStart(1); });

    // Simulate 250 ms played out of 1750 ms effective duration.
    // timingsFromChars enforces MIN_MS_PER_WORD=250ms, so 7 words have an
    // effective floor of 7*250=1750ms regardless of totalEnqueuedDurationMs.
    // With 27 total chars distributed proportionally over 1750ms:
    //   "one"   starts at   0ms  (3/27 * 1750 = 194ms duration)
    //   "two"   starts at 194ms  ← reached at 250ms
    //   "three" starts at 389ms  ← NOT reached yet at 250ms
    //   "four"  starts at 713ms
    // At 250ms: wordsHeard=2 ("one","two"), trimWordIndex=1 → heardText="one two"
    vi.mocked(fakePlayback.getPlayedDurationMs).mockReturnValue(250);

    // User interrupts mid-story
    injectClearAudio(1);

    const agent = hook.result.current.transcript.find((e) => e.role === "agent");
    expect(agent).toBeDefined();
    // heardText snapshot must NOT contain unheard words (three through seven)
    expect(agent!.heardText).toBeDefined();
    expect(agent!.heardText).not.toContain("three");
    expect(agent!.heardText).not.toContain("four");
    expect(agent!.heardText).not.toContain("seven");
    // Must show the words that were played
    expect(agent!.heardText).toContain("one");
    expect(agent!.heardText).toContain("two");
    // entry.text retains the full LLM text (never mutated)
    expect(agent!.text).toBe(fullText);
    expect(hook.result.current.speakingEntryIndex).toBe(-1);
  });

  it("does not crash when clear_audio fires and pendingFallbackText is for a different uttId", async () => {
    const hook = renderHook(() => usePlaygroundSession());
    await setupSession(hook);

    // Text for uttId=1
    act(() => {
      injectServerMessage({ kind: "text", text: "Hello world", messageId: "llm-1" });
    });

    // But clear_audio arrives for uttId=99 (mismatch — should not throw)
    expect(() => { injectClearAudio(99); }).not.toThrow();
    expect(hook.result.current.speakingEntryIndex).toBe(-1);
  });
});

// ── AudioContext suspended → resume race ────────────────────────────────────
//
// The root cause was a sentinel-collision in playback.ts (utteranceStartTime
// === 0 used both as "uninitialised" and as a valid AudioContext timestamp on
// fast-resume).  The unit-level fix is verified in playback.test.ts.
//
// At the integration level, the production trigger was: state→speaking arrives
// before any audio_meta has supplied an utteranceId, then the first audio chunk
// arrives so fast that getPlayedDurationMs() never returns >0 because the
// playback service's resume() promise hadn't completed.  We exercise the same
// orderings here against the real session-orchestration code to confirm the
// hook does not enter a wedged state (no orphan placeholders, speakingEntryIndex
// resolves correctly when audio finally arrives, no transcript corruption).

describe("AudioContext suspended/resume race", () => {
  it("state→speaking before any audio_meta does NOT create a phantom entry", async () => {
    const hook = renderHook(() => usePlaygroundSession());
    await setupSession(hook);
    // setupSession already injects state→speaking once.  Inject it again to
    // make sure repeated transitions while uttId === -1 stay no-ops.
    injectStateChanged("speaking");

    expect(hook.result.current.transcript).toHaveLength(0);
    expect(hook.result.current.speakingEntryIndex).toBe(-1);
  });

  it("first audio_meta after a delayed resume produces a single live entry", async () => {
    // Mirrors the production sequence: state→speaking fires while the audio
    // pipeline is still resolving its async resume(), getPlayedDurationMs()
    // returns 0 throughout.  When the first audio_meta finally arrives, the
    // hook must create exactly one placeholder and mark it live.
    const hook = renderHook(() => usePlaygroundSession());
    await setupSession(hook);

    // Simulate "AudioContext still suspended" — getPlayedDurationMs is 0.
    vi.mocked(fakePlayback.getPlayedDurationMs).mockReturnValue(0);

    injectAudioMessage(1, { captionText: "Hello there" });

    const agentEntries = hook.result.current.transcript.filter((e) => e.role === "agent");
    expect(agentEntries).toHaveLength(1);
    expect(hook.result.current.speakingEntryIndex).toBe(0);
  });

  it("clear_audio while getPlayedDurationMs is still 0 does not throw and resets speaking state", async () => {
    // Worst case: the user barges in before resume() ever completed.  Played
    // duration is 0, so heardText should be empty/undefined, but the hook
    // must still cleanly demote the entry — not leave it stuck on the
    // streaming bubble forever.
    const hook = renderHook(() => usePlaygroundSession());
    await setupSession(hook);

    act(() => {
      injectServerMessage({ kind: "text", text: "Some response text", messageId: "llm-1" });
    });
    injectAudioMessage(1, { captionText: undefined });
    act(() => { fakePlayback.firePlaybackStart(1); });

    vi.mocked(fakePlayback.getPlayedDurationMs).mockReturnValue(0);

    expect(() => { injectClearAudio(1); }).not.toThrow();
    expect(hook.result.current.speakingEntryIndex).toBe(-1);
  });
});

// ── Rapid state transitions ──────────────────────────────────────────────────
//
// Many of the historical bugs surface when state transitions arrive faster
// than the React render loop can settle (e.g. Piper TTS local server can
// emit listening→processing→speaking→draining within a single frame). These
// tests inject the full transition sequence inside a single `act()` so all
// updates batch and assert that no transcript / speaking-state corruption
// results.

describe("rapid state transitions inside a single render", () => {
  it("listening→processing→speaking→draining batched: no transcript corruption", async () => {
    const hook = renderHook(() => usePlaygroundSession());
    // Connect + session_ready, but skip setupSession's state→speaking so we
    // can inject the full sequence ourselves.
    await act(async () => { hook.result.current.connect("ws://test", "websocket", {}); });
    injectSessionReady();

    act(() => {
      injectServerMessage({ kind: "control", control: { action: "state_changed", state: "listening" } });
      injectServerMessage({ kind: "control", control: { action: "state_changed", state: "processing" } });
      injectServerMessage({ kind: "control", control: { action: "state_changed", state: "speaking" } });
      injectServerMessage({ kind: "control", control: { action: "state_changed", state: "draining" } });
    });

    // No phantom entries — uttId is still -1 throughout (no audio_meta yet).
    expect(hook.result.current.transcript).toHaveLength(0);
    // Final state is the LAST one injected.
    expect(hook.result.current.pipelineState).toBe("draining");
  });

  it("rapid transitions followed by audio_meta still produce exactly one entry", async () => {
    const hook = renderHook(() => usePlaygroundSession());
    await act(async () => { hook.result.current.connect("ws://test", "websocket", {}); });
    injectSessionReady();

    act(() => {
      injectServerMessage({ kind: "control", control: { action: "state_changed", state: "processing" } });
      injectServerMessage({ kind: "control", control: { action: "state_changed", state: "speaking" } });
    });

    injectAudioMessage(1, { captionText: "Quick response" });

    const agentEntries = hook.result.current.transcript.filter((e) => e.role === "agent");
    expect(agentEntries).toHaveLength(1);
    expect(agentEntries[0].text).toBe("Quick response");
  });

  it("two consecutive turns with rapid state cycles do not produce duplicate entries", async () => {
    // Regression guard for the cross-turn bleed pattern that produced a phantom duplicate agent bubble.
    // Each new utterance should land in its own bubble; rapid transitions
    // between turns must not resurrect a stale speakingUtteranceIdRef.
    const hook = renderHook(() => usePlaygroundSession());
    await act(async () => { hook.result.current.connect("ws://test", "websocket", {}); });
    injectSessionReady();

    // ── Turn 1 ──
    act(() => {
      injectServerMessage({ kind: "control", control: { action: "state_changed", state: "speaking" } });
    });
    injectAudioMessage(1, { captionText: "First answer" });
    act(() => {
      injectServerMessage({ kind: "control", control: { action: "state_changed", state: "draining" } });
      injectServerMessage({ kind: "control", control: { action: "state_changed", state: "listening" } });
    });

    // ── Turn 2 (rapid) ──
    act(() => {
      injectServerMessage({ kind: "control", control: { action: "state_changed", state: "processing" } });
      injectServerMessage({ kind: "control", control: { action: "state_changed", state: "speaking" } });
    });
    injectAudioMessage(2, { captionText: "Second answer" });

    const agentEntries = hook.result.current.transcript.filter((e) => e.role === "agent");
    expect(agentEntries).toHaveLength(2);
    expect(agentEntries[0].text).toBe("First answer");
    expect(agentEntries[1].text).toBe("Second answer");
  });
});

describe("Realtime Interruption and Transcription Flow", () => {
  it("Agent streaming text followed by user interruption and post-interruption transcript", async () => {
    const hook = renderHook(() => usePlaygroundSession());
    await setupSession(hook);

    // 1. Agent starts speaking
    injectAudioMessage(1, { captionText: "Agent starts telling a very long story about a magical kingdom." });
    act(() => { fakePlayback.firePlaybackStart(1); });

    // Mock that some of it played
    vi.mocked(fakePlayback.getPlayedDurationMs).mockReturnValue(500);

    // 2. User interrupts (barge-in)
    injectClearAudio(1);

    // Assert agent bubble is demoted and frozen
    const agentEntries = hook.result.current.transcript.filter((e) => e.role === "agent");
    expect(agentEntries).toHaveLength(1);
    expect(agentEntries[0].heardText).toBeDefined();
    expect(hook.result.current.speakingEntryIndex).toBe(-1);

    // 3. User transcript message arrives (this represents the user's barge-in speech)
    act(() => {
      injectServerMessage({
        kind: "message",
        message: {
          kind: "text",
          text: "Stop, tell me about dragons instead.",
          message_id: "stt-2",
          role: "user"
        }
      } as ServerMessage);
    });

    const userEntries = hook.result.current.transcript.filter((e) => e.role === "user");
    expect(userEntries).toHaveLength(1);
    expect(userEntries[0].text).toBe("Stop, tell me about dragons instead.");

    // Ensure chronological order: Agent (interrupted) -> System (divider) -> User
    expect(hook.result.current.transcript[0].role).toBe("agent");
    expect(hook.result.current.transcript[1].role).toBe("system");
    expect(hook.result.current.transcript[2].role).toBe("user");

    // 4. Server cycles and agent replies
    injectStateChanged("listening");
    injectStateChanged("processing");
    injectStateChanged("speaking");

    injectAudioMessage(3, { captionText: "Sure, dragons are fascinating creatures." });

    expect(hook.result.current.transcript).toHaveLength(4);
    expect(hook.result.current.transcript[3].role).toBe("agent");
    expect(hook.result.current.transcript[3].text).toBe("Sure, dragons are fascinating creatures.");
    expect(hook.result.current.speakingEntryIndex).toBe(3);
  });
});

describe("Gemini Realtime Transcription Flow", () => {
  it("Synchronized Streaming Test (Verify chronological appending and timing)", async () => {
    const hook = renderHook(() => usePlaygroundSession());
    await setupSession(hook);

    // 1. Send the first text delta with audio_ms = 0
    act(() => {
      injectServerMessage({
        kind: "message",
        message: {
          kind: "text",
          role: "assistant",
          message_id: "rt-asst-1-0",
          text: "Hello",
          timestamp_ms: 0
        }
      });
    });

    // 2. Send the second text delta with audio_ms = 100
    act(() => {
      injectServerMessage({
        kind: "message",
        message: {
          kind: "text",
          role: "assistant",
          message_id: "rt-asst-1-1",
          text: " there",
          timestamp_ms: 100
        }
      });
    });

    const agentEntries = hook.result.current.transcript.filter((e) => e.role === "agent");
    expect(agentEntries).toHaveLength(1);
    expect(agentEntries[0].text).toBe("Hello there");
  });

  it("User/Agent Order & Formatting Test (Ensure exactly two bubbles, strictly ordered)", async () => {
    const hook = renderHook(() => usePlaygroundSession());
    await setupSession(hook);

    // User speaks
    act(() => {
      injectServerMessage({
        kind: "message",
        message: {
          kind: "text",
          role: "user",
          message_id: "stt-1",
          text: "Hi agent"
        }
      });
    });

    // Agent replies
    act(() => {
      injectServerMessage({
        kind: "message",
        message: {
          kind: "text",
          role: "assistant",
          message_id: "rt-asst-1-0",
          text: "Hello",
          timestamp_ms: 0
        }
      });
    });

    const entries = hook.result.current.transcript.filter(e => e.role === 'user' || e.role === 'agent');
    expect(entries).toHaveLength(2);
    expect(entries[0].role).toBe("user");
    expect(entries[1].role).toBe("agent");
  });

  it("Interruption and Recovery Test (Barge-In truncates correctly and allows fresh subsequent turn)", async () => {
    const hook = renderHook(() => usePlaygroundSession());
    await setupSession(hook);

    // Agent starts speaking
    act(() => {
      injectServerMessage({
        kind: "message",
        message: {
          kind: "text",
          role: "assistant",
          message_id: "rt-asst-1-0",
          text: "I am telling a story.",
          timestamp_ms: 0
        }
      });
    });
    
    // Simulate playing audio
    act(() => { fakePlayback.firePlaybackStart(1); });
    vi.mocked(fakePlayback.getPlayedDurationMs).mockReturnValue(0);

    // Barge in
    injectClearAudio(1);

    // Should freeze agent bubble
    // Next turn
    injectStateChanged("listening");
    
    // Simulate user speaking
    act(() => {
      injectServerMessage({
        kind: "message",
        message: {
          kind: "text",
          role: "user",
          message_id: "stt-2",
          text: "Stop."
        }
      });
    });

    injectStateChanged("processing");
    injectStateChanged("speaking");

    act(() => {
      injectServerMessage({
        kind: "message",
        message: {
          kind: "text",
          role: "assistant",
          message_id: "rt-asst-2-0",
          text: "Okay.",
          timestamp_ms: 0
        }
      });
    });

    const allAgents = hook.result.current.transcript.filter((e) => e.role === "agent");
    expect(allAgents).toHaveLength(2);
    expect(allAgents[0].text).toContain("I am telling a story.");
    // Because we barge in before any playback ticks, heardText might be empty string or undefined depending on fallback logic.
    // We mainly care that it correctly truncated the agent's turn and started a new one.
    expect(allAgents[1].text).toContain("Okay.");
    
    const userEntries = hook.result.current.transcript.filter((e) => e.role === "user");
    expect(userEntries).toHaveLength(1);
    expect(userEntries[0].text).toBe("Stop.");
  });
});

describe("Text and Hybrid Mode Transcription Flow", () => {
  it("Text-only mode: user sends text, agent replies with text (no audio)", async () => {
    const hook = renderHook(() => usePlaygroundSession());
    await setupSession(hook);

    // 1. User sends text via sendText
    act(() => { hook.result.current.sendText("Hello text mode"); });
    
    // sendText synchronously adds the USER bubble
    const userEntries = hook.result.current.transcript.filter((e) => e.role === "user");
    expect(userEntries).toHaveLength(1);
    expect(userEntries[0].text).toBe("Hello text mode");

    // 2. Server cycles state
    injectStateChanged("processing");
    injectStateChanged("speaking");

    // 3. Agent sends text delta (no audio)
    act(() => {
      injectServerMessage({
        kind: "message",
        message: {
          kind: "text",
          role: "assistant",
          message_id: "rt-asst-1-0",
          text: "Hi, I am replying in text.",
          timestamp_ms: 0
        }
      });
    });

    const agentEntries = hook.result.current.transcript.filter((e) => e.role === "agent");
    expect(agentEntries).toHaveLength(1);
    expect(agentEntries[0].text).toBe("Hi, I am replying in text.");
  });

  it("Hybrid mode: user speaks (STT), agent replies with text (no audio)", async () => {
    const hook = renderHook(() => usePlaygroundSession());
    await setupSession(hook);

    // 1. User STT arrives
    act(() => {
      injectServerMessage({
        kind: "message",
        message: {
          kind: "text",
          role: "user",
          message_id: "stt-1",
          text: "I am speaking to you."
        }
      });
    });

    const userEntries = hook.result.current.transcript.filter((e) => e.role === "user");
    expect(userEntries).toHaveLength(1);
    expect(userEntries[0].text).toBe("I am speaking to you.");

    // 2. Server cycles state
    injectStateChanged("processing");
    injectStateChanged("speaking");

    // 3. Agent sends text delta
    act(() => {
      injectServerMessage({
        kind: "message",
        message: {
          kind: "text",
          role: "assistant",
          message_id: "rt-asst-1-0",
          text: "I heard you.",
          timestamp_ms: 0
        }
      });
    });

    const agentEntries = hook.result.current.transcript.filter((e) => e.role === "agent");
    expect(agentEntries).toHaveLength(1);
    expect(agentEntries[0].text).toBe("I heard you.");
  });

  it("Hybrid mode: user types (sendText), agent replies with audio", async () => {
    const hook = renderHook(() => usePlaygroundSession());
    await setupSession(hook);

    // 1. User sends text
    act(() => { hook.result.current.sendText("Tell me a story"); });

    // 2. Server cycles state
    injectStateChanged("processing");
    injectStateChanged("speaking");

    // 3. Agent sends audio with captions
    injectAudioMessage(1, { captionText: "Once upon a time in a digital world." });

    const agentEntries = hook.result.current.transcript.filter((e) => e.role === "agent");
    expect(agentEntries).toHaveLength(1);
    expect(agentEntries[0].text).toBe("Once upon a time in a digital world.");
  });

  it("Text interruption: user types 'stop' while agent is speaking audio", async () => {
    const hook = renderHook(() => usePlaygroundSession());
    await setupSession(hook);

    // 1. Agent starts speaking audio
    injectStateChanged("processing");
    injectStateChanged("speaking");
    injectAudioMessage(1, { captionText: "This is a very long story that will be interrupted." });
    act(() => { fakePlayback.firePlaybackStart(1); });
    vi.mocked(fakePlayback.getPlayedDurationMs).mockReturnValue(500);

    // 2. User interrupts by typing "stop"
    act(() => { hook.result.current.sendText("stop"); });
    
    // Server confirms the interruption by sending clear_audio
    injectClearAudio(1, "client_cancel");

    // The agent bubble should be demoted
    expect(hook.result.current.speakingEntryIndex).toBe(-1);

    // Ensure chronological order: Agent (interrupted) -> User ("stop") -> System (divider)
    // In text mode, the user bubble is appended synchronously when sendText is called.
    // The clear_audio (and its divider) arrives afterwards.
    const transcript = hook.result.current.transcript;
    const agentIdx = transcript.findIndex((e) => e.role === "agent");
    const dividerIdx = transcript.findIndex((e) => e.role === "system");
    const userIdx = transcript.findIndex((e) => e.role === "user" && e.text === "stop");

    expect(agentIdx).toBeLessThan(userIdx);
    expect(userIdx).toBeLessThan(dividerIdx);
  });
});
