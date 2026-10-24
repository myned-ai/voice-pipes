import { describe, it, expect } from "vitest";
import { StreamingResampler } from "./resampler";

/**
 * The capture path feeds this 128-sample worklet blocks at the browser's
 * native rate and puts the result on the wire. A cascade pipeline negotiates
 * 16 kHz, so 48k -> 16k in 128-sample blocks is the case that actually runs;
 * a speech-to-speech pipeline negotiates 48 kHz and never resamples at all,
 * which is why a fault here would look like "only the cascade is broken".
 */
describe("streaming 48k -> 16k in worklet-sized blocks", () => {
  const IN = 48000;
  const OUT = 16000;
  const BLOCK = 128;

  /** A 200 Hz tone at a realistic speech level, in i16. */
  function tone(n: number, startSample: number): Int16Array {
    const a = new Int16Array(n);
    for (let i = 0; i < n; i++) {
      const t = (startSample + i) / IN;
      a[i] = Math.round(Math.sin(2 * Math.PI * 200 * t) * 8000);
    }
    return a;
  }

  function rms(a: Int16Array): number {
    if (a.length === 0) return 0;
    let s = 0;
    for (const v of a) s += v * v;
    return Math.sqrt(s / a.length);
  }

  it("preserves the signal instead of emitting silence", () => {
    const r = new StreamingResampler(IN, OUT);
    const chunks: Int16Array[] = [];
    let produced = 0;
    for (let b = 0; b < 200; b++) {
      chunks.push(r.process(tone(BLOCK, b * BLOCK)));
      produced += chunks[chunks.length - 1].length;
    }
    const all = new Int16Array(produced);
    let o = 0;
    for (const c of chunks) {
      all.set(c, o);
      o += c.length;
    }

    // A 200 Hz tone is far below the 7.2 kHz anti-alias cutoff, so it should
    // come through at close to full amplitude.
    const inputRms = rms(tone(BLOCK * 200, 0));
    const outRms = rms(all);
    expect(outRms).toBeGreaterThan(inputRms * 0.5);
  });

  it("emits roughly one output sample per three input samples", () => {
    const r = new StreamingResampler(IN, OUT);
    let produced = 0;
    for (let b = 0; b < 100; b++) produced += r.process(tone(BLOCK, b * BLOCK)).length;
    const expected = (BLOCK * 100) / 3;
    expect(produced).toBeGreaterThan(expected * 0.95);
    expect(produced).toBeLessThan(expected * 1.05);
  });

  it("does not go silent after the first few blocks", () => {
    // A phase-tracking bug shows up as output that starts fine and decays to
    // nothing, which is indistinguishable from a quiet microphone.
    const r = new StreamingResampler(IN, OUT);
    const perBlock: number[] = [];
    for (let b = 0; b < 300; b++) perBlock.push(rms(r.process(tone(BLOCK, b * BLOCK))));
    const early = perBlock.slice(5, 15).reduce((a, b) => a + b, 0) / 10;
    const late = perBlock.slice(-10).reduce((a, b) => a + b, 0) / 10;
    expect(late).toBeGreaterThan(early * 0.5);
  });
});
