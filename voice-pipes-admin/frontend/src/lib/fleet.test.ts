import { describe, it, expect } from 'vitest';
import { groupFleetByPipeline, fleetWorkerHealth, fleetCapacity, type FleetWorker } from './fleet';

const w = (id: string, pipeline: string, status: FleetWorker['status'], aggregate_health?: FleetWorker['aggregate_health']): FleetWorker =>
  ({ id, pipeline_name: pipeline, status, aggregate_health });

describe('fleetWorkerHealth', () => {
  it('dead worker is unhealthy regardless of aggregate', () => {
    expect(fleetWorkerHealth(w('a', 'p', 'dead', 'healthy'))).toBe('unhealthy');
  });
  it('active worker defaults to healthy when no aggregate reported', () => {
    expect(fleetWorkerHealth(w('a', 'p', 'active'))).toBe('healthy');
  });
  it('uses the reported aggregate when present', () => {
    expect(fleetWorkerHealth(w('a', 'p', 'active', 'degraded'))).toBe('degraded');
  });
});

describe('groupFleetByPipeline', () => {
  it('groups workers by pipeline_name and counts health', () => {
    const groups = groupFleetByPipeline([
      w('1', 'alpha', 'active', 'healthy'),
      w('2', 'alpha', 'active', 'degraded'),
      w('3', 'beta', 'active', 'healthy'),
    ]);
    const alpha = groups.find((g) => g.name === 'alpha')!;
    const beta = groups.find((g) => g.name === 'beta')!;
    expect(alpha).toMatchObject({ total: 2, healthy: 1, status: 'degraded' });
    expect(beta).toMatchObject({ total: 1, healthy: 1, status: 'healthy' });
  });

  it('a pipeline with all workers dead is unhealthy', () => {
    const [g] = groupFleetByPipeline([w('1', 'p', 'dead'), w('2', 'p', 'dead')]);
    expect(g).toMatchObject({ status: 'unhealthy', dead: 2, total: 2, healthy: 0 });
  });

  it('one bad worker of many ⇒ degraded, not unhealthy', () => {
    const workers = [
      ...Array.from({ length: 9 }, (_, i) => w(`h${i}`, 'p', 'active', 'healthy')),
      w('bad', 'p', 'active', 'unhealthy'),
    ];
    const [g] = groupFleetByPipeline(workers);
    expect(g.status).toBe('degraded');
    expect(g.healthy).toBe(9);
    expect(g.total).toBe(10);
  });

  it('sorts worst pipelines first', () => {
    const groups = groupFleetByPipeline([
      w('1', 'ok', 'active', 'healthy'),
      w('2', 'down', 'dead'),
      w('3', 'mixed', 'active', 'healthy'),
      w('4', 'mixed', 'active', 'degraded'),
    ]);
    expect(groups.map((g) => g.name)).toEqual(['down', 'mixed', 'ok']);
  });

  it('falls back to "unknown" for workers with no pipeline_name', () => {
    const [g] = groupFleetByPipeline([{ id: 'x', status: 'active' }]);
    expect(g.name).toBe('unknown');
  });
});

describe('fleetCapacity', () => {
  it('sums concurrency and capacity and computes utilization', () => {
    const c = fleetCapacity([
      { status: 'active', capacity: { max_concurrent: 10, active_sessions: 3 } },
      { status: 'active', capacity: { max_concurrent: 10, active_sessions: 7 } },
    ]);
    expect(c).toMatchObject({ activeSessions: 10, maxConcurrent: 20, utilizationPct: 50, activeWorkers: 2, workers: 2 });
  });

  it('excludes dead workers from capacity but counts them', () => {
    const c = fleetCapacity([
      { status: 'active', capacity: { max_concurrent: 10, active_sessions: 5 } },
      { status: 'dead', capacity: { max_concurrent: 10, active_sessions: 0 } },
    ]);
    expect(c.maxConcurrent).toBe(10);
    expect(c.deadWorkers).toBe(1);
    expect(c.utilizationPct).toBe(50);
  });

  it('utilization is null when there is no live capacity', () => {
    expect(fleetCapacity([{ status: 'dead' }]).utilizationPct).toBeNull();
    expect(fleetCapacity([]).utilizationPct).toBeNull();
  });
});
