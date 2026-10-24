import { describe, it, expect } from "vitest";
import { concatBlocks, encodeWav } from "./wav";

describe("encodeWav", () => {
  it("writes a header the worker's WAV reader accepts", () => {
    const buf = encodeWav(new Int16Array([0, 1000, -1000]), 16000);
    const v = new DataView(buf);
    const tag = (off: number) =>
      String.fromCharCode(...new Uint8Array(buf, off, 4));

    expect(tag(0)).toBe("RIFF");
    expect(tag(8)).toBe("WAVE");
    expect(tag(12)).toBe("fmt ");
    expect(tag(36)).toBe("data");
    expect(v.getUint16(20, true)).toBe(1);      // PCM
    expect(v.getUint16(22, true)).toBe(1);      // mono
    expect(v.getUint32(24, true)).toBe(16000);  // rate
    expect(v.getUint16(34, true)).toBe(16);     // bits
    expect(v.getUint32(40, true)).toBe(6);      // 3 samples x 2 bytes
    expect(buf.byteLength).toBe(44 + 6);
  });

  it("stores samples little-endian and signed", () => {
    const v = new DataView(encodeWav(new Int16Array([1, -1, 32767, -32768]), 16000));
    expect(v.getInt16(44, true)).toBe(1);
    expect(v.getInt16(46, true)).toBe(-1);
    expect(v.getInt16(48, true)).toBe(32767);
    expect(v.getInt16(50, true)).toBe(-32768);
  });

  it("records the rate it was given, which the recogniser relies on", () => {
    // A WAV that claims the wrong rate is resampled by the probe to the wrong
    // speed -- the same class of fault as the aliasing that started all this.
    const v = new DataView(encodeWav(new Int16Array(4), 48000));
    expect(v.getUint32(24, true)).toBe(48000);
    expect(v.getUint32(28, true)).toBe(96000);
  });
});

describe("concatBlocks", () => {
  it("joins capture blocks in order", () => {
    const out = concatBlocks([new Int16Array([1, 2]), new Int16Array([]), new Int16Array([3])]);
    expect(Array.from(out)).toEqual([1, 2, 3]);
  });
});
