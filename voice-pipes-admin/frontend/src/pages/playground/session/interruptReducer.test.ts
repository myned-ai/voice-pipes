// Tests for interruptReducer — exhaustive state-machine coverage.
//
// Three initiator paths × every relevant message-ordering permutation.

import { describe, expect, it } from "vitest";
import {
  blockedMessageIdFor,
  initialInterruptState,
  interruptReducer,
  shouldDropAudio,
  shouldDropFinalText,
  type InterruptAction,
  type InterruptState,
} from "./interruptReducer";

const UTT = 7;
const OTHER = 99;

function reduceAll(actions: InterruptAction[]): InterruptState {
  return actions.reduce(interruptReducer, initialInterruptState);
}

describe("blockedMessageIdFor", () => {
  it("formats utterance IDs as llm-N", () => {
    expect(blockedMessageIdFor(0)).toBe("llm-0");
    expect(blockedMessageIdFor(42)).toBe("llm-42");
  });
});

describe("initial state", () => {
  it("has no flush in flight", () => {
    expect(initialInterruptState).toEqual({
      flushedUtteranceId: null,
      blockedFinalMessageId: null,
      pendingHeardMs: null,
      reason: null,
    });
  });

  it("drops nothing", () => {
    expect(shouldDropAudio(initialInterruptState, UTT)).toBe(false);
    expect(shouldDropFinalText(initialInterruptState, "llm-7")).toBe(false);
  });
});

describe("Path A — server-initiated barge-in (SERVER_CLEAR_AUDIO)", () => {
  it("marks the utterance as flushed and blocks its final text", () => {
    const state = reduceAll([
      { type: "SERVER_CLEAR_AUDIO", utteranceId: UTT, reason: "user_speech" },
    ]);
    expect(state.flushedUtteranceId).toBe(UTT);
    expect(state.blockedFinalMessageId).toBe("llm-7");
    expect(state.pendingHeardMs).toBeNull();
    expect(state.reason).toBe("user_speech");
  });

  it("subsequent audio for the utterance is dropped", () => {
    const state = reduceAll([
      { type: "SERVER_CLEAR_AUDIO", utteranceId: UTT, reason: "user_speech" },
    ]);
    expect(shouldDropAudio(state, UTT)).toBe(true);
    expect(shouldDropAudio(state, OTHER)).toBe(false);
  });

  it("the late final text for the utterance is dropped", () => {
    const state = reduceAll([
      { type: "SERVER_CLEAR_AUDIO", utteranceId: UTT, reason: "user_speech" },
    ]);
    expect(shouldDropFinalText(state, "llm-7")).toBe(true);
    expect(shouldDropFinalText(state, "llm-99")).toBe(false);
  });
});

describe("Path B — user types text (USER_INITIATED_FLUSH:user_text)", () => {
  it("captures heardMs and blocks the final text", () => {
    const state = reduceAll([
      {
        type: "USER_INITIATED_FLUSH",
        utteranceId: UTT,
        heardMs: 1234,
        reason: "user_text",
      },
    ]);
    expect(state.flushedUtteranceId).toBe(UTT);
    expect(state.blockedFinalMessageId).toBe("llm-7");
    expect(state.pendingHeardMs).toBe(1234);
    expect(state.reason).toBe("user_text");
  });
});

describe("Path C — explicit cancel button", () => {
  it("flushes with reason client_cancel", () => {
    const state = reduceAll([
      {
        type: "USER_INITIATED_FLUSH",
        utteranceId: UTT,
        heardMs: 800,
        reason: "client_cancel",
      },
    ]);
    expect(state.reason).toBe("client_cancel");
    expect(state.pendingHeardMs).toBe(800);
  });
});

