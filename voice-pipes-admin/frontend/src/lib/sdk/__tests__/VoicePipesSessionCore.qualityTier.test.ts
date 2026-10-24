import { describe, it, expect, beforeEach, vi } from "vitest";
import { VoicePipesSessionCore, DEFAULT_PREBUFFER_BY_TIER } from "../VoicePipesSessionCore";
import type { QualityTier } from "../../transport/types";
import { DEFAULT_PREBUFFER_MS } from "../../../pages/playground/session/types";

/* eslint-disable @typescript-eslint/no-explicit-any */
function deliverTier(core: VoicePipesSessionCore, tier?: QualityTier, quality = "poor") {
  (core as any).handleQualityUpdateControl({ action: "quality_update", quality, tier });
}
function attachPlayback(core: VoicePipesSessionCore) {
  const setPrebufferMs = vi.fn();
  (core as any).playback = { setPrebufferMs };
  return setPrebufferMs;
}
/* eslint-enable @typescript-eslint/no-explicit-any */

describe("quality tier", () => {
  let core: VoicePipesSessionCore;
  beforeEach(() => {
    core = new VoicePipesSessionCore();
  });

  it("starts at full", () => {
    expect(core.qualityTier).toBe("full");
  });

  // A degraded link is late and uneven, not merely slower — buffer is what
  // absorbs that, so the cushion grows as the tier drops.
  it("deepens the jitter buffer as the tier degrades", () => {
    const setPrebufferMs = attachPlayback(core);

    deliverTier(core, "reduced");
    expect(setPrebufferMs).toHaveBeenLastCalledWith(DEFAULT_PREBUFFER_BY_TIER.reduced);

    deliverTier(core, "critical");
    expect(setPrebufferMs).toHaveBeenLastCalledWith(DEFAULT_PREBUFFER_BY_TIER.critical);

    deliverTier(core, "full");
    expect(setPrebufferMs).toHaveBeenLastCalledWith(DEFAULT_PREBUFFER_BY_TIER.full);
  });

  it("the ladder is monotonic — worse links never get a smaller cushion", () => {
    const order: QualityTier[] = ["full", "reduced", "minimal", "critical"];
    for (let i = 1; i < order.length; i++) {
      expect(DEFAULT_PREBUFFER_BY_TIER[order[i]]).toBeGreaterThan(
        DEFAULT_PREBUFFER_BY_TIER[order[i - 1]],
      );
    }
  });

  it("emits only on transitions", () => {
    attachPlayback(core);
    const seen: QualityTier[] = [];
    core.on("qualityTierChanged", (tier) => seen.push(tier));

    deliverTier(core, "reduced");
    deliverTier(core, "reduced");
    deliverTier(core, "reduced");
    deliverTier(core, "minimal");

    expect(seen).toEqual(["reduced", "minimal"]);
  });

  // Servers predating adaptive quality send `quality` with no `tier`. Guessing
  // a tier from the coarse ping bucket would degrade sessions on a server that
  // never asked for it.
  it("treats a missing tier as full rather than inferring one", () => {
    const setPrebufferMs = attachPlayback(core);
    deliverTier(core, undefined, "poor");
    expect(core.qualityTier).toBe("full");
    expect(setPrebufferMs).not.toHaveBeenCalled();
  });

  it("honours an overridden ladder", () => {
    const setPrebufferMs = attachPlayback(core);
    core.prebufferByTier = { full: 0, reduced: 111, minimal: 222, critical: 333 };
    deliverTier(core, "minimal");
    expect(setPrebufferMs).toHaveBeenLastCalledWith(222);
  });

  it("survives having no playback service", () => {
    expect(() => deliverTier(core, "critical")).not.toThrow();
    expect(core.qualityTier).toBe("critical");
  });

  // A healthy link's cushion is the floor, not a separate number. When these
  // drifted apart, `full` sat at 200 ms — a value that demonstrably underran
  // on a bursting S2S provider (session 5fbf767c).
  it("never gives a healthy link less cushion than the default", () => {
    expect(DEFAULT_PREBUFFER_BY_TIER.full).toBeGreaterThanOrEqual(DEFAULT_PREBUFFER_MS);
  });
});
