// The Live Graph page's worker labels and empty-graph messages.
//
// Contract: a worker is labelled with its whole id (ids are names, and the
// end of the name tells workers apart); "No Workers Available" appears only
// when there are no workers, and a graph the orchestrator could not build
// says why.

import { describe, it, expect } from 'vitest';
import { emptyGraphMessage, workerOptionLabel } from '../labels';

describe('workerOptionLabel', () => {
  it('shows the pipeline and the whole worker id', () => {
    expect(workerOptionLabel({ id: 'worker-greek-parakeet', pipeline_name: 'greek-cascade-parakeet' }))
      .toBe('greek-cascade-parakeet (worker-greek-parakeet)');
  });

  it('falls back to the whole id without a pipeline name', () => {
    expect(workerOptionLabel({ id: 'worker-custom', pipeline_name: null })).toBe('worker-custom');
  });
});

describe('emptyGraphMessage', () => {
  it('says there are no workers only when there are none', () => {
    expect(emptyGraphMessage(null, 0).title).toBe('No Workers Available');
  });

  it('does not claim no workers when workers are registered', () => {
    const m = emptyGraphMessage(null, 2);
    expect(m.title).not.toBe('No Workers Available');
    expect(m.detail).not.toMatch(/no workers/i);
  });

  it("carries the orchestrator's reason", () => {
    const reason = 'worker worker-custom registered a pipeline config this orchestrator cannot read';
    const m = emptyGraphMessage(reason, 2);
    expect(m.title).toBe('Pipeline Unavailable');
    expect(m.detail).toContain(reason);
  });
});
