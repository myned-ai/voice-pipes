import { useEffect, useState } from 'react';
import axios from 'axios';
import { API_BASE } from '../../config/api';
import type { AnalyticsData, IssueRow } from './types';

export interface ClearMetricsOptions {
  pipeline?: string | null;
  sessionId?: string | null;
  deleteCaptures?: boolean;
}

export async function clearDashboardMetrics(options: ClearMetricsOptions = {}): Promise<void> {
  await axios.post(`${API_BASE}/api/v1/analytics/clear`, {
    pipeline: options.pipeline ?? null,
    session_id: options.sessionId ?? null,
    delete_captures: Boolean(options.deleteCaptures),
  });
}

/** Fetches the analytics window (and, in production mode, the prior window for
 *  the Δ indicators and the recent-issues feed) on `days`, pipeline-filter, or
 *  `refreshToken` change. */
export function useDashboardData(
  days: number,
  selectedPipelines: Set<string>,
  productionMode = false,
  refreshToken = 0,
) {
  const [data, setData] = useState<AnalyticsData | null>(null);
  const [priorData, setPriorData] = useState<AnalyticsData | null>(null);
  const [issues, setIssues] = useState<IssueRow[]>([]);
  // Set when the backend says its issues store cannot be used, so an empty
  // feed is not read as "no issues".
  const [issuesError, setIssuesError] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    // 1b — push the pipeline filter to the server so we don't download other
    // pipelines' data.  Sorted for a stable cache key.  Empty => all pipelines.
    const pipelineParam =
      selectedPipelines.size > 0 ? Array.from(selectedPipelines).sort().join(',') : undefined;
    const params: Record<string, unknown> = { days };
    if (pipelineParam) params.pipeline = pipelineParam;
    if (!productionMode) params.lite = true;
    axios
      .get(`${API_BASE}/api/v1/analytics`, { params })
      .then((res) => {
        if (cancelled) return;
        // Previous-period deltas are cleared together with the new primary
        // data (batched) so stale Δ indicators never sit next to fresh KPIs.
        setError(null);
        setPriorData(null);
        setData(res.data.data);
        if (!productionMode) return;
        // Defer the prior-period fetch (used only for the Δ vs prior
        // indicators in production mode) until after the primary KPIs land.
        axios
          .get(`${API_BASE}/api/v1/analytics`, { params: { ...params, offset_days: days } })
          .then((r) => {
            if (!cancelled) setPriorData(r.data.data);
          })
          .catch(() => {
            if (!cancelled) setPriorData(null);
          });
      })
      .catch((err) => {
        if (cancelled) return;
        console.error(err);
        setError('Could not load analytics data. Is the backend running?');
      });
    return () => { cancelled = true; };
  }, [days, selectedPipelines, productionMode, refreshToken]);

  // Recent-issues feed (P3 #18) — only polled when productionMode is enabled.
  useEffect(() => {
    if (!productionMode) return;
    let cancelled = false;
    const fetchIssues = () => {
      axios
        .get(`${API_BASE}/api/v1/issues`, { params: { days, limit: 50 } })
        .then((res) => {
          if (cancelled) return;
          setIssues(res.data?.data?.issues ?? []);
          const failure = res.data?.error;
          setIssuesError(failure ? (failure.message ?? failure.code ?? 'issues store unavailable') : null);
        })
        .catch(() => {
          if (!cancelled) setIssues([]);
        });
    };
    fetchIssues();
    const handle = window.setInterval(fetchIssues, 30000);
    return () => {
      cancelled = true;
      window.clearInterval(handle);
    };
  }, [days, productionMode, refreshToken]);

  return { data, priorData, issues, issuesError, error };
}

