import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";

// The client VAD drives the `timing_report speech_start` / `speech_end`
// frames and anchors the
// client-measured E2E.
//   - exactly one `speech_end` per user turn, wall stamp forwarded
//   - none during agent playback (unless the segment began in the user's turn)
//   - `clientE2eMs` = VAD end → first audio, source `client_vad`; falls back
//     to the `listening` stamp (source `listening`) when the VAD stamped nothing
//   - `speech_start` moves from "first mic frame" to the VAD's start of speech;
//     the mic-open rotation (`pendingListeningRotation` → `resetFirstAudio`)
//     stays on the first mic frame.

const h = vi.hoisted(() => ({ fake: null as unknown as ReturnType<typeof makeFakeTransport> }));

vi.mock("../../transport", async (importActual) => {
  const actual = await importActual<typeof import("../../transport")>();
  return { ...actual, createTransport: () => h.fake };
});

vi.mock("../../audio/capture", () => ({
  AudioCaptureService: class FakeCapture {
    active = false;
    level = 0;
    onSamples: ((samples: Int16Array) => void) | null = null;
    get actualSampleRate() {
      return 48000;
    }
    async start() {
      this.active = true;
    }
    stop() {
      this.active = false;
    }
    setMuted() {}
  },
}));

import { VoicePipesSessionCore, type ClientPlaybackMetrics, type TurnMetricsEnvelope } from "../VoicePipesSessionCore";
import type { AudioPlaybackService, PlaybackChunk } from "../../audio/playback";
import type { PipelineState, TurnMetricsSnapshot } from "../../transport/types";
import type { PlaybackOptions } from "../../../pages/playground/session/types";

function makeFakeTransport() {
  return {
    _state: "disconnected" as "disconnected" | "connecting" | "connected",
    onMessage: null as null | ((msg: unknown) => void),
    onStateChange: null as null | ((state: unknown, info?: unknown) => void),
    onError: null as null | ((err: Error) => void),
    get state() {
      return this._state;
    },
    async connect() {
      this._state = "connected";
      this.onStateChange?.("connected");
    },
    disconnect() {
      this._state = "disconnected";
      this.onStateChange?.("disconnected");
    },
    sendAudio: vi.fn(),
    sendText() {},
    sendControl: vi.fn(),
    sendMessage() {},
    sendTimingReport: vi.fn(),
    sendPlaybackReport: vi.fn(),
    sendPlaybackComplete: vi.fn(),
    sendPlaybackQuality: vi.fn(),
  };
}

const WALL_NOW = 1_700_000_000_000;
const LOUD = 0.1;
const QUIET = 0;
const FRAME_MS = 10;

const BASE_OPTS: PlaybackOptions = { limitScheduledAudio: false, maxScheduledChunks: 2, prebufferMs: 0 };

