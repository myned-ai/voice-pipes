import { describe, it, expect } from 'vitest';
import { deriveCostEfficiency } from './business';

describe('deriveCostEfficiency', () => {
  it('divides spend by sessions and turns', () => {
    const r = deriveCostEfficiency({ spendTotal: 60, sessions: 24, turns: 54, windowDays: 7 });
    expect(r.costPerSession).toBeCloseTo(2.5, 6);
    expect(r.costPerTurn).toBeCloseTo(60 / 54, 6);
  });

  it('projects a 30-day run-rate from the window', () => {
    const r = deriveCostEfficiency({ spendTotal: 70, sessions: 10, turns: 10, windowDays: 7 });
    expect(r.projectedMonthlySpend).toBeCloseTo(300, 6); // 70/7 * 30
  });

  it('treats missing spend as zero', () => {
    const r = deriveCostEfficiency({ spendTotal: null, sessions: 5, turns: 5, windowDays: 7 });
    expect(r.spendTotal).toBe(0);
    expect(r.costPerSession).toBe(0);
  });

  it('returns null per-unit costs when the denominator is zero', () => {
    const r = deriveCostEfficiency({ spendTotal: 10, sessions: 0, turns: 0, windowDays: 0 });
    expect(r.costPerSession).toBeNull();
    expect(r.costPerTurn).toBeNull();
    expect(r.projectedMonthlySpend).toBeNull();
  });
});
