import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { AudioCaptureService } from "./capture";

/**
 * These cover one thing: how capture gets from the microphone's rate to the
 * rate the session negotiated.
 *
 * The failure they exist for was silent. Asking for an `AudioContext` at the
 * wire rate makes `ctx.sampleRate === sampleRate`, which switches our own
 * resampler off and hands the conversion to `createMediaStreamSource`. That
 * conversion aliases, so speech still sounds like speech and still measures
 * like speech, but a recogniser's mel front-end reads noise and transcripts
 * come back empty. Nothing logs an error, and a 48 kHz speech-to-speech
 * session on the same microphone is unaffected -- so it presents as a broken
 * pipeline rather than broken capture.
 */

/** The device rate a typical microphone gives. */
const DEVICE_RATE = 48000;

let contextOptions: AudioContextOptions[] = [];
let mediaConstraints: MediaStreamConstraints[] = [];

class FakeAudioContext {
  sampleRate: number;
  state = "running";
  destination = {};
  audioWorklet = { addModule: vi.fn().mockResolvedValue(undefined) };

  constructor(options?: AudioContextOptions) {
    contextOptions.push(options ?? {});
    // A real browser honours an explicit rate and otherwise uses the device's.
    this.sampleRate = options?.sampleRate ?? DEVICE_RATE;
  }

  createMediaStreamSource() {
    return { connect: vi.fn(), disconnect: vi.fn() };
  }
  resume() {
    return Promise.resolve();
  }
  close() {
    return Promise.resolve();
  }
}

class FakeAudioWorkletNode {
  port = { onmessage: null, onmessageerror: null, postMessage: vi.fn() };
  connect = vi.fn();
  disconnect = vi.fn();
}

beforeEach(() => {
  contextOptions = [];
  mediaConstraints = [];
  vi.stubGlobal("AudioContext", FakeAudioContext);
  vi.stubGlobal("AudioWorkletNode", FakeAudioWorkletNode);
  vi.stubGlobal("Blob", class {});
  vi.stubGlobal("URL", { createObjectURL: () => "blob:x", revokeObjectURL: vi.fn() });
  vi.stubGlobal("navigator", {
    mediaDevices: {
      getUserMedia: vi.fn().mockImplementation((c: MediaStreamConstraints) => {
        mediaConstraints.push(c);
        return Promise.resolve({
          getTracks: () => [{ stop: vi.fn() }],
          getAudioTracks: () => [{ getSettings: () => ({ sampleRate: DEVICE_RATE }) }],
        });
      }),
    },
  });
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("AudioCaptureService rate handling", () => {
  it("does not pin the AudioContext to the requested wire rate", async () => {
    const capture = new AudioCaptureService();
    await capture.start(16000);

    expect(contextOptions).toHaveLength(1);
    expect(
      contextOptions[0].sampleRate,
      "pinning the context to the wire rate hands the downsample to the browser, which aliases",
    ).toBeUndefined();
  });

  it("resamples with our own converter when the device rate differs", async () => {
    const capture = new AudioCaptureService();
    await capture.start(16000);

    expect(capture.contextSampleRate).toBe(DEVICE_RATE);
    expect(capture.isResampling, "16 kHz from a 48 kHz device must be converted here").toBe(true);
    // Whatever route it took, the wire rate is the one the handshake bound.
    expect(capture.actualSampleRate).toBe(16000);
  });

  it("does not constrain the device's rate either", async () => {
    // Constraining the track as well as the context leaves the two free to
    // disagree -- a 16 kHz track feeding a 48 kHz graph -- which Chrome
    // resolves by resampling and sometimes by delivering no audio at all.
    // One conversion, in one place.
    const capture = new AudioCaptureService();
    await capture.start(16000);

    const audio = mediaConstraints[0]?.audio as MediaTrackConstraints;
    expect(audio.sampleRate).toBeUndefined();
    expect(audio.channelCount).toBe(1);
  });

  it("takes the direct route when the device already runs at the wire rate", async () => {
    // A speech-to-speech session negotiating 48 kHz needs no conversion, and
    // adding one would cost quality for nothing.
    const capture = new AudioCaptureService();
    await capture.start(DEVICE_RATE);

    expect(capture.isResampling).toBe(false);
    expect(capture.actualSampleRate).toBe(DEVICE_RATE);
  });
});

describe("AudioCaptureService silent-microphone watchdog", () => {
  it("reports a graph that never delivers a frame", async () => {
    // A mic delivering nothing looks exactly like a quiet room: the level
    // meter sits at zero and the session still says it is listening.
    vi.useFakeTimers();
    try {
      const capture = new AudioCaptureService();
      const silent = vi.fn();
      capture.onSilentMic = silent;
      await capture.start(16000);

      expect(silent).not.toHaveBeenCalled();
      vi.advanceTimersByTime(2000);
      expect(silent).toHaveBeenCalledTimes(1);
    } finally {
      vi.useRealTimers();
    }
  });

  it("reports a track that delivers frames of pure silence", async () => {
    // Distinct from the case above and indistinguishable from outside: the
    // graph runs, blocks arrive on schedule, and every sample in them is
    // zero. That is a muted track or one handed to another consumer, not a
    // capture path that failed to start.
    vi.useFakeTimers();
    try {
      const capture = new AudioCaptureService();
      const silent = vi.fn();
      capture.onSilentMic = silent;
      await capture.start(16000);

      const port = (capture as unknown as { worklet: FakeAudioWorkletNode }).worklet.port;
      const post = port.onmessage as unknown as (e: { data: ArrayBuffer }) => void;
      for (let i = 0; i < 5; i++) post({ data: new Int16Array(128).buffer });

      vi.advanceTimersByTime(2000);
      expect(silent).toHaveBeenCalledTimes(1);
    } finally {
      vi.useRealTimers();
    }
  });

  it("stays quiet once audio is flowing", async () => {
    vi.useFakeTimers();
    try {
      const capture = new AudioCaptureService();
      const silent = vi.fn();
      capture.onSilentMic = silent;
      await capture.start(16000);

      // One worklet block, as the real processor posts it.
      const block = new Int16Array(128);
      block[0] = 1000;
      const port = (capture as unknown as { worklet: FakeAudioWorkletNode }).worklet.port;
      (port.onmessage as unknown as (e: { data: ArrayBuffer }) => void)({ data: block.buffer });

      vi.advanceTimersByTime(2000);
      expect(silent).not.toHaveBeenCalled();
    } finally {
      vi.useRealTimers();
    }
  });
});