describe("VoicePipesSessionCore — client VAD timing reports and E2E anchor", () => {
  let core: VoicePipesSessionCore;
  let perfNow: ReturnType<typeof vi.spyOn>;
  let events: { dir: string; type: string; summary: string; detail?: unknown }[];

  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date(WALL_NOW));
    perfNow = vi.spyOn(performance, "now").mockReturnValue(0);
    h.fake = makeFakeTransport();
    core = new VoicePipesSessionCore();
    events = [];
    core.on("eventsEmitted", (dir, type, summary, detail) => events.push({ dir, type, summary, detail }));
  });

  afterEach(() => {
    core.destroy();
    perfNow.mockRestore();
    vi.useRealTimers();
  });

  async function connectWithMic(opts: PlaybackOptions = BASE_OPTS) {
    await core.connect("ws://test:8800/ws", "websocket", undefined, opts);
    await core.startMic();
  }

  /** Feed `durationMs / 10` mic frames at `rms`, starting at `fromMs`. */
  function frames(rms: number, fromMs: number, durationMs: number) {
    const cap = core["capture"] as unknown as { level: number; onSamples: ((s: Int16Array) => void) | null };
    for (let t = fromMs; t < fromMs + durationMs; t += FRAME_MS) {
      perfNow.mockReturnValue(t);
      cap.level = rms;
      cap.onSamples?.(new Int16Array(128));
    }
  }

  /** The server's `listening` transition at `atMs` (re-arms the per-turn
   *  reports and stamps the legacy fallback anchor). */
  function listening(atMs: number) {
    perfNow.mockReturnValue(atMs);
    core["handleStateChangedControl"]({ action: "state_changed", state: "listening" });
    core["_pipelineState"] = "listening";
  }

  function setState(state: PipelineState) {
    core["_pipelineState"] = state;
  }

  function firstAudio(utteranceId: number, atMs: number) {
    perfNow.mockReturnValue(atMs);
    core["handleAudioMessage"]({
      kind: "audio",
      samples: new Int16Array(160),
      meta: { utteranceId },
      seqNum: 0,
      endOfUtterance: false,
    } as never);
  }

  function turnMetrics(utteranceId: number): TurnMetricsEnvelope {
    let env: TurnMetricsEnvelope | null = null;
    const cb = (_id: string, m: TurnMetricsEnvelope) => { env = m; };
    core.on("metricsEmitted", cb);
    const snapshot: TurnMetricsSnapshot = { utterance_id: utteranceId, total_ms: 1500, interrupted: false, components: [] };
    core["handleTurnMetricsControl"]({ action: "turn_metrics", snapshot } as never);
    core.off("metricsEmitted", cb);
    return env!;
  }

  function reports(event: string): number[] {
    return h.fake.sendTimingReport.mock.calls.filter((c) => c[1] === event).map((c) => c[0] as number);
  }

  it("speech → silence: one speech_start and one speech_end, filed under the predicted next utterance id", async () => {
    await connectWithMic();
    listening(100);
    frames(LOUD, 1000, 500); // 1000..1490
    frames(QUIET, 1500, 400); // end fires at 1490 + 300 = 1790
    expect(reports("speech_start")).toEqual([1]);
    expect(reports("speech_end")).toEqual([1]);
    expect(reports("audio_receive")).toEqual([]);

    const start = events.find((e) => e.summary === "speech_start (client vad)")!;
    expect(start).toMatchObject({ dir: "out", type: "control", detail: { utterance_id: 1, client_timestamp_ms: WALL_NOW } });
    const end = events.find((e) => e.summary === "speech_end (client vad)")!;
    expect(end).toMatchObject({ dir: "out", type: "control", detail: { utterance_id: 1, client_timestamp_ms: WALL_NOW } });
    expect(core["vadSpeechEndPerf"]).toBe(1790);
  });

  it("clientE2eMs = VAD speech_end → first audio (source client_vad), carried on the envelope and the metrics log row", async () => {
    await connectWithMic();
    listening(100);
    frames(LOUD, 1000, 500);
    frames(QUIET, 1500, 400); // VAD end at 1790
    firstAudio(1, 2600);
    expect(core["clientE2eMs"]).toBe(810);
    expect(core["clientE2eSource"]).toBe("client_vad");
    const env = turnMetrics(1);
    expect(env.clientE2eMs).toBe(810);
    expect(env.clientE2eSource).toBe("client_vad");
    const row = events.find((e) => e.summary.startsWith("metrics:"))!;
    expect(row.summary).toContain("e2e=810ms (anchor client_vad)");
    // Cleared with the envelope: the next turn cannot inherit the anchor.
    expect(core["clientE2eMs"]).toBeUndefined();
    expect(core["clientE2eSource"]).toBeUndefined();
  });

  it("a pause is not the end: speech that resumes while the server is still listening supersedes the earlier speech_end", async () => {
    // "…to predict the internal bond in, uh, particle boards." The detector
    // takes the "uh" for the end. It used to be the only end reported -- "at
    // most one per turn" -- so the turn's response time was measured from the
    // pause: 7.7 s reported for a reply that took 1.1 s.
    await connectWithMic();
    listening(100);
    frames(LOUD, 1000, 500);
    frames(QUIET, 1500, 400); // a pause, reported as an end at 1790
    frames(LOUD, 2000, 500);  // the speaker carries on; server still listening
    frames(QUIET, 2500, 400); // the real end at 2790
    // Both are on the wire under the same turn; the consumer takes the last.
    expect(reports("speech_end")).toEqual([1, 1]);
    expect(reports("speech_start")).toEqual([1]);
    expect(events.some((e) => e.summary.startsWith("speech resumed before the turn committed"))).toBe(true);
    // And the client's own E2E is anchored on the real end, as the wire is.
    firstAudio(1, 3000);
    expect(core["clientE2eMs"]).toBe(3000 - 2790);
  });

  it("speech that resumes after the server committed the turn does not move its anchor", async () => {
    // Once the server is `processing`, the utterance is closed. Anything said
    // now is a new utterance or a barge-in, and letting it supersede would
    // measure the reply from speech that came after the turn it answers.
    await connectWithMic();
    listening(100);
    frames(LOUD, 1000, 500);
    frames(QUIET, 1500, 400); // the end, at 1790
    setState("processing");
    frames(LOUD, 2000, 500);
    frames(QUIET, 2500, 400); // suppressed
    expect(reports("speech_end")).toEqual([1]);
    firstAudio(1, 3000);
    expect(core["clientE2eMs"]).toBe(3000 - 1790);
  });

  it("no speech_end during agent playback; E2E then falls back to the listening stamp (source listening)", async () => {
    await connectWithMic();
    listening(100);
    setState("speaking");
    frames(LOUD, 5000, 500); // a segment that starts while the agent is talking
    frames(QUIET, 5500, 400); // end at 5790 → suppressed
    expect(reports("speech_end")).toEqual([]);
    // speech_start is still reported (it precedes the barge-in decision).
    expect(reports("speech_start")).toEqual([1]);
    expect(events.some((e) => e.summary === "speech_end (client vad) suppressed: agent speaking")).toBe(true);
    firstAudio(1, 6000);
    expect(core["clientE2eMs"]).toBe(6000 - 100);
    expect(core["clientE2eSource"]).toBe("listening");
    const env = turnMetrics(1);
    expect(env.clientE2eSource).toBe("listening");
  });

  it("a segment that began in the user's turn is still reported when its end lands after a fast `speaking` transition", async () => {
    await connectWithMic();
    listening(100);
    frames(LOUD, 1000, 500); // speech_start at 1150 while listening
    setState("speaking"); // the reply started before the VAD hangover elapsed
    frames(QUIET, 1500, 400); // end at 1790
    expect(reports("speech_end")).toEqual([1]);
  });

  it("a barge-in the server took mid-speech (listening while the VAD is in speech) makes that segment's end report", async () => {
    await connectWithMic();
    listening(100);
    firstAudio(1, 500); // utterance 1 playing
    setState("speaking");
    frames(LOUD, 1000, 300); // 1000..1290 — user barges in
    // The server took the turn: clear_audio → listening while the user still talks.
    listening(1300);
    setState("speaking"); // …and immediately went on to answer (fast provider)
    frames(QUIET, 1300, 400); // end at 1290 + 300 = 1590
    expect(reports("speech_end")).toEqual([2]);
  });

  it("re-arms per turn: the next user turn gets its own speech_end with the next utterance id", async () => {
    await connectWithMic();
    listening(100);
    frames(LOUD, 1000, 500);
    frames(QUIET, 1500, 400);
    firstAudio(1, 2600);
    turnMetrics(1);
    listening(4000);
    frames(LOUD, 5000, 500);
    frames(QUIET, 5500, 400); // end at 5790
    expect(reports("speech_end")).toEqual([1, 2]);
    expect(reports("speech_start")).toEqual([1, 2]);
    firstAudio(2, 7000);
    expect(core["clientE2eMs"]).toBe(7000 - 5790);
    expect(core["clientE2eSource"]).toBe("client_vad");
  });

  it("playbackMetrics carries clientE2eSource next to clientTtfaMs", async () => {
    await connectWithMic();
    const pb = core["createPlaybackService"](16000) as AudioPlaybackService;
    Object.defineProperty(pb, "audioContext", { get: () => ({ currentTime: 10, outputLatency: 0 }) });
    const emitted: ClientPlaybackMetrics[] = [];
    core.on("playbackMetrics", (m) => emitted.push(m));
    listening(100);
    frames(LOUD, 1000, 500);
    frames(QUIET, 1500, 400); // end at 1790
    firstAudio(1, 2600);
    perfNow.mockReturnValue(2700);
    pb.onChunk?.({ samples: new Int16Array(160), meta: { utteranceId: 1 }, startTime: 10.02, endTime: 10.03 } as PlaybackChunk);
    expect(emitted).toHaveLength(1);
    expect(emitted[0]).toMatchObject({ utteranceId: 1, clientReceiveToPlayMs: 120, clientTtfaMs: 930, clientE2eSource: "client_vad" });
    const timings = events.find((e) => e.summary.startsWith("playback timings:"))!;
    expect(timings.summary).toContain("(anchor: client_vad)");
  });

  it("clientVad.enabled = false: no speech reports at all, E2E anchored on listening", async () => {
    await connectWithMic({ ...BASE_OPTS, clientVad: { enabled: false } });
    expect(core["clientVad"]).toBeNull();
    listening(100);
    frames(LOUD, 1000, 500);
    frames(QUIET, 1500, 400);
    expect(h.fake.sendTimingReport).not.toHaveBeenCalled();
    firstAudio(1, 2600);
    expect(core["clientE2eMs"]).toBe(2500);
    expect(core["clientE2eSource"]).toBe("listening");
  });

  it("clientVad options are forwarded to the endpointer", async () => {
    await connectWithMic({ ...BASE_OPTS, clientVad: { hangoverMs: 100, minSpeechMs: 0, enterThreshold: 0.05, exitThreshold: 0.03 } });
    expect(core["clientVad"]?.options).toEqual({ hangoverMs: 100, minSpeechMs: 0, enterThreshold: 0.05, exitThreshold: 0.03 });
    listening(100);
    frames(LOUD, 1000, 200); // start on the first frame (minSpeech 0)
    frames(QUIET, 1200, 300); // end at 1190 + 100 = 1290
    expect(reports("speech_start")).toEqual([1]);
    expect(reports("speech_end")).toEqual([1]);
    expect(core["vadSpeechEndPerf"]).toBe(1290);
  });

  it("the mic-open rotation still runs on the first mic frame, independent of the VAD", async () => {
    await connectWithMic();
    listening(100);
    core["pendingListeningRotation"] = true;
    core["firstAudioArrivalPerf"] = 123;
    frames(QUIET, 200, FRAME_MS); // one silent frame
    expect(core["pendingListeningRotation"]).toBe(false);
    expect(core["firstAudioArrivalPerf"]).toBeNull();
    expect(core["micFrameSeen"]).toBe(true);
    expect(h.fake.sendTimingReport).not.toHaveBeenCalled();
    // Every mic frame is still uplinked.
    expect(h.fake.sendAudio).toHaveBeenCalledTimes(1);
  });

  it("stopMic drops the endpointer; a new session resets the per-turn state", async () => {
    await connectWithMic();
    listening(100);
    frames(LOUD, 1000, 500);
    frames(QUIET, 1500, 400);
    expect(core["speechEndReported"]).toBe(true);
    core.stopMic();
    expect(core["clientVad"]).toBeNull();
    core["resetSessionState"]();
    expect(core["speechEndReported"]).toBe(false);
    expect(core["vadSpeechEndPerf"]).toBe(0);
    // Asserted through behaviour, not a private counter: a new session
    // predicts the first turn again.
    expect(core["predictedNextUtteranceId"]()).toBe(1);
    expect(core["clientE2eSource"]).toBeUndefined();
  });
});
