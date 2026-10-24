import { useEffect, useState, useCallback } from 'react';
import axios from 'axios';
import type { Experiment, ExperimentResults, TestSuite, Variant } from '../../components/benchmark/types';
import { API_BASE } from '../../config/api';

const API = `${API_BASE}/api/v1/benchmark`;

/**
 * Pull the payload out of an axios response body. The admin backend proxies
 * the orchestrator and answers either with the orchestrator's bare body or
 * with the `{ data, error }` envelope (see backend/app/routers/benchmark.py),
 * so accept both: an envelope with a non-null `data` yields that `data`,
 * anything else is returned as-is.
 */
export function unwrap<T>(raw: unknown): T {
  if (raw && typeof raw === 'object' && 'data' in raw) {
    const inner = (raw as { data?: unknown }).data;
    if (inner != null) return inner as T;
  }
  return raw as T;
}

/** Extract a human-readable message from a rejected axios promise. */
function errorMessage(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

// ── useExperiment ───────────────────────────────────────

export function useExperiment(id: string | undefined) {
  const [experiment, setExperiment] = useState<Experiment | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const fetch = useCallback(() => {
    if (!id) return;
    axios.get(`${API}/experiments/${id}`)
      .then(res => { setExperiment(unwrap(res.data)); setError(null); })
      .catch(err => setError(err.message))
      .finally(() => setLoading(false));
  }, [id]);

  useEffect(() => { fetch(); }, [fetch]);

  // Auto-poll while running
  useEffect(() => {
    if (experiment?.status !== 'running') return;
    const interval = setInterval(fetch, 5000);
    return () => clearInterval(interval);
  }, [experiment?.status, fetch]);

  return { experiment, loading, error, refetch: fetch };
}

// ── useExperimentResults ────────────────────────────────

export function useExperimentResults(id: string | undefined) {
  const [results, setResults] = useState<ExperimentResults | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const fetch = useCallback(() => {
    if (!id) return;
    axios.get(`${API}/experiments/${id}/results`)
      .then(res => { setResults(unwrap(res.data)); setError(null); })
      .catch(err => setError(err.message))
      .finally(() => setLoading(false));
  }, [id]);

  useEffect(() => { fetch(); }, [fetch]);

  return { results, loading, error, refetch: fetch };
}

// ── useExperiments ──────────────────────────────────────

export function useExperiments() {
  const [experiments, setExperiments] = useState<Experiment[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const fetch = useCallback(() => {
    axios.get(`${API}/experiments`)
      .then(res => {
        const data = res.data;
        setExperiments(Array.isArray(data) ? data : data?.data ?? []);
        setError(null);
      })
      .catch(err => setError(err.message))
      .finally(() => setLoading(false));
  }, []);

  useEffect(() => { fetch(); }, [fetch]);

  // Auto-poll while any experiment is running
  useEffect(() => {
    const hasRunning = experiments.some(e => e.status === 'running');
    if (!hasRunning) return;
    const interval = setInterval(fetch, 5000);
    return () => clearInterval(interval);
  }, [experiments, fetch]);

  return { experiments, loading, error, refetch: fetch, setExperiments };
}

// ── useSuite ────────────────────────────────────────────

export function useSuite(id: string | undefined) {
  const [suite, setSuite] = useState<TestSuite | null>(null);
  const [error, setError] = useState<string | null>(null);
  // Id whose request most recently settled. `loading` is derived from
  // whether it matches the current id, so the fetch effect never has to
  // flip a loading flag synchronously.
  const [settledId, setSettledId] = useState<string | undefined>(undefined);

  const load = useCallback((target: string) => {
    axios.get(`${API}/suites/${target}`)
      .then(res => { setSuite(unwrap(res.data)); setError(null); })
      .catch(err => setError(errorMessage(err)))
      .finally(() => setSettledId(target));
  }, []);

  useEffect(() => { if (id) load(id); }, [id, load]);

  const refetch = useCallback(() => {
    if (!id) return;
    setSettledId(undefined);
    load(id);
  }, [id, load]);

  const loading = id != null && settledId !== id;

  return { suite, loading, error, refetch };
}

// ── useSuites ───────────────────────────────────────────

export function useSuites() {
  const [suites, setSuites] = useState<TestSuite[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  // Only touches state from the response callbacks, so it is safe to call
  // from the mount effect. `loading` starts true via useState.
  const load = useCallback(() => {
    axios.get(`${API}/suites`)
      .then(res => {
        const data = res.data;
        setSuites(Array.isArray(data) ? data : data?.data ?? []);
        setError(null);
      })
      .catch(err => setError(errorMessage(err)))
      .finally(() => setLoading(false));
  }, []);

  useEffect(() => { load(); }, [load]);

  const refetch = useCallback(() => {
    setLoading(true);
    load();
  }, [load]);

  return { suites, loading, error, refetch, setSuites };
}

// ── Suite mutations (CRUD) ──────────────────────────────

/**
 * Pull a TestSuite out of an axios response, tolerating both the
 * ApiResponse envelope (`{ data, error }`) and a bare TestSuite body.
 * Throws if the server returned an error envelope or a payload with
 * no usable `id`, so callers don't accidentally navigate to
 * `/suites/undefined`.
 */
function unwrapSuite(raw: unknown, op: 'create' | 'update'): TestSuite {
  // Error envelope wins over any partial data.
  if (raw && typeof raw === 'object' && 'error' in raw) {
    const env = raw as { error?: { code?: string; message?: string } | null; data?: unknown };
    if (env.error) {
      const msg = env.error.message ?? env.error.code ?? `unknown ${op} error`;
      throw new Error(msg);
    }
    if (env.data && typeof env.data === 'object' && 'id' in env.data) {
      return env.data as TestSuite;
    }
  }
  // Bare body case (no envelope).
  if (raw && typeof raw === 'object' && 'id' in raw) {
    return raw as TestSuite;
  }
  throw new Error(`server returned no suite payload on ${op}`);
}

/** Create a new suite. Returns the created suite (server may echo it back). */
export async function createSuite(suite: TestSuite): Promise<TestSuite> {
  const res = await axios.post(`${API}/suites`, suite);
  return unwrapSuite(res.data, 'create');
}

/** Replace an existing suite by id (PUT). */
export async function updateSuite(id: string, suite: TestSuite): Promise<TestSuite> {
  const res = await axios.put(`${API}/suites/${encodeURIComponent(id)}`, suite);
  return unwrapSuite(res.data, 'update');
}

/** Delete a suite by id. */
export async function deleteSuite(id: string): Promise<void> {
  await axios.delete(`${API}/suites/${encodeURIComponent(id)}`);
}

/**
 * Duplicate a suite: GET the source, mutate id + created_at + name,
 * then POST as a new suite. Implemented client-side so no new backend
 * endpoint is required.
 */
export async function duplicateSuite(id: string): Promise<TestSuite> {
  const res = await axios.get(`${API}/suites/${encodeURIComponent(id)}`);
  const original = unwrap<TestSuite>(res.data);
  const copy: TestSuite = {
    ...original,
    id: cryptoRandomId(),
    name: `${original.name} (copy)`,
    created_at: new Date().toISOString(),
  };
  return createSuite(copy);
}

function cryptoRandomId(): string {
  // Browser-native uuid v4 when available; cheap fallback otherwise.
  const c = (globalThis as unknown as { crypto?: Crypto }).crypto;
  if (c && typeof c.randomUUID === 'function') return c.randomUUID();
  return 'xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx'.replace(/[xy]/g, ch => {
    const r = (Math.random() * 16) | 0;
    const v = ch === 'x' ? r : (r & 0x3) | 0x8;
    return v.toString(16);
  });
}

// ── useVariantConfig ────────────────────────────────────

interface VariantConfigResult {
  /** `${experimentId}/${variantId}` the snapshot was fetched for. */
  key: string;
  config: string | null;
  error: string | null;
}

export function useVariantConfig(experimentId: string | undefined, variantId: string | undefined) {
  const key = experimentId && variantId ? `${experimentId}/${variantId}` : null;
  const [result, setResult] = useState<VariantConfigResult | null>(null);

  useEffect(() => {
    if (!experimentId || !variantId) return;
    const target = `${experimentId}/${variantId}`;
    let cancelled = false;
    axios.get<string>(
      `${API}/experiments/${encodeURIComponent(experimentId)}/variants/${encodeURIComponent(variantId)}/config`,
      { responseType: 'text' }
    )
      .then(res => { if (!cancelled) setResult({ key: target, config: res.data, error: null }); })
      .catch(() => { if (!cancelled) setResult({ key: target, config: null, error: 'Config snapshot not available.' }); });
    return () => { cancelled = true; };
  }, [experimentId, variantId]);

  // Loading is derived: we are loading whenever a key is requested and the
  // latest settled result is for a different key (or none has settled yet).
  const loading = key != null && result?.key !== key;

  return { config: result?.config ?? null, loading, error: result?.error ?? null };
}

// ── Available pipeline configs (variant.config_path picker) ──

export interface PipelineConfigEntry {
  path: string;
  name: string;
  /** Worker IDs that advertised this config at registration. Empty when
   *  the orchestrator returned its own local-scan fallback (no workers
   *  registered) — purely informational; the UI can display availability. */
  worker_ids?: string[];
}

export function useConfigs() {
  const [configs, setConfigs] = useState<PipelineConfigEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    axios.get(`${API}/configs`)
      .then(res => {
        const data = res.data;
        const items = Array.isArray(data) ? data : data?.data ?? [];
        setConfigs(items);
        setError(null);
      })
      .catch(err => setError(err.message))
      .finally(() => setLoading(false));
  }, []);

  return { configs, loading, error };
}

// ── Experiment mutations ────────────────────────────────

export interface CreateExperimentRequest {
  name: string;
  description?: string;
  test_suite_id: string;
  variants: Variant[];
  repetitions: number;
}

/** Create a new experiment. Returns the server-assigned experiment. */
export async function createExperiment(req: CreateExperimentRequest): Promise<Experiment> {
  const res = await axios.post(`${API}/experiments`, req);
  const data = res.data;
  if (data && typeof data === 'object' && 'error' in data && data.error) {
    const err = data.error as { message?: string; code?: string };
    throw new Error(err.message ?? err.code ?? 'create experiment failed');
  }
  return (data?.data ?? data) as Experiment;
}