describe("Coalescence — ordering of USER_INITIATED_FLUSH and SERVER_CLEAR_AUDIO", () => {
  it("user-text then server-clear: user-text wins (heardMs preserved)", () => {
    const state = reduceAll([
      {
        type: "USER_INITIATED_FLUSH",
        utteranceId: UTT,
        heardMs: 1500,
        reason: "user_text",
      },
      { type: "SERVER_CLEAR_AUDIO", utteranceId: UTT, reason: "user_speech" },
    ]);
    expect(state.pendingHeardMs).toBe(1500);
    expect(state.reason).toBe("user_text");
    expect(state.flushedUtteranceId).toBe(UTT);
  });

  it("server-clear then user-text: server-clear wins (already flushed)", () => {
    const state = reduceAll([
      { type: "SERVER_CLEAR_AUDIO", utteranceId: UTT, reason: "user_speech" },
      {
        type: "USER_INITIATED_FLUSH",
        utteranceId: UTT,
        heardMs: 1500,
        reason: "user_text",
      },
    ]);
    expect(state.reason).toBe("user_speech");
    expect(state.pendingHeardMs).toBeNull(); // server-clear left it null
  });

  it("duplicate USER_INITIATED_FLUSH is idempotent", () => {
    const state = reduceAll([
      {
        type: "USER_INITIATED_FLUSH",
        utteranceId: UTT,
        heardMs: 1000,
        reason: "user_text",
      },
      {
        type: "USER_INITIATED_FLUSH",
        utteranceId: UTT,
        heardMs: 2000,
        reason: "user_text",
      },
    ]);
    expect(state.pendingHeardMs).toBe(1000); // first capture wins
  });

  it("duplicate SERVER_CLEAR_AUDIO is idempotent", () => {
    const a = reduceAll([
      { type: "SERVER_CLEAR_AUDIO", utteranceId: UTT, reason: "user_speech" },
    ]);
    const b = interruptReducer(a, {
      type: "SERVER_CLEAR_AUDIO",
      utteranceId: UTT,
      reason: "user_speech",
    });
    expect(b).toBe(a); // same reference — no state churn
  });
});

describe("Ordering invariant — TURN_FINALIZED arrival relative to flush", () => {
  it("TURN_FINALIZED for the flushed utterance does NOT clear interrupt state", () => {
    const state = reduceAll([
      {
        type: "USER_INITIATED_FLUSH",
        utteranceId: UTT,
        heardMs: 1000,
        reason: "user_text",
      },
      { type: "TURN_FINALIZED", utteranceId: UTT },
    ]);
    // The interrupt state survives — late audio/text for UTT must still be dropped.
    expect(state.flushedUtteranceId).toBe(UTT);
    expect(state.blockedFinalMessageId).toBe("llm-7");
    expect(shouldDropAudio(state, UTT)).toBe(true);
    expect(shouldDropFinalText(state, "llm-7")).toBe(true);
  });

  it("TURN_FINALIZED for a different utterance is a no-op", () => {
    const before = reduceAll([
      {
        type: "USER_INITIATED_FLUSH",
        utteranceId: UTT,
        heardMs: 1000,
        reason: "user_text",
      },
    ]);
    const after = interruptReducer(before, {
      type: "TURN_FINALIZED",
      utteranceId: OTHER,
    });
    expect(after).toBe(before);
  });

  it("TURN_FINALIZED before any flush is a no-op (state unchanged)", () => {
    const after = interruptReducer(initialInterruptState, {
      type: "TURN_FINALIZED",
      utteranceId: UTT,
    });
    expect(after).toBe(initialInterruptState);
  });

  it("works in either order: flush → finalize, or finalize → flush", () => {
    const order1 = reduceAll([
      {
        type: "USER_INITIATED_FLUSH",
        utteranceId: UTT,
        heardMs: 500,
        reason: "user_text",
      },
      { type: "TURN_FINALIZED", utteranceId: UTT },
    ]);
    const order2 = reduceAll([
      // Note: a finalize for an utterance that was never flushed is a no-op,
      // so this ordering ends up at the same flushed state.
      { type: "TURN_FINALIZED", utteranceId: UTT },
      {
        type: "USER_INITIATED_FLUSH",
        utteranceId: UTT,
        heardMs: 500,
        reason: "user_text",
      },
    ]);
    expect(order1).toEqual(order2);
  });
});

