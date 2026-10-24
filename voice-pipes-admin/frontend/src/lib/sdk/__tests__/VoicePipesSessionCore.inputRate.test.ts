import { describe, it, expect, vi, beforeEach } from "vitest";

/** Records the rate `startMic` actually asked capture for. */
let requestedRates: number[] = [];

vi.mock("../../audio/capture", () => ({
  AudioCaptureService: class FakeCapture {
    active = false;
    level = 0;
    private rate = 0;
    onSamples: ((samples: Int16Array) => void) | null = null;
    get actualSampleRate() {
      // Real capture resamples, so the emitted rate is always the one asked
      // for — that property is what makes honouring the server safe.
      return this.rate;
    }
    async start(sampleRate: number) {
      this.rate = sampleRate;
      requestedRates.push(sampleRate);
      this.active = true;
    }
    stop() {
      this.active = false;
    }
    setMuted() {}
  },
}));

import { VoicePipesSessionCore } from "../VoicePipesSessionCore";

/** Feed a `session_ready` naming `input_sample_rate` straight into the core. */
function deliverSessionReady(core: VoicePipesSessionCore, inputRate?: number) {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  (core as any).handleSessionReadyControl({
    action: "session_ready",
    session_id: "s1",
    session_token: "t1",
    resumed: false,
    negotiated: {
      audio_format: "pcm_i16_le",
      input_sample_rate: inputRate,
      output_sample_rate: 48000,
      channels: 1,
      protocol_version: 1,
    },
  });
}

describe("startMic input rate", () => {
  beforeEach(() => {
    requestedRates = [];
  });

  // The 512 kbps uplink saving: the server knows the STT wants 16 kHz, the
  // client proposed the browser's 48 kHz, and the server's answer wins.
  it("captures at the rate the server negotiated, not the one requested", async () => {
    const core = new VoicePipesSessionCore();
    deliverSessionReady(core, 16000);

    const rate = await core.startMic(48000);

    expect(requestedRates).toEqual([16000]);
    expect(rate).toBe(16000);
  });

  it("falls back to the requested rate when the server names none", async () => {
    const core = new VoicePipesSessionCore();
    deliverSessionReady(core, undefined);

    const rate = await core.startMic(48000);

    expect(requestedRates).toEqual([48000]);
    expect(rate).toBe(48000);
  });

  it("falls back to the requested rate before any session_ready arrives", async () => {
    const core = new VoicePipesSessionCore();

    await core.startMic(24000);

    expect(requestedRates).toEqual([24000]);
  });

  // The server may raise as well as lower; the client honours either without
  // second-guessing, because the server is the one that knows what the STT
  // needs.
  it("honours a higher negotiated rate too", async () => {
    const core = new VoicePipesSessionCore();
    deliverSessionReady(core, 48000);

    await core.startMic(16000);

    expect(requestedRates).toEqual([48000]);
  });
});
