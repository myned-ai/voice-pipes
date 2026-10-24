import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";

// On the first scheduled chunk of an utterance the core derives
//   clientReceiveToPlayMs   — first audio byte received → first sample audible
//   clientTtfaMs            — speech_end → audible (= clientE2eMs + receive→play)
//   clientPlaybackLatencyMs — server pacer emit → audible, clock-offset corrected
// and emits `playbackMetrics`; `turn_metrics` for the same utterance carries
// the three fields in its envelope.

const h = vi.hoisted(() => ({ fake: null as unknown as ReturnType<typeof makeFakeTransport> }));

vi.mock("../../transport", async (importActual) => {
  const actual = await importActual<typeof import("../../transport")>();
  return { ...actual, createTransport: () => h.fake };
});

import { VoicePipesSessionCore, type ClientPlaybackMetrics, type TurnMetricsEnvelope } from "../VoicePipesSessionCore";
import type { AudioPlaybackService, PlaybackChunk } from "../../audio/playback";
import type { AudioMeta, TurnMetricsSnapshot } from "../../transport/types";

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
    sendAudio() {},
    sendText() {},
    sendControl() {},
    sendMessage() {},
    sendTimingReport: vi.fn(),
  };
}

const WALL_NOW = 1_700_000_000_000;
const SERVER_TS = WALL_NOW + 100;

