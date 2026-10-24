import { describe, it, expect, beforeEach } from "vitest";
import {
  CONNECT_SOURCE_KEY, describeTarget, effectiveSource, pipelineUnavailableReason,
  readPreferredSource, storePreferredSource,
} from "./connectTarget";

describe("effectiveSource", () => {
  it("defaults to the pipeline when the orchestrator has one", () => {
    expect(effectiveSource("pipeline", "ok", 2)).toBe("pipeline");
  });

  it("holds the preference while the list loads, so the bar does not flip on every page load", () => {
    expect(effectiveSource("pipeline", "loading", 0)).toBe("pipeline");
  });

  it("falls back to a URL when the orchestrator is down or empty", () => {
    expect(effectiveSource("pipeline", "unreachable", 0)).toBe("url");
    expect(effectiveSource("pipeline", "ok", 0)).toBe("url");
  });

  it("never overrides someone who chose a URL", () => {
    expect(effectiveSource("url", "ok", 5)).toBe("url");
  });
});

describe("pipelineUnavailableReason", () => {
  it("tells a dead orchestrator from an empty one", () => {
    expect(pipelineUnavailableReason("unreachable", 0)).toBe("Orchestrator not reachable");
    expect(pipelineUnavailableReason("ok", 0)).toBe("No pipelines registered");
    expect(pipelineUnavailableReason("ok", 1)).toBeNull();
    expect(pipelineUnavailableReason("loading", 0)).toBeNull();
  });
});

describe("preferred source", () => {
  beforeEach(() => localStorage.removeItem(CONNECT_SOURCE_KEY));

  it("is the pipeline until someone says otherwise, and is remembered", () => {
    expect(readPreferredSource()).toBe("pipeline");
    storePreferredSource("url");
    expect(readPreferredSource()).toBe("url");
  });

  it("reads anything unexpected as the default", () => {
    localStorage.setItem(CONNECT_SOURCE_KEY, "carrier-pigeon");
    expect(readPreferredSource()).toBe("pipeline");
  });
});

describe("describeTarget", () => {
  it("names the pipeline and the tail of a long worker id", () => {
    expect(describeTarget({ source: "pipeline", pipelineName: "english", workerId: "worker-0123456789abcdef", url: "ws://h:1/ws" }))
      .toBe("english › …89abcdef");
  });

  it("keeps a short worker id whole", () => {
    expect(describeTarget({ source: "pipeline", pipelineName: "english", workerId: "w1", url: "ws://h:1/ws" }))
      .toBe("english › w1");
  });

  it("falls back to the host when the orchestrator named no worker", () => {
    expect(describeTarget({ source: "pipeline", pipelineName: null, workerId: null, url: "ws://10.0.0.4:8800/ws" }))
      .toBe("pipeline › 10.0.0.4:8800");
  });

  it("shows the host of a direct URL, or the text itself when it is not a URL", () => {
    expect(describeTarget({ source: "url", pipelineName: null, workerId: null, url: "ws://localhost:8800/ws" })).toBe("localhost:8800");
    expect(describeTarget({ source: "url", pipelineName: null, workerId: null, url: "not a url" })).toBe("not a url");
  });
});
