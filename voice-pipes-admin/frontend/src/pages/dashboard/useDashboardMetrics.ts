import { useMemo } from 'react';
import { GLOBAL_SLO, breachPct } from '../../lib/slo';
import { deriveCostEfficiency } from '../../lib/business';
import { aggregate, bucketSizeMs, latencyYDomain, percentileOf } from './aggregation';
import type { Agg, AnalyticsData, Bucket, FailedSession, KPI, PipelineRow, ProviderSpend } from './types';

interface DashboardMetricsInput {
  data: AnalyticsData | null;
  priorData: AnalyticsData | null;
  days: number;
  bucket: Bucket;
  agg: Agg;
  selectedPipelines: Set<string>;
  selectedSttProviders: Set<string>;
  selectedTtsProviders: Set<string>;
  selectedLlmModels: Set<string>;
}

/** Everything the Dashboard derives from the fetched window and the active
 *  filters: filter options, chart series, KPIs, SLO breaches, the
 *  per-pipeline matrix, bucketed series and the table sources. */
export function useDashboardMetrics({
  data,
  priorData,
  days,
  bucket,
  agg,
  selectedPipelines,
  selectedSttProviders,
  selectedTtsProviders,
  selectedLlmModels,
}: DashboardMetricsInput) {
  /* Unique pipeline names for the filter.  1b — prefer the server's
   * ``available_pipelines`` (the full set for the window, independent of the
   * active filter) so the picker doesn't collapse when filtering server-side.
   * Falls back to deriving from utterances for older backends. */
  const pipelineNames = useMemo(() => {
    if (!data) return [];
    if (data.available_pipelines && data.available_pipelines.length > 0) {
      return [...data.available_pipelines].sort();
    }
    const names = new Set(data.utterances.map((u) => u.pipeline_name || 'unknown'));
    return Array.from(names).sort();
  }, [data]);

  /* Unique provider / model lists for the P3 #15 chip filters.  Drawn from
   * the full utterance set (not the filtered view) so toggling one filter
   * does not collapse the option list. */
  const sttProviderOptions = useMemo(() => {
    if (!data) return [];
    const s = new Set<string>();
    for (const u of data.utterances) if (u.stt_provider) s.add(u.stt_provider);
    return Array.from(s).sort();
  }, [data]);
  const ttsProviderOptions = useMemo(() => {
    if (!data) return [];
    const s = new Set<string>();
    for (const u of data.utterances) if (u.tts_provider) s.add(u.tts_provider);
    return Array.from(s).sort();
  }, [data]);
  const llmModelOptions = useMemo(() => {
    if (!data) return [];
    const s = new Set<string>();
    for (const u of data.utterances) if (u.model) s.add(u.model);
    return Array.from(s).sort();
  }, [data]);

  /* Filtered utterances for KPI recompute */
  const filteredUtterances = useMemo(() => {
    if (!data) return [];
    let list = data.utterances;
    if (selectedPipelines.size > 0) {
      list = list.filter((u) => selectedPipelines.has(u.pipeline_name || 'unknown'));
    }
    if (selectedSttProviders.size > 0) {
      list = list.filter((u) => selectedSttProviders.has(u.stt_provider));
    }
    if (selectedTtsProviders.size > 0) {
      list = list.filter((u) => selectedTtsProviders.has(u.tts_provider));
    }
    if (selectedLlmModels.size > 0) {
      list = list.filter((u) => u.model != null && selectedLlmModels.has(u.model));
    }
    return list;
  }, [data, selectedPipelines, selectedSttProviders, selectedTtsProviders, selectedLlmModels]);

  /* Prepare chart data — sort by time, add display label.  Cap at 500 points
   * (even-stride downsample) so Recharts paint stays responsive on large
   * windows; KPIs/SLOs/bucketed series stay on the full set. */
  const chartData = useMemo(() => {
    if (!data) return [];
    
    // ISO8601 strings sort correctly alphabetically, which is much faster than parsing Dates
    const sorted = [...filteredUtterances]
      .filter((u) => u.timestamp)
      .sort((a, b) => (a.timestamp < b.timestamp ? -1 : a.timestamp > b.timestamp ? 1 : 0));
      
    const MAX_POINTS = 500;
    
    // Downsample first to avoid heavy parsing on invisible points
    let sampled: typeof sorted;
    if (sorted.length <= MAX_POINTS) {
      sampled = sorted;
    } else {
      const stride = sorted.length / MAX_POINTS;
      sampled = [];
      for (let i = 0; i < MAX_POINTS; i++) {
        sampled.push(sorted[Math.floor(i * stride)]);
      }
      // Always keep the last point so the right edge of the chart isn't stale.
      if (sampled[sampled.length - 1] !== sorted[sorted.length - 1]) {
        sampled.push(sorted[sorted.length - 1]);
      }
    }

    // Now apply expensive Date parsing ONLY to the points we will actually chart
    return sampled.map((u) => {
      const ts = Date.parse(u.timestamp);
      return {
        ...u,
        _time: ts,
        _timeLabel: new Date(ts).toLocaleString(),
      };
    });
  }, [data, filteredUtterances]);

  /* Clamp the End-to-End latency Y axis to the bulk of total-latency values so
   * outliers don't flatten the chart (mirrors the bucketed chart behaviour). */
  const e2eYDomain = useMemo(() => {
    const vals = chartData
      .map((u) => u.total_ms)
      .filter((v): v is number => typeof v === 'number');
    return latencyYDomain(vals, GLOBAL_SLO.total_p95_ms);
  }, [chartData]);

  /* Single-pass derivation of the four memos that all walk filteredUtterances:
   * KPIs (when filters are active), audio-quality KPIs, SLO breach counters,
   * and the per-pipeline matrix.  Previously each was its own useMemo with
   * its own scan; folding them into one reducer halves the post-fetch
   * main-thread work on large windows. */
  const derived = useMemo(() => {
    type Bucket = {
      latencies: number[]; ttfts: number[]; eps: number[]; tps: number[];
      sessions: Set<string>; failedTurns: number; turns: number;
    };
    const byName = new Map<string, Bucket>();
    const getBucket = (name: string): Bucket => {
      let cur = byName.get(name);
      if (!cur) {
        cur = { latencies: [], ttfts: [], eps: [], tps: [], sessions: new Set(), failedTurns: 0, turns: 0 };
        byName.set(name, cur);
      }
      return cur;
    };

    // Pooled vectors for KPI / audio-quality / SLO aggregations.
    const totalLats: number[] = [];
    const ttftAll: number[] = [];
    const epAll: number[] = [];
    const tpsAll: number[] = [];
    const ttfas: number[] = [];
    const buffers: number[] = [];
    const rtts: number[] = [];
    const emitRtfs: number[] = [];
    const sessionIds = new Set<string>();
    const errorsByComp: Record<string, number> = { stt: 0, llm: 0, tts: 0, other: 0 };
    let failedTurns = 0;
    let turnsWithUnder = 0;
    let turnsOverRealtime = 0;
    let epSum = 0;
    let epCount = 0;
    // S2S provider endpointing (read defensively — optional on the wire).
    let pepSum = 0;
    let pepCount = 0;

    for (const u of filteredUtterances) {
      sessionIds.add(u.session_id);
      const name = u.pipeline_name || 'unknown';
      const cur = getBucket(name);
      cur.turns += 1;
      cur.sessions.add(u.session_id);
      if (u.had_error === true) {
        failedTurns += 1;
        cur.failedTurns += 1;
      }
      if (u.total_ms != null) {
        totalLats.push(u.total_ms);
        cur.latencies.push(u.total_ms);
      }
      if (u.ttft_ms != null) {
        ttftAll.push(u.ttft_ms);
        cur.ttfts.push(u.ttft_ms);
      }
      if (u.endpointing_ms != null) {
        epAll.push(u.endpointing_ms);
        cur.eps.push(u.endpointing_ms);
        epSum += u.endpointing_ms;
        epCount += 1;
      }
      if (typeof u.provider_endpointing_ms === 'number' && Number.isFinite(u.provider_endpointing_ms)) {
        pepSum += u.provider_endpointing_ms;
        pepCount += 1;
      }
      if (u.tokens_per_sec != null) {
        tpsAll.push(u.tokens_per_sec);
        cur.tps.push(u.tokens_per_sec);
      }
      if (u.client_ttfa_ms != null) ttfas.push(u.client_ttfa_ms);
      if (u.client_receive_to_play_ms != null) buffers.push(u.client_receive_to_play_ms);
      if (u.barge_in_rtt_ms != null) rtts.push(u.barge_in_rtt_ms);
      if ((u.audio_underruns ?? 0) > 0) turnsWithUnder += 1;
      if (u.emit_rtf != null && Number.isFinite(u.emit_rtf)) {
        emitRtfs.push(u.emit_rtf);
        if (u.emit_rtf > 1.0) turnsOverRealtime += 1;
      }
      for (const c of u.error_components ?? []) {
        errorsByComp[c] = (errorsByComp[c] ?? 0) + 1;
      }
    }

    const totalTurns = filteredUtterances.length;
    const avg = (arr: number[]): number | null =>
      arr.length === 0 ? null : Math.round(arr.reduce((a, b) => a + b, 0) / arr.length);

    const pct = (n: number, d: number) => (d > 0 ? Math.round((n / d) * 1000) / 10 : null);

    const audioQuality = {
      avgTtfaMs: avg(ttfas),
      avgBufferMs: avg(buffers),
      avgBargeRttMs: avg(rtts),
      underrunPct: pct(turnsWithUnder, totalTurns),
      ttfaSamples: ttfas.length,
      rttSamples: rtts.length,
      underrunTurns: turnsWithUnder,
      // Output throughput distribution.
      emitRtfP90: percentileOf(emitRtfs, 0.9),
      emitRtfMax: emitRtfs.length > 0 ? Math.max(...emitRtfs) : null,
      emitRtfSamples: emitRtfs.length,
      turnsOverRealtime,
    };
    const sloBreaches = {
      latency: breachPct(totalLats, GLOBAL_SLO.total_p95_ms),
      ttft: breachPct(ttftAll, GLOBAL_SLO.ttft_p95_ms),
      endpointing: breachPct(epAll, GLOBAL_SLO.endpointing_p95_ms),
    };
    const avgEndpointingMs = epCount > 0 ? Math.round(epSum / epCount) : null;
    const avgProviderEndpointingMs = pepCount > 0 ? Math.round(pepSum / pepCount) : null;

    // Filtered-window KPI rebuild (used when any chip filter is active).
    const sortedLat = [...totalLats].sort((a, b) => a - b);
    const filteredKpi = {
      total_sessions: sessionIds.size,
      total_utterances: totalTurns,
      avg_latency_ms: totalLats.length > 0
        ? Math.round(totalLats.reduce((a, b) => a + b, 0) / totalLats.length * 10) / 10
        : null,
      p95_latency_ms: sortedLat.length > 0
        ? Math.round(sortedLat[Math.floor(sortedLat.length * 0.95)] * 10) / 10
        : null,
      avg_tokens_per_sec: tpsAll.length > 0
        ? Math.round(tpsAll.reduce((a, b) => a + b, 0) / tpsAll.length * 10) / 10
        : null,
      failed_turns: failedTurns,
      error_rate_pct: pct(failedTurns, totalTurns),
      errors_by_component: errorsByComp,
    };

    return { byName, audioQuality, sloBreaches, avgEndpointingMs, avgProviderEndpointingMs, filteredKpi };
  }, [filteredUtterances]);

  /* Recompute KPIs for the selected pipeline(s) */
  const kpi = useMemo<KPI | null>(() => {
    if (!data) return null;
    const noFilters =
      selectedPipelines.size === 0 &&
      selectedSttProviders.size === 0 &&
      selectedTtsProviders.size === 0 &&
      selectedLlmModels.size === 0;
    if (noFilters) return data.kpi;
    // Filtered-window KPI: failed_sessions comes from the server list narrowed
    // to selected pipelines; the rest is folded from `derived.filteredKpi`.
    const failedSessionsInView = (data.failed_sessions ?? []).filter((fs) =>
      selectedPipelines.has(fs.pipeline_name || 'unknown'),
    );
    return {
      ...derived.filteredKpi,
      failed_sessions: failedSessionsInView.length,
    };
  }, [data, selectedPipelines, selectedSttProviders, selectedTtsProviders, selectedLlmModels, derived]);

  /* Business / cost-efficiency metrics from the (filtered) window. */
  const costEfficiency = useMemo(
    () => (kpi ? deriveCostEfficiency({
      spendTotal: kpi.spend_total_usd,
      sessions: kpi.total_sessions,
      turns: kpi.total_utterances,
      windowDays: days,
    }) : null),
    [kpi, days],
  );

  /* Avg endpointing latency across filtered utterances */
  const avgEndpointingMs = derived.avgEndpointingMs;
  /* S2S provider endpointing: per-utterance
   * first, the analytics KPI rollup as fallback for rollups that carry only
   * the aggregate. */
  const avgProviderEndpointingMs = derived.avgProviderEndpointingMs ?? data?.kpi?.avg_provider_endpointing_ms ?? null;

  /* P3 #16 — Audio quality KPIs.  Returns null for any KPI lacking samples so
   * the card can render an em-dash. */
  const audioQuality = derived.audioQuality;

  /* SLO breach percentages over the filtered window. */
  const sloBreaches = derived.sloBreaches;

  /* Δ vs prior period — percentage change of selected KPIs.  When the prior
   * fetch is in flight or unavailable, deltas are null and KpiCard hides the
   * indicator. The denominator-zero case also yields null. */
  const priorDeltas = useMemo(() => {
    const change = (curr: number | null | undefined, prev: number | null | undefined) => {
      if (curr == null || prev == null || !Number.isFinite(curr) || !Number.isFinite(prev)) return null;
      if (prev === 0) return curr === 0 ? 0 : null;
      return ((curr - prev) / prev) * 100;
    };
    if (!priorData || !data) {
      return {
        sessions: null as number | null,
        turns: null as number | null,
        errorRate: null as number | null,
        avgLatency: null as number | null,
        tps: null as number | null,
      };
    }
    const prevKpi = priorData.kpi || ({} as typeof data.kpi);
    return {
      sessions: change(kpi?.total_sessions, prevKpi.total_sessions),
      turns: change(kpi?.total_utterances, prevKpi.total_utterances),
      errorRate: change(kpi?.error_rate_pct, prevKpi.error_rate_pct),
      avgLatency: change(kpi?.avg_latency_ms, prevKpi.avg_latency_ms),
      tps: change(kpi?.avg_tokens_per_sec, prevKpi.avg_tokens_per_sec),
    };
  }, [data, priorData, kpi]);

  /* Failed sessions filtered by the pipeline multi-select */
  const failedSessionsInView = useMemo<FailedSession[]>(() => {
    if (!data?.failed_sessions) return [];
    if (selectedPipelines.size === 0) return data.failed_sessions;
    return data.failed_sessions.filter((fs) =>
      selectedPipelines.has(fs.pipeline_name || 'unknown'),
    );
  }, [data, selectedPipelines]);

  /* Per-pipeline matrix: one row per pipeline with key health & perf stats.
   * Consumes the per-pipeline buckets already built in `derived` so we don't
   * re-walk `filteredUtterances` a second time. */
  const pipelineMatrix = useMemo<PipelineRow[]>(() => {
    if (!derived.byName.size && !failedSessionsInView.length) return [];
    // Count failed sessions per pipeline; track extra session IDs for pipelines
    // that have failures but no successful turns (so they still appear).
    const failedByName = new Map<string, number>();
    const extraSessionsByName = new Map<string, Set<string>>();
    for (const fs of failedSessionsInView) {
      const name = fs.pipeline_name || 'unknown';
      failedByName.set(name, (failedByName.get(name) ?? 0) + 1);
      let extras = extraSessionsByName.get(name);
      if (!extras) {
        extras = new Set();
        extraSessionsByName.set(name, extras);
      }
      extras.add(fs.session_id);
    }
    const pct = (n: number, d: number) => (d > 0 ? Math.round((n / d) * 1000) / 10 : null);
    const avg = (arr: number[]): number | null =>
      arr.length === 0 ? null : Math.round(arr.reduce((a, b) => a + b, 0) / arr.length);
    const percentileFromSorted = (sorted: number[], p: number): number | null => {
      if (sorted.length === 0) return null;
      const idx = Math.min(sorted.length - 1, Math.floor(sorted.length * p));
      return Math.round(sorted[idx]);
    };
    const allNames = new Set<string>([...derived.byName.keys(), ...failedByName.keys()]);
    const rows: PipelineRow[] = [];
    for (const name of allNames) {
      const s = derived.byName.get(name);
      const sortedLat = s ? [...s.latencies].sort((a, b) => a - b) : [];
      const extras = extraSessionsByName.get(name);
      const sessionCount = s
        ? (extras ? new Set([...s.sessions, ...extras]).size : s.sessions.size)
        : (extras?.size ?? 0);
      rows.push({
        name,
        sessions: sessionCount,
        turns: s?.turns ?? 0,
        p50: percentileFromSorted(sortedLat, 0.5),
        p95: percentileFromSorted(sortedLat, 0.95),
        avgTtft: avg(s?.ttfts ?? []),
        avgEp: avg(s?.eps ?? []),
        avgTps: avg(s?.tps ?? []),
        failedSessions: failedByName.get(name) ?? 0,
        errorRatePct: pct(s?.failedTurns ?? 0, s?.turns ?? 0),
      });
    }
    return rows.sort((a, b) => (b.p95 ?? 0) - (a.p95 ?? 0));
  }, [derived, failedSessionsInView]);

  /* Time-bucketed per-pipeline series for the comparison chart. */
  const bucketMs = useMemo(() => bucketSizeMs(bucket, days), [bucket, days]);
  const bucketedSeries = useMemo(() => {
    if (filteredUtterances.length === 0) {
      return {
        rows: [] as Array<Record<string, number | null>>,
        pipelines: [] as string[],
        yMax: GLOBAL_SLO.total_p95_ms * 2,
        outliers: 0,
      };
    }
    // Group: bucket_ts -> pipelineName -> values[]
    const byBucket = new Map<number, Map<string, number[]>>();
    const pipelines = new Set<string>();
    for (const u of filteredUtterances) {
      if (u.total_ms == null || !u.timestamp) continue;
      const ts = new Date(u.timestamp).getTime();
      if (!Number.isFinite(ts)) continue;
      const bts = Math.floor(ts / bucketMs) * bucketMs;
      const name = u.pipeline_name || 'unknown';
      pipelines.add(name);
      let row = byBucket.get(bts);
      if (!row) {
        row = new Map();
        byBucket.set(bts, row);
      }
      let arr = row.get(name);
      if (!arr) {
        arr = [];
        row.set(name, arr);
      }
      arr.push(u.total_ms);
    }
    const rows = Array.from(byBucket.entries())
      .sort((a, b) => a[0] - b[0])
      .map(([bts, perPipe]) => {
        const out: Record<string, number | null> = { _time: bts };
        for (const name of pipelines) {
          const vals = perPipe.get(name);
          out[name] = vals && vals.length > 0 ? Math.round(aggregate(vals, agg)) : null;
        }
        return out;
      });
    // Clamp the Y axis to the bulk of the plotted (aggregated) values so a few
    // outlier buckets don't crush everything into a flat line at the bottom.
    const plotted: number[] = [];
    for (const r of rows) {
      for (const name of pipelines) {
        const v = r[name];
        if (typeof v === 'number') plotted.push(v);
      }
    }
    const { yMax, outliers } = latencyYDomain(plotted, GLOBAL_SLO.total_p95_ms);
    return { rows, pipelines: Array.from(pipelines).sort(), yMax, outliers };
  }, [filteredUtterances, bucketMs, agg]);

  /* Per-pipeline LLM cost over time, summed (not aggregated) per bucket.  Only
   * LLM cost is per-turn; STT/TTS cost is session-scoped and would require a
   * separate session-time chart we don't render today. */
  const bucketedCost = useMemo(() => {
    if (filteredUtterances.length === 0) {
      return { rows: [] as Array<Record<string, number | null>>, pipelines: [] as string[] };
    }
    const byBucket = new Map<number, Map<string, number>>();
    const pipelines = new Set<string>();
    for (const u of filteredUtterances) {
      if (u.llm_cost_usd == null || !u.timestamp) continue;
      const ts = new Date(u.timestamp).getTime();
      if (!Number.isFinite(ts)) continue;
      const bts = Math.floor(ts / bucketMs) * bucketMs;
      const name = u.pipeline_name || 'unknown';
      pipelines.add(name);
      let row = byBucket.get(bts);
      if (!row) {
        row = new Map();
        byBucket.set(bts, row);
      }
      row.set(name, (row.get(name) ?? 0) + u.llm_cost_usd);
    }
    const rows = Array.from(byBucket.entries())
      .sort((a, b) => a[0] - b[0])
      .map(([bts, perPipe]) => {
        const out: Record<string, number | null> = { _time: bts };
        for (const name of pipelines) {
          const v = perPipe.get(name);
          out[name] = v != null ? Math.round(v * 1_000_000) / 1_000_000 : null;
        }
        return out;
      });
    return { rows, pipelines: Array.from(pipelines).sort() };
  }, [filteredUtterances, bucketMs]);

  /* Aggregate tokens per pipeline */
  const tokensByPipeline = useMemo(() => {
    if (!filteredUtterances.length) return [];
    interface Bucket {
      prompt: number; output: number; turns: number; models: Set<string>; totalGenMs: number;
      sttAudioS: number; sttSpeechModel: Set<string>;
      ttsChars: number; ttsAudioS: number; ttsVoices: Set<string>;
      /** Sessions whose STT/TTS breakdown has already been folded in.  Each
       *  per-turn row carries the *session-cumulative* breakdown, so we count
       *  it once per session per pipeline. */
      countedSttSessions: Set<string>;
      countedTtsSessions: Set<string>;
    }
    const map = new Map<string, Bucket>();
    for (const u of filteredUtterances) {
      const key = u.pipeline_name || 'unknown';
      const cur: Bucket = map.get(key) ?? {
        prompt: 0, output: 0, turns: 0, models: new Set<string>(), totalGenMs: 0,
        sttAudioS: 0, sttSpeechModel: new Set<string>(),
        ttsChars: 0, ttsAudioS: 0, ttsVoices: new Set<string>(),
        countedSttSessions: new Set<string>(),
        countedTtsSessions: new Set<string>(),
      };
      cur.prompt += u.prompt_tokens ?? 0;
      cur.output += u.output_tokens ?? 0;
      cur.totalGenMs += u.generate_ms ?? 0;
      if (u.model) cur.models.add(u.model);
      if (u.stt_speech_model) cur.sttSpeechModel.add(u.stt_speech_model);
      if (u.tts_voice) cur.ttsVoices.add(u.tts_voice);

      // STT usage — fold the per-(provider, model) breakdown once per session.
      // Falls back to the legacy session-cumulative scalar when the breakdown
      // is missing (older capture data).
      if (!cur.countedSttSessions.has(u.session_id)) {
        if (u.stt_usage_by_model && u.stt_usage_by_model.length > 0) {
          for (const row of u.stt_usage_by_model) {
            cur.sttAudioS += row.audio_s ?? 0;
            if (row.model) cur.sttSpeechModel.add(row.model);
          }
          cur.countedSttSessions.add(u.session_id);
        } else if (u.stt_audio_duration_s != null) {
          cur.sttAudioS += u.stt_audio_duration_s;
          cur.countedSttSessions.add(u.session_id);
        }
      }

      // TTS usage — same pattern; per-voice breakdown captures mid-session
      // provider/voice switches that the legacy scalar would obscure.
      if (!cur.countedTtsSessions.has(u.session_id)) {
        if (u.tts_usage_by_voice && u.tts_usage_by_voice.length > 0) {
          for (const row of u.tts_usage_by_voice) {
            cur.ttsChars += row.chars ?? 0;
            cur.ttsAudioS += row.audio_s ?? 0;
            if (row.voice) cur.ttsVoices.add(row.voice);
          }
          cur.countedTtsSessions.add(u.session_id);
        } else if (u.tts_audio_duration_s != null) {
          cur.ttsChars += u.tts_chars ?? 0;
          cur.ttsAudioS += u.tts_audio_duration_s;
          cur.countedTtsSessions.add(u.session_id);
        }
      }
      if (u.prompt_tokens != null || u.output_tokens != null) cur.turns += 1;
      map.set(key, cur);
    }
    return Array.from(map.entries())
      .map(([name, v]) => ({
        name,
        prompt: v.prompt,
        output: v.output,
        total: v.prompt + v.output,
        turns: v.turns,
        models: Array.from(v.models),
        totalGenMs: v.totalGenMs,
        sttAudioS: v.sttAudioS,
        sttSpeechModels: Array.from(v.sttSpeechModel),
        ttsChars: v.ttsChars,
        ttsAudioS: v.ttsAudioS,
        ttsVoices: Array.from(v.ttsVoices),
      }))
      .sort((a, b) => b.total - a.total);
  }, [filteredUtterances]);

  /* ── Collapsible + paged table state ───────────────────
   * Sorted source arrays (failed sessions newest-first, provider spend by
   * total desc) plus collapse + paging state for each of the four tables. */
  const failedSessionsSorted = useMemo<FailedSession[]>(
    () =>
      failedSessionsInView
        .slice()
        .sort((a, b) => (b.timestamp || '').localeCompare(a.timestamp || '')),
    [failedSessionsInView],
  );
  const spendRows = useMemo<[string, ProviderSpend][]>(() => {
    const sbp = data?.kpi.spend_by_provider;
    if (!sbp) return [];
    return Object.entries(sbp).sort((a, b) => (b[1].total ?? 0) - (a[1].total ?? 0));
  }, [data]);

  /* Activity feed lists (anomalies / recent issues) — paged so the feed
   * stays bounded instead of the old hard `.slice(0, 15|30)` truncation. */
  const anomalySessions = useMemo(() => data?.anomaly_sessions ?? [], [data]);

  return {
    pipelineNames,
    sttProviderOptions,
    ttsProviderOptions,
    llmModelOptions,
    filteredUtterances,
    chartData,
    e2eYDomain,
    kpi,
    costEfficiency,
    avgEndpointingMs,
    avgProviderEndpointingMs,
    audioQuality,
    sloBreaches,
    priorDeltas,
    failedSessionsInView,
    pipelineMatrix,
    bucketedSeries,
    bucketedCost,
    tokensByPipeline,
    failedSessionsSorted,
    spendRows,
    anomalySessions,
  };
}

export type DashboardMetrics = ReturnType<typeof useDashboardMetrics>;