describe("TRANSPORT_DISCONNECTED", () => {
  it("flushes with disconnect reason", () => {
    const state = reduceAll([
      { type: "TRANSPORT_DISCONNECTED", utteranceId: UTT },
    ]);
    expect(state.reason).toBe("disconnect");
    expect(state.flushedUtteranceId).toBe(UTT);
    expect(state.blockedFinalMessageId).toBe("llm-7");
  });

  it("blocks subsequent audio and final text for the dropped utterance", () => {
    const state = reduceAll([
      { type: "TRANSPORT_DISCONNECTED", utteranceId: UTT },
    ]);
    expect(shouldDropAudio(state, UTT)).toBe(true);
    expect(shouldDropFinalText(state, "llm-7")).toBe(true);
  });
});

describe("RESET", () => {
  it("returns the initial state from any prior state", () => {
    const dirty = reduceAll([
      {
        type: "USER_INITIATED_FLUSH",
        utteranceId: UTT,
        heardMs: 1000,
        reason: "user_text",
      },
      { type: "SERVER_CLEAR_AUDIO", utteranceId: UTT, reason: "user_speech" },
    ]);
    const reset = interruptReducer(dirty, { type: "RESET" });
    expect(reset).toEqual(initialInterruptState);
  });
});

describe("CONSUME_PENDING_HEARD_MS", () => {
  it("clears pendingHeardMs to null", () => {
    const before = reduceAll([
      {
        type: "USER_INITIATED_FLUSH",
        utteranceId: UTT,
        heardMs: 1234,
        reason: "user_text",
      },
    ]);
    expect(before.pendingHeardMs).toBe(1234);

    const after = interruptReducer(before, { type: "CONSUME_PENDING_HEARD_MS" });
    expect(after.pendingHeardMs).toBeNull();
  });

  it("preserves flushedUtteranceId and blockedFinalMessageId", () => {
    const before = reduceAll([
      {
        type: "USER_INITIATED_FLUSH",
        utteranceId: UTT,
        heardMs: 500,
        reason: "user_text",
      },
    ]);
    const after = interruptReducer(before, { type: "CONSUME_PENDING_HEARD_MS" });
    expect(after.flushedUtteranceId).toBe(UTT);
    expect(after.blockedFinalMessageId).toBe(blockedMessageIdFor(UTT));
    expect(after.reason).toBe("user_text");
  });

  it("is a no-op when pendingHeardMs is already null", () => {
    const before = reduceAll([
      { type: "SERVER_CLEAR_AUDIO", utteranceId: UTT, reason: "user_speech" },
    ]);
    expect(before.pendingHeardMs).toBeNull();
    const after = interruptReducer(before, { type: "CONSUME_PENDING_HEARD_MS" });
    expect(after).toBe(before); // referential identity — no new object
  });

  it("is idempotent on repeat", () => {
    const before = reduceAll([
      {
        type: "USER_INITIATED_FLUSH",
        utteranceId: UTT,
        heardMs: 100,
        reason: "user_text",
      },
    ]);
    const once = interruptReducer(before, { type: "CONSUME_PENDING_HEARD_MS" });
    const twice = interruptReducer(once, { type: "CONSUME_PENDING_HEARD_MS" });
    expect(twice).toBe(once); // already null → identity
  });
});

describe("Cross-utterance isolation", () => {
  it("flushing utterance A does not block messages for utterance B", () => {
    const state = reduceAll([
      { type: "SERVER_CLEAR_AUDIO", utteranceId: UTT, reason: "user_speech" },
    ]);
    expect(shouldDropAudio(state, OTHER)).toBe(false);
    expect(shouldDropFinalText(state, blockedMessageIdFor(OTHER))).toBe(false);
  });

  it("a fresh flush for a different utterance overwrites the in-flight state", () => {
    // This models the case where the user interrupts utterance A, the app
    // moves on to utterance B, and B itself is then interrupted.  The reducer
    // is not aware of "current utterance" — that's the caller's job.  But a
    // flush on B with a different ID does replace the in-flight state.
    const state = reduceAll([
      {
        type: "USER_INITIATED_FLUSH",
        utteranceId: UTT,
        heardMs: 1000,
        reason: "user_text",
      },
      {
        type: "USER_INITIATED_FLUSH",
        utteranceId: OTHER,
        heardMs: 200,
        reason: "user_text",
      },
    ]);
    expect(state.flushedUtteranceId).toBe(OTHER);
    expect(state.pendingHeardMs).toBe(200);
    expect(state.blockedFinalMessageId).toBe(blockedMessageIdFor(OTHER));
  });
});
