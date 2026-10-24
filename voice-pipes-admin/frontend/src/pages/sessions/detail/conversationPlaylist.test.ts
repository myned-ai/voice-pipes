// Conversation Playback playlist: trimmed user speech, skipped silent turns,
// and a lead-in before each agent reply equal to the real response latency.

import { describe, it, expect } from "vitest";
import { buildPlaylist, LEAD_IN_CAP_MS, TURN_GAP_MS } from "./conversationPlaylist";
import type { Utterance } from "./types";

const API = "http://bff";
const SID = "s1";

function turn(id: number, extra: Partial<Utterance> = {}): Utterance {
  return {
    utterance_id: id,
    audio_files: { mic_raw: `${String(id).padStart(3, "0")}_mic_raw.wav`, tts_frag_000: `${String(id).padStart(3, "0")}_tts_frag_000.wav` },
    ...extra,
  } as Utterance;
}

describe("buildPlaylist", () => {
  it("plays the trimmed user speech, then the reply after the measured latency", () => {
    const list = buildPlaylist(
      [turn(1, { user_speech: { kind: "mic_raw", start_ms: 3000, end_ms: 3900 }, summary: { e2e_perceived_ms: 5395 } })],
      SID,
      API,
    );
    expect(list.map(e => [e.side, e.leadInMs, e.leadInReason])).toEqual([
      ["user", 0, null],
      ["ai", 5395, "response latency"],
    ]);
    expect(list[0].url).toBe(`${API}/api/v1/sessions/${SID}/audio/turn/1/user_speech.wav`);
    expect(list[1].url).toBe(`${API}/api/v1/sessions/${SID}/audio/turn/1/tts_concat.wav`);
  });

  it("skips a user clip the backend found no speech in (typed turn) and uses a plain gap before the reply", () => {
    const list = buildPlaylist(
      [
        turn(1, { user_speech: { kind: "mic_raw", start_ms: 0, end_ms: 500 } }),
        turn(2, { user_speech: null, summary: { e2e_perceived_ms: 1940 } }),
      ],
      SID,
      API,
    );
    expect(list.map(e => [e.turnId, e.side, e.leadInMs, e.leadInReason])).toEqual([
      [1, "user", 0, null],
      [1, "ai", TURN_GAP_MS, "turn gap"],
      [2, "ai", TURN_GAP_MS, "turn gap"],
    ]);
  });

  it("keeps a session id with reserved characters in its own path segment", () => {
    const list = buildPlaylist(
      [turn(1, { user_speech: { kind: "mic_raw", start_ms: 0, end_ms: 500 } })],
      "a?b#c",
      API,
    );
    expect(list[0].url).toBe(`${API}/api/v1/sessions/a%3Fb%23c/audio/turn/1/user_speech.wav`);
  });

  it("caps an outsized latency so playback does not stall", () => {
    const list = buildPlaylist(
      [turn(1, { user_speech: { kind: "mic_raw", start_ms: 0, end_ms: 500 }, summary: { e2e_perceived_ms: 25000 } })],
      SID,
      API,
    );
    expect(list[1].leadInMs).toBe(LEAD_IN_CAP_MS);
  });

  it("falls back to the raw mic clip when the BFF predates user_speech", () => {
    const list = buildPlaylist([turn(1)], SID, API);
    expect(list[0].url).toBe(`${API}/api/v1/sessions/${SID}/audio/001_mic_raw.wav`);
  });

  it("puts a short gap before every user turn after the first", () => {
    const list = buildPlaylist(
      [
        turn(1, { user_speech: { kind: "mic_raw", start_ms: 0, end_ms: 500 } }),
        turn(2, { user_speech: { kind: "mic_raw", start_ms: 0, end_ms: 500 } }),
      ],
      SID,
      API,
    );
    expect(list.filter(e => e.side === "user").map(e => e.leadInMs)).toEqual([0, TURN_GAP_MS]);
  });

  it("a turn with neither mic nor TTS contributes nothing", () => {
    expect(buildPlaylist([{ utterance_id: 9 } as Utterance], SID, API)).toEqual([]);
  });
});
