import { describe, it, expect } from "vitest";
import { workerHealthState, pipelineHealthSummary } from "./pipelinesHealth.logic";

type State = "healthy" | "degraded" | "unhealthy" | "not_configured";

const since = { secs_since_epoch: 0, nanos_since_epoch: 0 };

function comp(state: State) {
  return {
    id: `c-${state}`,
    kind: "stt" as const,
    health: { state, reason: "", since },
    telemetry: { processed_total: 0, errors_total: 0, timeouts_total: 0 },
  };
}

/** worker with status + either an aggregate_health or component states. */
function w(
  status: "active" | "draining" | "dead",
  opts: { aggregate?: State; comps?: State[] } = {},
) {
  return {
    worker: { id: "w", status, pipeline_name: "p" },
    rollup: {
      worker_id: "w",
      aggregate_health: opts.aggregate,
      components: (opts.comps ?? []).map(comp),
    },
  };
}

describe("workerHealthState", () => {
  it("treats a dead worker as unhealthy regardless of last-known components", () => {
    expect(workerHealthState(w("dead", { aggregate: "healthy", comps: ["healthy"] }))).toBe("unhealthy");
  });
  it("prefers the server aggregate_health", () => {
    expect(workerHealthState(w("active", { aggregate: "degraded", comps: ["healthy"] }))).toBe("degraded");
  });
  it("falls back to worst-of component states", () => {
    expect(workerHealthState(w("active", { comps: ["healthy", "unhealthy", "degraded"] }))).toBe("unhealthy");
  });
  it("is not_configured with no components and no aggregate", () => {
    expect(workerHealthState(w("active", {}))).toBe("not_configured");
  });
});

describe("pipelineHealthSummary (fraction-healthy, P14)", () => {
  it("all healthy -> healthy", () => {
    const s = pipelineHealthSummary([w("active", { aggregate: "healthy" }), w("active", { aggregate: "healthy" })]);
    expect(s).toMatchObject({ status: "healthy", total: 2, healthy: 2 });
  });

  it("one bad worker of fifty -> degraded (NOT unhealthy), with the count", () => {
    const workers = [
      ...Array.from({ length: 49 }, () => w("active", { aggregate: "healthy" })),
      w("active", { aggregate: "unhealthy" }),
    ];
    const s = pipelineHealthSummary(workers);
    expect(s.status).toBe("degraded");        // the whole pipeline is NOT "down"
    expect(s.healthy).toBe(49);
    expect(s.unhealthy).toBe(1);
    expect(s.total).toBe(50);
  });

  it("every worker down -> unhealthy", () => {
    const s = pipelineHealthSummary([w("dead"), w("active", { aggregate: "unhealthy" })]);
    expect(s.status).toBe("unhealthy");
    expect(s.unhealthy).toBe(2);
  });

  it("mix of healthy + degraded -> degraded", () => {
    const s = pipelineHealthSummary([w("active", { aggregate: "healthy" }), w("active", { aggregate: "degraded" })]);
    expect(s.status).toBe("degraded");
    expect(s).toMatchObject({ healthy: 1, degraded: 1 });
  });

  it("only not_configured workers -> not_configured", () => {
    const s = pipelineHealthSummary([w("active", {}), w("active", {})]);
    expect(s.status).toBe("not_configured");
  });

  it("empty pipeline -> not_configured", () => {
    expect(pipelineHealthSummary([]).status).toBe("not_configured");
  });
});
