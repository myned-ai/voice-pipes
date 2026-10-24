import { describe, it, expect } from 'vitest';
import { deriveSystemHealth } from './systemStatusStrip.logic';
import type { Slo } from '../lib/slo';

const SLO: Slo = { total_p95_ms: 3000, ttft_p95_ms: 1200, endpointing_p95_ms: 800, error_rate_max_pct: 1.0 };

describe('deriveSystemHealth', () => {
  it('all metrics within budget -> ok / operational', () => {
    const h = deriveSystemHealth({ error_rate_pct: 0.4, p95_latency_ms: 1500, spend_by_provider: { openai: { cap_pct: 20 } } }, SLO);
    expect(h.level).toBe('ok');
    expect(h.headline).toMatch(/operational/i);
  });

  it('error rate over the SLO max -> warn', () => {
    const h = deriveSystemHealth({ error_rate_pct: 2.0, p95_latency_ms: 1500 }, SLO);
    expect(h.level).toBe('warn');
    expect(h.signals.find((s) => s.key === 'errors')?.level).toBe('warn');
  });

  it('error rate >= 5% -> critical', () => {
    const h = deriveSystemHealth({ error_rate_pct: 6.0 }, SLO);
    expect(h.level).toBe('critical');
  });

  it('p95 over SLO -> critical; within 80% -> warn', () => {
    expect(deriveSystemHealth({ p95_latency_ms: 3500 }, SLO).level).toBe('critical');
    expect(deriveSystemHealth({ p95_latency_ms: 2600 }, SLO).level).toBe('warn'); // > 2400 (0.8*3000)
    expect(deriveSystemHealth({ p95_latency_ms: 2000 }, SLO).level).toBe('ok');
  });

  it('budget cap >= 100% -> critical, >= 80% -> warn', () => {
    expect(deriveSystemHealth({ spend_by_provider: { a: { cap_pct: 100 } } }, SLO).level).toBe('critical');
    expect(deriveSystemHealth({ spend_by_provider: { a: { cap_pct: 85 }, b: { cap_pct: 10 } } }, SLO).level).toBe('warn');
  });

  it('overall level is the worst of the signals', () => {
    const h = deriveSystemHealth({ error_rate_pct: 0.2, p95_latency_ms: 3500, spend_by_provider: { a: { cap_pct: 10 } } }, SLO);
    expect(h.level).toBe('critical'); // latency critical dominates the healthy error rate
    expect(h.signals.find((s) => s.key === 'errors')?.level).toBe('ok');
  });

  it('builds one signal per available metric', () => {
    const h = deriveSystemHealth({ error_rate_pct: 0.5, p95_latency_ms: 1500, spend_by_provider: { a: { cap_pct: 30 } } }, SLO);
    expect(h.signals.map((s) => s.key).sort()).toEqual(['budget', 'errors', 'latency']);
  });

  it('adds an SLO error-budget-burn signal when latency breach is supplied', () => {
    // latency 14.8% breach / 5% budget = 2.96× -> critical; error 0.5% / 1% = 0.5×
    const h = deriveSystemHealth({ error_rate_pct: 0.5, p95_latency_ms: 1500 }, SLO, 14.8);
    const burn = h.signals.find((s) => s.key === 'slo_burn')!;
    expect(burn.level).toBe('critical');
    expect(burn.detail).toBe('3.0× budget');
    expect(h.level).toBe('critical'); // burn dominates the otherwise-ok signals
  });

  it('burn under 1× is ok; the worst of error/latency budget wins', () => {
    // error 1.5% / 1% = 1.5× (warn); latency 2% / 5% = 0.4× -> worst 1.5×
    const h = deriveSystemHealth({ error_rate_pct: 1.5 }, SLO, 2);
    const burn = h.signals.find((s) => s.key === 'slo_burn')!;
    expect(burn.level).toBe('warn');
    expect(burn.detail).toBe('1.5× budget');
  });

  it('omits the burn signal when latency breach is not provided (back-compat)', () => {
    const h = deriveSystemHealth({ error_rate_pct: 0.5 }, SLO);
    expect(h.signals.find((s) => s.key === 'slo_burn')).toBeUndefined();
  });
});