describe("VoicePipesSessionCore — client playback-latency metrics", () => {
  let core: VoicePipesSessionCore;
  let pb: AudioPlaybackService;
  let perfNow: ReturnType<typeof vi.spyOn>;
  let emitted: ClientPlaybackMetrics[];
  let events: { type: string; summary: string }[];

  beforeEach(async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date(WALL_NOW));
    perfNow = vi.spyOn(performance, "now").mockReturnValue(0);
    h.fake = makeFakeTransport();
    core = new VoicePipesSessionCore();
    await core.connect("ws://test:8800/ws", "websocket");
    pb = core["createPlaybackService"](16000) as AudioPlaybackService;
    // The getter at playback.ts reads the private ctx; override on the instance
    // (enqueue() still sees no ctx and stays inert, so onChunk is driven by hand).
    Object.defineProperty(pb, "audioContext", { get: () => ({ currentTime: 10, outputLatency: 0.01 }) });
    emitted = [];
    events = [];
    core.on("playbackMetrics", (m) => emitted.push(m));
    core.on("eventsEmitted", (_dir, type, summary) => events.push({ type, summary }));
  });

  afterEach(() => {
    core.destroy();
    perfNow.mockRestore();
    vi.useRealTimers();
  });

  /** One Pong sample with rtt=0 so offset = serverTs − recvWall exactly. */
  function feedPong(offsetMs: number) {
    core["clockSync"].addSample(0, 1_000_000 + offsetMs, 0, 1_000_000);
  }

  function firstAudio(meta: AudioMeta, atPerf: number) {
    perfNow.mockReturnValue(atPerf);
    core["handleAudioMessage"]({
      kind: "audio",
      samples: new Int16Array(160),
      meta,
      seqNum: 0,
      endOfUtterance: false,
    } as never);
  }

  function schedule(meta: AudioMeta, atPerf: number, startTime = 10.02): PlaybackChunk {
    perfNow.mockReturnValue(atPerf);
    const chunk = { samples: new Int16Array(160), meta, startTime, endTime: startTime + 0.01 } as PlaybackChunk;
    pb.onChunk?.(chunk);
    return chunk;
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

  it("happy path: one playbackMetrics per utterance with all three timings", () => {
    feedPong(250);
    core["clientE2eMs"] = 800;
    const meta = { utteranceId: 7, serverTimestampMs: SERVER_TS };
    firstAudio(meta, 1000);
    schedule(meta, 1450);
    expect(emitted).toHaveLength(1);
    // 450 ms wait + 20 ms until startTime + 10 ms outputLatency
    expect(emitted[0]).toEqual({
      utteranceId: 7,
      clientReceiveToPlayMs: 480,
      clientTtfaMs: 1280,
      clientPlaybackLatencyMs: Math.round(WALL_NOW + 30 + 250 - SERVER_TS),
    });
    // Second chunk of the same utterance → no second event.
    schedule(meta, 1500, 10.03);
    expect(emitted).toHaveLength(1);
  });

  it("no Pong yet → clientPlaybackLatencyMs undefined, the other two defined", () => {
    core["clientE2eMs"] = 800;
    const meta = { utteranceId: 7, serverTimestampMs: SERVER_TS };
    firstAudio(meta, 1000);
    schedule(meta, 1450);
    expect(emitted[0].clientReceiveToPlayMs).toBe(480);
    expect(emitted[0].clientTtfaMs).toBe(1280);
    expect(emitted[0].clientPlaybackLatencyMs).toBeUndefined();
  });

  it("no serverTimestampMs on the chunk → clientPlaybackLatencyMs undefined", () => {
    feedPong(250);
    const meta = { utteranceId: 7 };
    firstAudio(meta, 1000);
    schedule(meta, 1450);
    expect(emitted[0].clientReceiveToPlayMs).toBe(480);
    expect(emitted[0].clientPlaybackLatencyMs).toBeUndefined();
  });

  it("agent-initiated turn (no clientE2eMs) → clientTtfaMs undefined, receive→play defined", () => {
    const meta = { utteranceId: 7 };
    firstAudio(meta, 1000);
    schedule(meta, 1450);
    expect(emitted[0].clientReceiveToPlayMs).toBe(480);
    expect(emitted[0].clientTtfaMs).toBeUndefined();
  });

  it("a first-audio reset between arrival and schedule discards the stale arrival stamp", () => {
    const meta7 = { utteranceId: 7 };
    firstAudio(meta7, 1000);
    core["resetFirstAudio"]();
    // The next utterance's chunk is scheduled before its own first audio arrived.
    schedule({ utteranceId: 8 }, 1450);
    expect(emitted).toHaveLength(0);
    // Its own first audio → measured against ITS arrival.
    firstAudio({ utteranceId: 8 }, 2000);
    schedule({ utteranceId: 8 }, 2100);
    expect(emitted).toHaveLength(1);
    expect(emitted[0]).toMatchObject({ utteranceId: 8, clientReceiveToPlayMs: 130 });
  });

  it("envelope merge: playbackMetrics before turn_metrics → envelope carries the fields", () => {
    feedPong(250);
    core["clientE2eMs"] = 800;
    const meta = { utteranceId: 7, serverTimestampMs: SERVER_TS };
    firstAudio(meta, 1000);
    schedule(meta, 1450);
    const env = turnMetrics(7);
    expect(env.clientReceiveToPlayMs).toBe(480);
    expect(env.clientTtfaMs).toBe(1280);
    expect(env.clientPlaybackLatencyMs).toBe(Math.round(WALL_NOW + 30 + 250 - SERVER_TS));
    expect(env.clientE2eMs).toBe(800);
  });

  it("envelope merge: turn_metrics before playbackMetrics → envelope without the fields, event still fires", () => {
    const env = turnMetrics(8);
    expect(env.clientReceiveToPlayMs).toBeUndefined();
    expect(env.clientTtfaMs).toBeUndefined();
    expect(env.clientPlaybackLatencyMs).toBeUndefined();
    const meta = { utteranceId: 8 };
    firstAudio(meta, 1000);
    schedule(meta, 1450);
    expect(emitted).toHaveLength(1);
    expect(emitted[0].utteranceId).toBe(8);
  });

  it("envelope merge: a different utterance's playbackMetrics is not merged", () => {
    const meta = { utteranceId: 7 };
    firstAudio(meta, 1000);
    schedule(meta, 1450);
    const env = turnMetrics(9);
    expect(env.clientReceiveToPlayMs).toBeUndefined();
    expect(env.clientTtfaMs).toBeUndefined();
  });

  it("clamp: a negative delivery latency (stale offset) is dropped, the others unaffected", () => {
    feedPong(-5000);
    core["clientE2eMs"] = 800;
    const meta = { utteranceId: 7, serverTimestampMs: SERVER_TS };
    firstAudio(meta, 1000);
    schedule(meta, 1450);
    expect(emitted[0].clientPlaybackLatencyMs).toBeUndefined();
    expect(emitted[0].clientReceiveToPlayMs).toBe(480);
    expect(emitted[0].clientTtfaMs).toBe(1280);
  });

  it("event log: one `playback timings:` row per utterance and the `metrics:` row carries ttfa/buffer/delivery", () => {
    feedPong(250);
    core["clientE2eMs"] = 800;
    const meta = { utteranceId: 7, serverTimestampMs: SERVER_TS };
    firstAudio(meta, 1000);
    schedule(meta, 1450);
    schedule(meta, 1500, 10.03);
    const timings = events.filter((e) => e.summary.startsWith("playback timings:"));
    expect(timings).toHaveLength(1);
    expect(timings[0].summary).toContain("receive→play=480ms ttfa=1280ms");
    turnMetrics(7);
    const metrics = events.find((e) => e.summary.startsWith("metrics:"))!;
    expect(metrics.summary).toContain("ttfa=1280ms");
    expect(metrics.summary).toContain("buffer=480ms");
    expect(metrics.summary).toMatch(/delivery=\d+ms/);
  });

  // The end-of-turn quality frame yields `clientArrivalRtf`, re-emitted
  // on `playbackMetrics` (earlier timings kept)
  // and merged into the envelope like the other three fields.
  it("envelope merge: clientArrivalRtf from the end-of-turn quality frame joins the three timings", () => {
    feedPong(250);
    core["clientE2eMs"] = 800;
    const meta = { utteranceId: 7, serverTimestampMs: SERVER_TS };
    firstAudio(meta, 1000);
    schedule(meta, 1450);
    // 1200 ms of audio took 2000 ms to arrive → arrival RTF 1.67.
    pb.getPlaybackQuality = (utteranceId: number) => ({
      utteranceId, underrunCount: 0, totalSilenceMs: 0, audioMs: 1200, heardMs: 1200, wallMs: 1200,
      arrivalWallMs: 2000, interrupted: false, annotationLateCount: 0, annotationLateMsMax: 0, gaps: [],
    });
    core["executeFsmAction"]({ type: "SEND_PLAYBACK_COMPLETE", utteranceId: 7 });
    expect(emitted).toHaveLength(2);
    expect(emitted[1]).toEqual({
      utteranceId: 7,
      clientReceiveToPlayMs: 480,
      clientTtfaMs: 1280,
      clientPlaybackLatencyMs: Math.round(WALL_NOW + 30 + 250 - SERVER_TS),
      clientArrivalRtf: 1.67,
    });
    const env = turnMetrics(7);
    expect(env.clientArrivalRtf).toBe(1.67);
    expect(env.clientTtfaMs).toBe(1280);
    expect(env.clientReceiveToPlayMs).toBe(480);
  });

  it("event log: the `metrics:` row carries emit_rtf / source_rtf / chain and one post[<name>] cell per audio_post component", () => {
    const snapshot: TurnMetricsSnapshot = {
      utterance_id: 7,
      total_ms: 60000,
      interrupted: false,
      components: [
        { component_id: "output", kind: "output", started_at_ms: 0, extras: { emit_rtf: 1.67, source_rtf: 0.53, chain_overhead_ms: 41360, audio_output_ms: 36285 } },
        { component_id: "audio_post:grpc", kind: "audio_post", provider: "grpc", started_at_ms: 0, extras: { calls: 880, latency_p50_ms: 58, latency_max_ms: 1000, timeouts: 0, failures: 0, budget_ratio: 1.41 } },
        { component_id: "audio_post:gain", kind: "audio_post", started_at_ms: 0, extras: { calls: 880, latency_max_ms: 1, timeouts: 0, failures: 0 } },
      ],
    };
    core["handleTurnMetricsControl"]({ action: "turn_metrics", snapshot } as never);
    const row = events.find((e) => e.summary.startsWith("metrics:"))!;
    expect(row.summary).toContain("emit_rtf=1.67 source_rtf=0.53 chain=41360ms pacer=-ms");
    expect(row.summary).toContain("post[grpc]=p50 58ms ratio 1.41");
    expect(row.summary).toContain("post[gain]=p50 -ms ratio -");
    // Old worker: no output component → dashes, no post cells.
    turnMetrics(8);
    const old = events.filter((e) => e.summary.startsWith("metrics:"))[1]!;
    expect(old.summary).toContain("emit_rtf=- source_rtf=- chain=-ms pacer=-ms");
    expect(old.summary).not.toContain("post[");
  });

  it("resetSessionState clears the pending playback metrics so a new session cannot merge stale ones", () => {
    const meta = { utteranceId: 7 };
    firstAudio(meta, 1000);
    schedule(meta, 1450);
    core["resetSessionState"]();
    const env = turnMetrics(7);
    expect(env.clientReceiveToPlayMs).toBeUndefined();
  });
});
