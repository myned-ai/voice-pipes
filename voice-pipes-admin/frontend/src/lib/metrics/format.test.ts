import { describe, expect, it } from "vitest";
import { formatMetric } from "./format";

describe("metrics/format", () => {
  it("returns '-' for null/undefined/NaN/Infinity", () => {
    expect(formatMetric(undefined, "ms")).toBe("-");
    expect(formatMetric(null, "ms")).toBe("-");
    expect(formatMetric(Number.NaN, "ms")).toBe("-");
    expect(formatMetric(Number.POSITIVE_INFINITY, "ratio")).toBe("-");
  });

  it("ms rounds to nearest integer with unit suffix", () => {
    expect(formatMetric(123.7, "ms")).toBe("124 ms");
    expect(formatMetric(0, "ms")).toBe("0 ms");
  });

  it("percent multiplies by 100", () => {
    expect(formatMetric(0.5, "percent")).toBe("50.0%");
    expect(formatMetric(0.123, "percent", "0.00")).toBe("12.30%");
  });

  it("ratio uses 2 decimals by default", () => {
    expect(formatMetric(0.16, "ratio")).toBe("0.16");
    expect(formatMetric(0.16, "ratio", "0.000")).toBe("0.160");
  });

  it("count rounds to integer", () => {
    expect(formatMetric(42.7, "count")).toBe("43");
  });

  it("tokens_per_sec adds tok/s suffix", () => {
    expect(formatMetric(120.5, "tokens_per_sec")).toBe("120.5 tok/s");
    expect(formatMetric(120, "tokens_per_sec", "0.00")).toBe("120.00 tok/s");
  });

  it("bytes uses SI-ish ladder", () => {
    expect(formatMetric(512, "bytes")).toBe("512 B");
    expect(formatMetric(2048, "bytes")).toBe("2.0 KB");
    expect(formatMetric(5 * 1024 * 1024, "bytes")).toBe("5.0 MB");
  });

  it("raw passes through, honouring decimal format", () => {
    expect(formatMetric("piper", "raw")).toBe("piper");
    expect(formatMetric(3.14159, "raw", "0.00")).toBe("3.14");
  });

  it("db keeps the sign and one decimal", () => {
    expect(formatMetric(-20.8, "db")).toBe("-20.8 dB");
    expect(formatMetric(31, "db")).toBe("31.0 dB");
    expect(formatMetric(-20.84, "db", "0.00")).toBe("-20.84 dB");
  });

  // String extras (e.g. `ttfb_anchor_source`) render as text for every
  // unit — a descriptor/unit mismatch must never produce "NaN".
  it("string values pass through unchanged for every unit", () => {
    for (const unit of ["ms", "percent", "ratio", "count", "tokens_per_sec", "bytes", "raw", "db"] as const) {
      expect(formatMetric("input_transcript_proxy", unit)).toBe("input_transcript_proxy");
    }
  });
});
