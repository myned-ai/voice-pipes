import { describe, it, expect } from "vitest";
import { PureVoicePipesClientFsm } from "../PureVoicePipesClientFsm";

describe("PureVoicePipesClientFsm", () => {
  it("forces IDLE -> LISTENING transition and START_PLAYBACK_SYSTEM directive on SESSION_READY", () => {
    const fsm = new PureVoicePipesClientFsm();
    expect(fsm.getCurrentState()).toBe("idle");

    const actions = fsm.transition({ type: "SESSION_READY", outputRate: 24000 });
    expect(fsm.getCurrentState()).toBe("listening");
    expect(actions).toEqual([
      { type: "START_PLAYBACK_SYSTEM", sampleRate: 24000 },
    ]);
  });

  it("handles standard processing state cycle cleanly", () => {
    const fsm = new PureVoicePipesClientFsm();
    fsm.transition({ type: "SESSION_READY", outputRate: 48000 });

    const a1 = fsm.transition({ type: "SERVER_STATE_CHANGED", serverState: "processing" });
    expect(fsm.getCurrentState()).toBe("processing");
    expect(a1).toEqual([{ type: "OPEN_BOT_TYPING_BUBBLE" }]);

    const a2 = fsm.transition({ type: "SERVER_STATE_CHANGED", serverState: "speaking" });
    expect(fsm.getCurrentState()).toBe("speaking");
    expect(a2).toEqual([]);

    const a3 = fsm.transition({ type: "SERVER_STATE_CHANGED", serverState: "draining" });
    expect(fsm.getCurrentState()).toBe("draining");
    expect(a3).toEqual([]);
  });

  it("handles fast processing speed race correctly by shifting directly to speaking on AUDIO_META_ARRIVED", () => {
    const fsm = new PureVoicePipesClientFsm();
    fsm.transition({ type: "SESSION_READY", outputRate: 48000 });
    fsm.transition({ type: "SERVER_STATE_CHANGED", serverState: "processing" });

    // Simulate AUDIO_META arriving before StateChanged("speaking")
    const actions = fsm.transition({ type: "AUDIO_META_ARRIVED", utteranceId: 5 });
    expect(fsm.getCurrentState()).toBe("speaking");
    expect(actions).toEqual([
      { type: "ROTATE_BUBBLE_TO_KARAOKE", utteranceId: 5 },
    ]);
  });

  it("triggers freeze and complete actions on barge-in / CLEAR_AUDIO command", () => {
    const fsm = new PureVoicePipesClientFsm();
    fsm.transition({ type: "SESSION_READY", outputRate: 48000 });
    fsm.transition({ type: "SERVER_STATE_CHANGED", serverState: "processing" });
    fsm.transition({ type: "AUDIO_META_ARRIVED", utteranceId: 10 });

    // Barge-in command received for active utterance
    const actions = fsm.transition({ type: "CLEAR_AUDIO", utteranceId: 10 });
    expect(fsm.getCurrentState()).toBe("listening");
    expect(fsm.getActiveUtteranceId()).toBeNull();
    expect(actions).toEqual([
      { type: "FREEZE_BUBBLE_AT_POSITION", utteranceId: 10 },
      { type: "SEND_PLAYBACK_REPORT", utteranceId: 10 },
      { type: "SEND_PLAYBACK_COMPLETE", utteranceId: 10 },
    ]);
  });

  it("runs the flush protocol when CLEAR_AUDIO races ahead of the turn's first audio (processing state)", () => {
    // Server-side barge-in during Processing sends clear_audio before any
    // audio chunk/meta for the turn reaches the client. The flush protocol
    // must still run — skipping it leaves playback tracking anchored to the
    // previous utterance (over-reported PlaybackReports) and lets the flushed
    // turn's late audio play.
    const fsm = new PureVoicePipesClientFsm();
    fsm.transition({ type: "SESSION_READY", outputRate: 48000 });
    fsm.transition({ type: "SERVER_STATE_CHANGED", serverState: "processing" });

    const actions = fsm.transition({ type: "CLEAR_AUDIO", utteranceId: 7 });
    expect(fsm.getCurrentState()).toBe("listening");
    expect(fsm.getActiveUtteranceId()).toBeNull();
    expect(actions).toEqual([
      { type: "FREEZE_BUBBLE_AT_POSITION", utteranceId: 7 },
      { type: "SEND_PLAYBACK_REPORT", utteranceId: 7 },
      { type: "SEND_PLAYBACK_COMPLETE", utteranceId: 7 },
    ]);
  });

  it("still completes the flush protocol for a very late CLEAR_AUDIO in listening", () => {
    const fsm = new PureVoicePipesClientFsm();
    fsm.transition({ type: "SESSION_READY", outputRate: 48000 });
    expect(fsm.getCurrentState()).toBe("listening");

    const actions = fsm.transition({ type: "CLEAR_AUDIO", utteranceId: 3 });
    expect(fsm.getCurrentState()).toBe("listening");
    expect(actions).toEqual([
      { type: "FREEZE_BUBBLE_AT_POSITION", utteranceId: 3 },
      { type: "SEND_PLAYBACK_REPORT", utteranceId: 3 },
      { type: "SEND_PLAYBACK_COMPLETE", utteranceId: 3 },
    ]);
  });

  it("reverts back to IDLE on CONNECTION_LOST and executes STOP_PLAYBACK_SYSTEM", () => {
    const fsm = new PureVoicePipesClientFsm();
    fsm.transition({ type: "SESSION_READY", outputRate: 48000 });
    fsm.transition({ type: "SERVER_STATE_CHANGED", serverState: "processing" });

    const actions = fsm.transition({ type: "CONNECTION_LOST" });
    expect(fsm.getCurrentState()).toBe("idle");
    expect(actions).toEqual([
      { type: "STOP_PLAYBACK_SYSTEM" },
    ]);
  });

  it("self-heals LISTENING -> SPEAKING transition when raw audio arrives unexpectedly", () => {
    const fsm = new PureVoicePipesClientFsm();
    fsm.transition({ type: "SESSION_READY", outputRate: 48000 });
    expect(fsm.getCurrentState()).toBe("listening");

    const actions = fsm.transition({ type: "AUDIO_CHUNK_ARRIVED", utteranceId: 42, isFirst: true });
    expect(fsm.getCurrentState()).toBe("speaking");
    expect(fsm.getActiveUtteranceId()).toBe(42);
    expect(actions).toEqual([
      { type: "ROTATE_BUBBLE_TO_KARAOKE", utteranceId: 42 },
    ]);
  });

  it("returns to listening upon playback end and self-heals back to speaking for consecutive utterances", () => {
    const fsm = new PureVoicePipesClientFsm();
    fsm.transition({ type: "SESSION_READY", outputRate: 48000 });
    fsm.transition({ type: "SERVER_STATE_CHANGED", serverState: "processing" });
    fsm.transition({ type: "AUDIO_META_ARRIVED", utteranceId: 101 }); // Transitions to speaking

    expect(fsm.getCurrentState()).toBe("speaking");
    expect(fsm.getActiveUtteranceId()).toBe(101);

    // Playback ends for utterance 101
    const act1 = fsm.transition({ type: "PLAYBACK_END", utteranceId: 101 });
    expect(fsm.getCurrentState()).toBe("listening");
    expect(fsm.getActiveUtteranceId()).toBeNull();
    expect(act1).toEqual([
      { type: "SEND_PLAYBACK_COMPLETE", utteranceId: 101 }
    ]);

    // Consecutive utterance 102 starts streaming without an intermediate server state changed event
    const act2 = fsm.transition({ type: "AUDIO_CHUNK_ARRIVED", utteranceId: 102, isFirst: true });
    expect(fsm.getCurrentState()).toBe("speaking");
    expect(fsm.getActiveUtteranceId()).toBe(102);
    expect(act2).toEqual([
      { type: "ROTATE_BUBBLE_TO_KARAOKE", utteranceId: 102 }
    ]);
  });
  it("handles AUDIO_CHUNK_ARRIVED while in draining state (pocket-http fast TTS race condition)", () => {
    const fsm = new PureVoicePipesClientFsm();
    fsm.transition({ type: "SESSION_READY", outputRate: 48000 });
    fsm.transition({ type: "SERVER_STATE_CHANGED", serverState: "processing" });
    fsm.transition({ type: "SERVER_STATE_CHANGED", serverState: "speaking" });

    // Server finishes TTS incredibly fast and transitions to draining before paced audio chunks arrive
    fsm.transition({ type: "SERVER_STATE_CHANGED", serverState: "draining" });
    expect(fsm.getCurrentState()).toBe("draining");

    // The first audio chunk arrives AFTER the server has transitioned to draining
    const act1 = fsm.transition({ type: "AUDIO_CHUNK_ARRIVED", utteranceId: 55, isFirst: true });
    
    // FSM remains in draining, but sets active utterance and rotates to karaoke
    expect(fsm.getCurrentState()).toBe("draining");
    expect(fsm.getActiveUtteranceId()).toBe(55);
    expect(act1).toEqual([
      { type: "ROTATE_BUBBLE_TO_KARAOKE", utteranceId: 55 }
    ]);

    // When playback eventually ends, the utteranceId matches and it transitions to listening
    const act2 = fsm.transition({ type: "PLAYBACK_END", utteranceId: 55 });
    expect(fsm.getCurrentState()).toBe("listening");
    expect(fsm.getActiveUtteranceId()).toBeNull();
    expect(act2).toEqual([
      { type: "SEND_PLAYBACK_COMPLETE", utteranceId: 55 }
    ]);
  });

  it("transitions to processing from speaking when server transitions to processing", () => {
    const fsm = new PureVoicePipesClientFsm();
    fsm.transition({ type: "SESSION_READY", outputRate: 48000 });
    fsm.transition({ type: "SERVER_STATE_CHANGED", serverState: "processing" });
    fsm.transition({ type: "AUDIO_META_ARRIVED", utteranceId: 10 });
    expect(fsm.getCurrentState()).toBe("speaking");
    expect(fsm.getActiveUtteranceId()).toBe(10);

    const actions = fsm.transition({ type: "SERVER_STATE_CHANGED", serverState: "processing" });
    expect(fsm.getCurrentState()).toBe("processing");
    expect(fsm.getActiveUtteranceId()).toBeNull();
    expect(actions).toEqual([{ type: "OPEN_BOT_TYPING_BUBBLE" }]);
  });

  it("transitions to processing from draining when server transitions to processing", () => {
    const fsm = new PureVoicePipesClientFsm();
    fsm.transition({ type: "SESSION_READY", outputRate: 48000 });
    fsm.transition({ type: "SERVER_STATE_CHANGED", serverState: "processing" });
    fsm.transition({ type: "AUDIO_META_ARRIVED", utteranceId: 10 });
    fsm.transition({ type: "SERVER_STATE_CHANGED", serverState: "draining" });
    expect(fsm.getCurrentState()).toBe("draining");

    const actions = fsm.transition({ type: "SERVER_STATE_CHANGED", serverState: "processing" });
    expect(fsm.getCurrentState()).toBe("processing");
    expect(fsm.getActiveUtteranceId()).toBeNull();
    expect(actions).toEqual([{ type: "OPEN_BOT_TYPING_BUBBLE" }]);
  });

  it("transitions to speaking from draining when server transitions to speaking", () => {
    const fsm = new PureVoicePipesClientFsm();
    fsm.transition({ type: "SESSION_READY", outputRate: 48000 });
    fsm.transition({ type: "SERVER_STATE_CHANGED", serverState: "processing" });
    fsm.transition({ type: "AUDIO_META_ARRIVED", utteranceId: 10 });
    fsm.transition({ type: "SERVER_STATE_CHANGED", serverState: "draining" });
    expect(fsm.getCurrentState()).toBe("draining");

    const actions = fsm.transition({ type: "SERVER_STATE_CHANGED", serverState: "speaking" });
    expect(fsm.getCurrentState()).toBe("speaking");
    expect(actions).toEqual([]);
  });

  it("handles an agent-initiated turn (no preceding user input) on the pinned happy path", () => {
    // Idle re-engagement prompts and other out-of-turn agent speech open
    // a turn straight from
    // listening. The server emits the standard Processing -> Speaking
    // lifecycle, so the client must ride the pinned path — not the
    // self-heal fallback. Mirrors the agent_initiated_turn.trace
    // golden fixture on the server side.
    const fsm = new PureVoicePipesClientFsm();
    fsm.transition({ type: "SESSION_READY", outputRate: 48000 });
    expect(fsm.getCurrentState()).toBe("listening");

    // No user turn ever happens; the server opens the turn itself.
    const a1 = fsm.transition({ type: "SERVER_STATE_CHANGED", serverState: "processing" });
    expect(fsm.getCurrentState()).toBe("processing");
    expect(a1).toEqual([{ type: "OPEN_BOT_TYPING_BUBBLE" }]);

    // Agent turns allocate text-utterance ids (10_000+ range).
    const a2 = fsm.transition({ type: "AUDIO_META_ARRIVED", utteranceId: 10001 });
    expect(fsm.getCurrentState()).toBe("speaking");
    expect(a2).toEqual([{ type: "ROTATE_BUBBLE_TO_KARAOKE", utteranceId: 10001 }]);

    fsm.transition({ type: "SERVER_STATE_CHANGED", serverState: "draining" });
    expect(fsm.getCurrentState()).toBe("draining");

    const a3 = fsm.transition({ type: "PLAYBACK_END", utteranceId: 10001 });
    expect(fsm.getCurrentState()).toBe("listening");
    expect(fsm.getActiveUtteranceId()).toBeNull();
    expect(a3).toEqual([{ type: "SEND_PLAYBACK_COMPLETE", utteranceId: 10001 }]);
  });
});
