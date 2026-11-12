/* ── dashboard mode & toggleable sections ────────────────────
   For the first release, production/FinOps/SRE telemetry is gated behind
   `DASHBOARD_PRODUCTION_MODE` (set `VITE_DASHBOARD_PRODUCTION_MODE=true` or
   flip the constant below).  When false, only essential developer sections are
   loaded and shown. */
export const DASHBOARD_PRODUCTION_MODE: boolean =
  import.meta.env.VITE_DASHBOARD_PRODUCTION_MODE === 'true' ||
  import.meta.env.VITE_PRODUCTION_DASHBOARD === 'true';

export interface DashboardSection {
  id: string;
  label: string;
  group: string;
  productionOnly?: boolean;
}

export const DASHBOARD_SECTIONS: DashboardSection[] = [
  { id: 'health', label: 'Live pipelines & health', group: 'Overview' },
  { id: 'kpis', label: 'Summary KPI cards', group: 'Overview' },
  { id: 'resources', label: 'Resource efficiency (CPU & RAM)', group: 'Overview' },
  { id: 'audioQuality', label: 'Audio quality KPIs', group: 'Overview', productionOnly: true },
  { id: 'business', label: 'Business / cost efficiency', group: 'Cost', productionOnly: true },
  { id: 'failedSessions', label: 'Failed sessions', group: 'Reliability' },
  { id: 'activity', label: 'Activity feed (anomalies & issues)', group: 'Reliability', productionOnly: true },
  { id: 'pipelineMatrix', label: 'Per-pipeline comparison', group: 'Pipelines' },
  { id: 'latencyByPipeline', label: 'Latency by pipeline (chart)', group: 'Pipelines', productionOnly: true },
  { id: 'filters', label: 'Pipeline & provider filters', group: 'Controls' },
  { id: 'latencyChart', label: 'End-to-end latency (chart)', group: 'Latency', productionOnly: true },
  { id: 'llmChart', label: 'LLM performance (chart)', group: 'LLM', productionOnly: true },
  { id: 'tokenChart', label: 'Token usage (chart)', group: 'LLM', productionOnly: true },
  { id: 'tokenTable', label: 'Token & API usage by pipeline', group: 'LLM', productionOnly: true },
  { id: 'costByPipeline', label: 'LLM cost by pipeline (chart)', group: 'Cost', productionOnly: true },
  { id: 'providerSpend', label: 'Provider spend', group: 'Cost', productionOnly: true },
];

export const DASHBOARD_SECTION_IDS = new Set(DASHBOARD_SECTIONS.map((s) => s.id));

export const DASHBOARD_PRODUCTION_ONLY_IDS = new Set(
  DASHBOARD_SECTIONS.filter((s) => s.productionOnly).map((s) => s.id),
);

export function getAvailableSections(productionMode: boolean): DashboardSection[] {
  return productionMode
    ? DASHBOARD_SECTIONS
    : DASHBOARD_SECTIONS.filter((s) => !s.productionOnly);
}

/* Sections hidden by default for FIRST-TIME users (no saved preference yet).
   These are the time-series charts that overlap with the "Latency by pipeline"
   chart / tables and tend to look empty on low-traffic windows.  Power users
   can re-enable any of them from the Customize menu (the choice then persists).
   Existing users with a saved preference are unaffected. */
export const DASHBOARD_DEFAULT_HIDDEN = new Set<string>([
  'latencyChart', // End-to-end latency — redundant with "Latency by pipeline"
  'llmChart',     // LLM performance time chart — sparse on low traffic
  'tokenChart',   // Token usage time chart — sparse; table covers the totals
]);

