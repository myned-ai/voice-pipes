import { describe, it, expect } from "vitest";
import { StreamingResampler, resampleBuffer } from "./resampler";

/** `seconds` of a sine at `freq`, as PCM i16. */
function tone(freq: number, rate: number, seconds: number, amp = 8000): Int16Array {
  const n = Math.round(rate * seconds);
  const out = new Int16Array(n);
  for (let i = 0; i < n; i++) {
    out[i] = Math.round(amp * Math.sin((2 * Math.PI * freq * i) / rate));
  }
  return out;
}

function rms(x: Int16Array, skip = 0): number {
  let sum = 0;
  let count = 0;
  for (let i = skip; i < x.length; i++) {
    sum += x[i] * x[i];
    count++;
  }
  return count === 0 ? 0 : Math.sqrt(sum / count);
}

describe("StreamingResampler", () => {
  it("is a pass-through when the rates match", () => {
    const r = new StreamingResampler(48000, 48000);
    expect(r.isPassthrough).toBe(true);
    const input = tone(1000, 48000, 0.01);
    expect(r.process(input)).toBe(input);
  });

  it("downsamples 48k to 16k at roughly a third the length", () => {
    const input = tone(1000, 48000, 0.5);
    const out = resampleBuffer(input, 48000, 16000);
    // One sample of slack either way as the fractional phase advances.
    expect(out.length).toBeGreaterThan(input.length / 3 - 2);
    expect(out.length).toBeLessThan(input.length / 3 + 2);
  });

  it("preserves a passband tone", () => {
    const input = tone(1000, 48000, 0.5);
    const out = resampleBuffer(input, 48000, 16000);
    // Skip the filter's settling transient.
    const ratio = rms(out, 200) / rms(input, 200);
    expect(ratio).toBeGreaterThan(0.85);
    expect(ratio).toBeLessThan(1.15);
  });

  // The test that matters: 12 kHz is above the 8 kHz Nyquist of a 16 kHz
  // stream. Decimating without a low-pass would fold it to 4 kHz — a loud
  // tone that was never in the source. It must be attenuated instead.
  it("attenuates content above the output Nyquist instead of aliasing it", () => {
    const input = tone(12000, 48000, 0.5);
    const out = resampleBuffer(input, 48000, 16000);
    const ratio = rms(out, 200) / rms(input, 200);
    expect(ratio).toBeLessThan(0.25);
  });

  it("is continuous across block boundaries", () => {
    const input = tone(1000, 48000, 0.2);
    const oneShot = resampleBuffer(input, 48000, 16000);

    // Same input, fed in 128-sample worklet-sized blocks.
    const streaming = new StreamingResampler(48000, 16000);
    const parts: number[] = [];
    for (let i = 0; i < input.length; i += 128) {
      parts.push(...streaming.process(input.subarray(i, i + 128)));
    }
    const blocked = Int16Array.from(parts);

    expect(Math.abs(blocked.length - oneShot.length)).toBeLessThanOrEqual(1);
    // Identical sample-for-sample: state carries, so blocking changes nothing.
    const n = Math.min(blocked.length, oneShot.length);
    for (let i = 0; i < n; i++) {
      expect(Math.abs(blocked[i] - oneShot[i])).toBeLessThanOrEqual(1);
    }
  });

  it("reset clears filter and phase state", () => {
    const r = new StreamingResampler(48000, 16000);
    const input = tone(1000, 48000, 0.05);
    const first = r.process(input);
    r.reset();
    const afterReset = r.process(input);
    expect(Array.from(afterReset)).toEqual(Array.from(first));
  });

  it("handles empty input", () => {
    const r = new StreamingResampler(48000, 16000);
    expect(r.process(new Int16Array(0)).length).toBe(0);
  });

  it("handles a non-integer ratio", () => {
    const input = tone(1000, 44100, 0.5);
    const out = resampleBuffer(input, 44100, 16000);
    const expected = (input.length * 16000) / 44100;
    expect(Math.abs(out.length - expected)).toBeLessThan(2);
  });

  it("rejects invalid rates", () => {
    expect(() => new StreamingResampler(0, 16000)).toThrow();
    expect(() => new StreamingResampler(48000, -1)).toThrow();
  });
});
