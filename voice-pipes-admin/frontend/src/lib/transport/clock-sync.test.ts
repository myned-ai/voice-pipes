import { describe, it, expect } from "vitest";
import { ClockSync } from "./clock-sync";

describe("ClockSync", () => {
  it("hasSamples flips once the first Pong is fed; offset follows the median", () => {
    const cs = new ClockSync(3);
    expect(cs.hasSamples).toBe(false);
    expect(cs.offset).toBe(0);
    // rtt 0 → offset = serverTs − recvWall
    cs.addSample(0, 1_000_250, 0, 1_000_000);
    expect(cs.hasSamples).toBe(true);
    expect(cs.offset).toBe(250);
    cs.addSample(0, 1_000_300, 0, 1_000_000);
    cs.addSample(0, 1_000_200, 0, 1_000_000);
    expect(cs.offset).toBe(250);
    expect(cs.toServerTime(1_000_000)).toBe(1_000_250);
  });
});
