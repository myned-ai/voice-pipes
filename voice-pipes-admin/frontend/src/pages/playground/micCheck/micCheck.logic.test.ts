import { describe, it, expect } from "vitest";
import {
  summarise, classify, compare, SETTLE_MS,
  type Block, type Take,
} from "./micCheck.logic";

/** Blocks at a fixed cadence, from per-block RMS values. Peak is derived as a
 *  plausible multiple of RMS unless given. */
function blocks(rmsValues: number[], stepMs = 20, peakFactor = 3): Block[] {
  return rmsValues.map((rms, i) => ({
    t: i * stepMs,
    rms,
    peak: Math.min(1, rms * peakFactor),
  }));
}

const silence = (n: number) => new Array(n).fill(0.0002);
const speech = (n: number, level = 0.08) => new Array(n).fill(level);

describe("summarise", () => {
  it("reports nothing for no blocks", () => {
    const s = summarise([]);
    expect(s.blocks).toBe(0);
    expect(s.peak).toBe(0);
    expect(s.activeMs).toBe(0);
  });

  it("measures speech duration, not wall time", () => {
    // 1 s of room tone, 0.6 s of speech, 1 s of room tone.
    const s = summarise(blocks([...silence(50), ...speech(30), ...silence(50)]));
    expect(s.activeMs).toBe(600);
    expect(s.longestRunMs).toBe(600);
    expect(s.spanMs).toBe(2580);
    expect(s.activeRatio).toBeCloseTo(600 / 2580, 2);
  });

  it("tracks the longest unbroken run separately from the total", () => {
    // Two bursts with a gap: 0.4 s and 0.2 s.
    const s = summarise(blocks([...speech(20), ...silence(20), ...speech(10)]));
    expect(s.activeMs).toBe(600);
    expect(s.longestRunMs).toBe(400);
  });
});

describe("classify", () => {
  it("waits before judging a device that has not started", () => {
    expect(classify(summarise([]), 200).kind).toBe("waiting");
  });

  it("calls a graph that never ran separately from a silent track", () => {
    // Nothing at all after the settle window.
    expect(classify(summarise([]), SETTLE_MS + 500).kind).toBe("no-frames");

    // Blocks arriving on schedule, every sample zero. This is the one that
    // looked exactly like a quiet room for an entire debugging session.
    const dead = blocks(new Array(75).fill(0)).map((b) => ({ ...b, peak: 0 }));
    const v = classify(summarise(dead), SETTLE_MS + 500);
    expect(v.kind).toBe("silent-track");
    expect(v.status).toBe("critical");
    expect(v.detail).toMatch(/another tab|application|muted/i);
  });

  it("flags a signal too quiet for a recogniser", () => {
    const faint = blocks(new Array(100).fill(0.002), 20, 2);
    expect(classify(summarise(faint), 3000).kind).toBe("very-quiet");
  });

  it("warns rather than fails when the signal reaches the rail", () => {
    const hot = blocks([...silence(20), ...speech(40, 0.34)], 20, 3);
    const v = classify(summarise(hot), 3000);
    expect(v.kind).toBe("clipping");
    expect(v.status).toBe("warning");
  });

  it("passes an ordinary working microphone", () => {
    const good = blocks([...silence(30), ...speech(60, 0.09), ...silence(30)], 20, 4);
    const v = classify(summarise(good), 3000);
    expect(v.kind).toBe("ok");
    expect(v.status).toBe("good");
  });
});

describe("compare", () => {
  const take = (label: string, processing: boolean, b: Block[]): Take => ({
    label, processing, summary: summarise(b),
  });

  it("asks for both takes before judging", () => {
    const on = take("on", true, blocks(speech(30)));
    const none = take("off", false, []);
    expect(compare(on, none).status).toBe("neutral");
  });

  it("declines to judge when the reference take holds no speech", () => {
    const on = take("on", true, blocks(speech(30)));
    const off = take("off", false, blocks(silence(50)));
    expect(compare(on, off).status).toBe("neutral");
    expect(compare(on, off).title).toMatch(/not enough speech/i);
  });

  it("catches processing that gates speech and amplifies the remainder", () => {
    // The measured shape of the real fault: with processing on, a 1.3 s
    // phrase survives as a 0.55 s burst at roughly 4x the level.
    const on = take("on", true, blocks([
      ...silence(35), ...speech(27, 0.34), ...silence(40),
    ]));
    const off = take("off", false, blocks([
      ...silence(35), ...speech(65, 0.09), ...silence(40),
    ]));

    const c = compare(on, off);
    expect(c.status).toBe("critical");
    expect(c.keptRatio).toBeLessThan(0.7);
    expect(c.title).toMatch(/removing your speech/i);
    // The gain clause only appears when the survivor really is louder.
    expect(c.detail).toMatch(/louder/);
    expect(c.detail).toMatch(/turn the browser's processing off/i);
  });

  it("reports gating without the gain clause when levels match", () => {
    const on = take("on", true, blocks([...silence(20), ...speech(20, 0.09)]));
    const off = take("off", false, blocks([...silence(20), ...speech(65, 0.09)]));
    const c = compare(on, off);
    expect(c.status).toBe("critical");
    expect(c.detail).not.toMatch(/louder/);
  });

  it("clears processing that leaves speech intact", () => {
    const on = take("on", true, blocks([...silence(20), ...speech(60, 0.07)]));
    const off = take("off", false, blocks([...silence(20), ...speech(64, 0.09)]));
    const c = compare(on, off);
    expect(c.status).toBe("good");
    expect(c.keptRatio).toBeGreaterThan(0.7);
    expect(c.detail).toMatch(/downstream of capture/i);
  });
});
