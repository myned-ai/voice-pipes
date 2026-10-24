import { describe, expect, it } from "vitest";
import {
  audioPostComponents,
  chainOverheadMs,
  pacerWaitMs,
  componentById,
  componentByKind,
  componentShortName,
  emitRtf,
  endpointingMs,
  extra,
  extraNumber,
  firstTextMs,
  providerEndpointingMs,
  realtimeTtfbMs,
  sourceRtf,
  ttfbAnchorSource,
  llmGenerationMs,
  llmTokenCount,
  llmTokenEstimate,
  llmTokens,
  sttLatencyMs,
  ttfbMs,
  ttftMs,
  ttsAudioDurationMs,
  ttsChars,
  ttsProvider,
  ttsRtf,
  ttsSynthesisMs,
  ttsTtfbMs,
} from "./lookup";
import { makeComponent, makeSnapshot } from "../../test/fixtures/turnMetrics";

describe("metrics/lookup", () => {
  describe("undefined snapshot guard", () => {
    it("returns undefined for every helper", () => {
      expect(componentByKind(undefined, "stt")).toBeUndefined();
      expect(componentById(undefined, "stt")).toBeUndefined();
      expect(ttftMs(undefined)).toBeUndefined();
      expect(ttfbMs(undefined)).toBeUndefined();
      expect(sttLatencyMs(undefined)).toBeUndefined();
      expect(llmGenerationMs(undefined)).toBeUndefined();
      expect(ttsTtfbMs(undefined)).toBeUndefined();
      expect(ttsSynthesisMs(undefined)).toBeUndefined();
      expect(ttsAudioDurationMs(undefined)).toBeUndefined();
      expect(ttsRtf(undefined)).toBeUndefined();
      expect(ttsChars(undefined)).toBeUndefined();
      expect(ttsProvider(undefined)).toBeUndefined();
      expect(endpointingMs(undefined)).toBeUndefined();
      expect(llmTokens(undefined)).toBeUndefined();
      expect(emitRtf(undefined)).toBeUndefined();
      expect(sourceRtf(undefined)).toBeUndefined();
      expect(chainOverheadMs(undefined)).toBeUndefined();
      expect(audioPostComponents(undefined)).toEqual([]);
    });
  });

  describe("componentByKind / componentById", () => {
    it("finds the first component matching `kind`", () => {
      const s = makeSnapshot();
      expect(componentByKind(s, "stt")?.component_id).toBe("stt");
      expect(componentByKind(s, "llm")?.component_id).toBe("llm");
      expect(componentByKind(s, "tts")?.component_id).toBe("tts");
    });

    it("returns the first match when there are duplicate kinds", () => {
      const s = makeSnapshot({
        components: [
          makeComponent({ component_id: "tts_a", kind: "tts", provider: "piper" }),
          makeComponent({ component_id: "tts_b", kind: "tts", provider: "elevenlabs" }),
        ],
      });
      expect(componentByKind(s, "tts")?.component_id).toBe("tts_a");
    });

    it("matches `component_id` exactly (case-sensitive)", () => {
      const s = makeSnapshot();
      expect(componentById(s, "llm")?.kind).toBe("llm");
      expect(componentById(s, "LLM")).toBeUndefined();
      expect(componentById(s, "missing")).toBeUndefined();
    });
  });

  describe("extra / extraNumber", () => {
    it("returns undefined when extras key is missing", () => {
      const c = makeComponent({ component_id: "x", kind: "x", extras: {} });
      expect(extra(c, "missing")).toBeUndefined();
      expect(extraNumber(c, "missing")).toBeUndefined();
    });

    it("returns undefined for non-finite numbers", () => {
      const c = makeComponent({
        component_id: "x",
        kind: "x",
        extras: { nan: Number.NaN, inf: Number.POSITIVE_INFINITY, ok: 42 },
      });
      expect(extraNumber(c, "nan")).toBeUndefined();
      expect(extraNumber(c, "inf")).toBeUndefined();
      expect(extraNumber(c, "ok")).toBe(42);
    });

    it("returns undefined when value is not a number", () => {
      const c = makeComponent({
        component_id: "x",
        kind: "x",
        extras: { provider: "piper" },
      });
      expect(extraNumber(c, "provider")).toBeUndefined();
    });
  });

  describe("well-known stage helpers", () => {
    it("ttftMs reads llm.extras.ttft_ms", () => {
      const s = makeSnapshot({ llm: { extras: { ttft_ms: 99 } } });
      expect(ttftMs(s)).toBe(99);
    });

    it("ttfbMs reads tts.extras.ttfb_ms", () => {
      const s = makeSnapshot({ tts: { extras: { ttfb_ms: 33 } } });
      expect(ttfbMs(s)).toBe(33);
    });

    it("sttLatencyMs prefers extras.processing_ms over latency_ms", () => {
      const s = makeSnapshot({
        stt: { latency_ms: 999, extras: { processing_ms: 12 } },
      });
      expect(sttLatencyMs(s)).toBe(12);
    });

    it("sttLatencyMs falls back to latency_ms when extras absent", () => {
      const s = makeSnapshot({ stt: { latency_ms: 77, extras: {} } });
      expect(sttLatencyMs(s)).toBe(77);
    });

    it("llmGenerationMs prefers extras.generation_ms over latency_ms", () => {
      const s = makeSnapshot({
        llm: { latency_ms: 999, extras: { generation_ms: 250 } },
      });
      expect(llmGenerationMs(s)).toBe(250);
    });

    it("llmGenerationMs falls back to latency_ms", () => {
      const s = makeSnapshot({ llm: { latency_ms: 88, extras: {} } });
      expect(llmGenerationMs(s)).toBe(88);
    });

    it("llmTokens prefers token_count over token_estimate", () => {
      const s = makeSnapshot({
        llm: { extras: { token_count: 100, token_estimate: 50 } },
      });
      expect(llmTokenCount(s)).toBe(100);
      expect(llmTokenEstimate(s)).toBe(50);
      expect(llmTokens(s)).toBe(100);
    });

    it("llmTokens falls back to token_estimate when token_count absent", () => {
      const s = makeSnapshot({
        llm: { extras: { token_estimate: 25 } },
      });
      expect(llmTokens(s)).toBe(25);
    });

    it("returns undefined for missing well-known component", () => {
      const s = makeSnapshot({
        components: [makeComponent({ component_id: "stt", kind: "stt" })],
      });
      expect(ttftMs(s)).toBeUndefined();
      expect(ttfbMs(s)).toBeUndefined();
      expect(llmGenerationMs(s)).toBeUndefined();
    });

    it("ttsProvider reads from component.provider", () => {
      const s = makeSnapshot({ tts: { provider: "elevenlabs" } });
      expect(ttsProvider(s)).toBe("elevenlabs");
    });
  });

  // ── realtime fused-component helpers ──────────────────────────────────────
  // Realtime S2S sessions emit a single fused component instead of the
  // classic stt/llm/tts triple. The lookup helpers must surface TTFB from
  // the realtime extras (via fallback) so existing aggregations keep
  // working, and the dedicated realtime accessors must return the raw
  // values without leaking into the classic helpers.
  describe("realtime fused component", () => {
    const realtimeSnap = makeSnapshot({
      components: [
        makeComponent({
          component_id: "realtime",
          kind: "realtime",
          extras: { ttfb_ms: 120, audio_output_ms: 1500, input_audio_ms: 800, sample_rate: 24000 },
        }),
      ],
    });

    it("ttfbMs falls back to realtime.extras.ttfb_ms when no tts component exists", async () => {
      expect(ttfbMs(realtimeSnap)).toBe(120);
    });

    it("classic TTS/LLM helpers stay undefined for realtime snapshots", () => {
      expect(ttftMs(realtimeSnap)).toBeUndefined();
      expect(ttsRtf(realtimeSnap)).toBeUndefined();
      expect(ttsSynthesisMs(realtimeSnap)).toBeUndefined();
      expect(ttsAudioDurationMs(realtimeSnap)).toBeUndefined();
    });

    it("dedicated realtime helpers return raw extras", async () => {
      const { realtimeTtfbMs, realtimeAudioOutputMs, realtimeInputAudioMs, realtimeSampleRate, isRealtimeSnapshot } =
        await import("./lookup");
      expect(realtimeTtfbMs(realtimeSnap)).toBe(120);
      expect(realtimeAudioOutputMs(realtimeSnap)).toBe(1500);
      expect(realtimeInputAudioMs(realtimeSnap)).toBe(800);
      expect(realtimeSampleRate(realtimeSnap)).toBe(24000);
      expect(isRealtimeSnapshot(realtimeSnap)).toBe(true);
    });

    it("isRealtimeSnapshot returns false for classic snapshots", async () => {
      const { isRealtimeSnapshot } = await import("./lookup");
      expect(isRealtimeSnapshot(makeSnapshot())).toBe(false);
      expect(isRealtimeSnapshot(undefined)).toBe(false);
    });

    // The new realtime extras.
    it("firstTextMs / ttfbAnchorSource / providerEndpointingMs read the new realtime extras", () => {
      const s = makeSnapshot({
        components: [
          makeComponent({
            component_id: "realtime",
            kind: "realtime",
            extras: {
              ttfb_ms: 1623,
              first_text_ms: 1081,
              ttfb_anchor_source: "input_transcript_proxy",
              provider_endpointing_ms: 666,
              user_speech_end_epoch_ms: 1_700_000_000_000,
              vp_speech_end_epoch_ms: 1_699_999_999_334,
            },
          }),
        ],
      });
      expect(firstTextMs(s)).toBe(1081);
      expect(ttfbAnchorSource(s)).toBe("input_transcript_proxy");
      expect(providerEndpointingMs(s)).toBe(666);
      // Real TTFB unchanged (now honest: anchor → first AUDIO delta).
      expect(realtimeTtfbMs(s)).toBe(1623);
    });

    it("the new realtime helpers are undefined on older snapshots, on non-string sources and on classic snapshots", () => {
      expect(firstTextMs(realtimeSnap)).toBeUndefined();
      expect(ttfbAnchorSource(realtimeSnap)).toBeUndefined();
      expect(providerEndpointingMs(realtimeSnap)).toBeUndefined();
      expect(firstTextMs(makeSnapshot())).toBeUndefined();
      expect(ttfbAnchorSource(makeSnapshot())).toBeUndefined();
      expect(providerEndpointingMs(makeSnapshot())).toBeUndefined();
      expect(firstTextMs(undefined)).toBeUndefined();
      expect(ttfbAnchorSource(undefined)).toBeUndefined();
      expect(providerEndpointingMs(undefined)).toBeUndefined();
      const weird = makeSnapshot({
        components: [makeComponent({ component_id: "realtime", kind: "realtime", extras: { ttfb_anchor_source: 3, provider_endpointing_ms: "666", first_text_ms: Number.NaN } })],
      });
      expect(ttfbAnchorSource(weird)).toBeUndefined();
      expect(providerEndpointingMs(weird)).toBeUndefined();
      expect(firstTextMs(weird)).toBeUndefined();
      const empty = makeSnapshot({
        components: [makeComponent({ component_id: "realtime", kind: "realtime", extras: { ttfb_anchor_source: "" } })],
      });
      expect(ttfbAnchorSource(empty)).toBeUndefined();
    });
  });

  // ── output-throughput helpers ──
  describe("output / audio_post components", () => {
    const throughputSnap = makeSnapshot({
      components: [
        makeComponent({
          component_id: "realtime",
          kind: "realtime",
          extras: { ttfb_ms: 120, audio_output_ms: 36285 },
        }),
        makeComponent({
          component_id: "output",
          kind: "output",
          extras: { emit_rtf: 1.67, source_rtf: 0.53, chain_overhead_ms: 41360, audio_output_ms: 36285 },
        }),
        makeComponent({
          component_id: "audio_post:grpc",
          kind: "audio_post",
          provider: "grpc",
          // Summed call time: 880 calls at ~58 ms.
          latency_ms: 51040,
          extras: { calls: 880, latency_p50_ms: 58, latency_max_ms: 1000, timeouts: 0, failures: 0, budget_ratio: 1.41 },
        }),
        makeComponent({
          component_id: "audio_post:gain",
          kind: "audio_post",
          latency_ms: 880,
          extras: { calls: 880, latency_max_ms: 1, timeouts: 0, failures: 0 },
        }),
      ],
    });

    it("emitRtf / sourceRtf / chainOverheadMs read the `output` component extras", () => {
      expect(emitRtf(throughputSnap)).toBe(1.67);
      expect(sourceRtf(throughputSnap)).toBe(0.53);
      // The processors' call time exceeds emit − source: all of it is theirs.
      expect(chainOverheadMs(throughputSnap)).toBe(41360);
      expect(pacerWaitMs(throughputSnap)).toBe(0);
    });

    it("with no post-processor the emit − source gap is pacer wait, not chain", () => {
      // A provider at 0.23× realtime paced to playback speed.
      const paced = makeSnapshot({
        components: [
          makeComponent({
            component_id: "output",
            kind: "output",
            extras: { emit_rtf: 1.0, source_rtf: 0.23, chain_overhead_ms: 4920, audio_output_ms: 6400 },
          }),
        ],
      });
      expect(chainOverheadMs(paced)).toBe(0);
      expect(pacerWaitMs(paced)).toBe(4920);
    });

    it("splits processor call time from pacer wait, and keeps the whole gap when call time is unknown", () => {
      const output = makeComponent({
        component_id: "output",
        kind: "output",
        extras: { emit_rtf: 1.0, source_rtf: 0.23, chain_overhead_ms: 4920, audio_output_ms: 6400 },
      });
      const split = makeSnapshot({
        components: [output, makeComponent({ component_id: "audio_post:gain", kind: "audio_post", latency_ms: 180 })],
      });
      expect(chainOverheadMs(split)).toBe(180);
      expect(pacerWaitMs(split)).toBe(4740);
      const unknown = makeSnapshot({
        components: [output, makeComponent({ component_id: "audio_post:gain", kind: "audio_post", latency_ms: undefined })],
      });
      expect(chainOverheadMs(unknown)).toBe(4920);
      expect(pacerWaitMs(unknown)).toBeUndefined();
    });

    it("stay undefined for snapshots without an `output` component (old captures)", () => {
      expect(emitRtf(makeSnapshot())).toBeUndefined();
      expect(sourceRtf(makeSnapshot())).toBeUndefined();
      expect(chainOverheadMs(makeSnapshot())).toBeUndefined();
      // A realtime-only snapshot (older worker) has no `output` component either.
      const realtimeOnly = makeSnapshot({
        components: [makeComponent({ component_id: "realtime", kind: "realtime", extras: { ttfb_ms: 120, audio_output_ms: 1500 } })],
      });
      expect(emitRtf(realtimeOnly)).toBeUndefined();
    });

    it("audioPostComponents returns every `audio_post` component in order; short names prefer `provider`", () => {
      const posts = audioPostComponents(throughputSnap);
      expect(posts.map((c) => c.component_id)).toEqual(["audio_post:grpc", "audio_post:gain"]);
      expect(posts.map(componentShortName)).toEqual(["grpc", "gain"]);
      expect(audioPostComponents(makeSnapshot())).toEqual([]);
      expect(componentShortName(makeComponent({ component_id: "annotator:lipsync", kind: "annotator" }))).toBe("lipsync");
      expect(componentShortName(makeComponent({ component_id: "output", kind: "output" }))).toBe("output");
    });
  });
});
