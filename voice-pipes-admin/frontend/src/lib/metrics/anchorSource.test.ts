import { describe, expect, it } from "vitest";
import { describeAnchorSource, describeClientE2eSource } from "./anchorSource";

// The anchor badge vocabulary shared by the Playground metrics bar and
// the Session History turn header.
describe("describeAnchorSource", () => {
  it("maps the wire vocabulary to short labels and tones", () => {
    expect(describeAnchorSource("input_transcript_proxy")).toMatchObject({ label: "transcript proxy", tone: "proxy" });
    expect(describeAnchorSource("vp_vad")).toMatchObject({ label: "vp vad", tone: "ok" });
    expect(describeAnchorSource("client_speech_end")).toMatchObject({ label: "client vad", tone: "ok" });
    expect(describeAnchorSource("provider_speech_stopped")).toMatchObject({ label: "provider", tone: "ok" });
    expect(describeAnchorSource("text_submit")).toMatchObject({ label: "text", tone: "ok" });
    expect(describeAnchorSource("turn_start")).toMatchObject({ label: "unanchored", tone: "none" });
  });

  it("falls back to the raw key for unknown sources", () => {
    expect(describeAnchorSource("future_source")).toMatchObject({ label: "future source", tone: "ok" });
  });

  it("describes the client E2E anchor including the absent case", () => {
    expect(describeClientE2eSource("client_vad")).toContain("client VAD");
    expect(describeClientE2eSource("listening")).toContain("listening");
    expect(describeClientE2eSource(undefined)).toContain("no client anchor");
  });
});
