import type React from 'react';
import { Activity, AlertTriangle } from 'lucide-react';
import { pipelineColor } from '../../lib/pipelineColor';
import { TablePager } from './TableControls';
import type { NavigateFunction } from 'react-router-dom';
import type { ActivityTab, AnalyticsData, AnomalySession, IssueRow, KPI } from './types';
import type { PagedTable } from './tableState';

interface ActivityFeedProps {
  isDark: boolean;
  navigate: NavigateFunction;
  data: AnalyticsData;
  issues: IssueRow[];
  issuesError: string | null;
  days: number;
  activityTab: ActivityTab;
  setActivityTab: React.Dispatch<React.SetStateAction<ActivityTab>>;
  kpi: KPI;
  anomalyPaged: PagedTable<AnomalySession>;
  issuesPaged: PagedTable<IssueRow>;
}

/** Tabbed feed of anomalous sessions and recent issues. */
export function ActivityFeed({ isDark, navigate, data, issues, issuesError, days, activityTab, setActivityTab, kpi, anomalyPaged, issuesPaged }: ActivityFeedProps) {
  const anomalyCount = data?.anomaly_sessions?.length ?? 0;
  const issueCount = issues.length;
  if (anomalyCount === 0 && issueCount === 0) return null;
  // If the persisted tab has zero rows but the other has data,
  // render the non-empty tab instead (without mutating state,
  // so the user's preference is restored on next load).
  const effectiveTab: ActivityTab =
    activityTab === 'anomalies' && anomalyCount === 0 && issueCount > 0
      ? 'issues'
      : activityTab === 'issues' && issueCount === 0 && anomalyCount > 0
        ? 'anomalies'
        : activityTab;
  const tabBtn = (id: ActivityTab, label: string, count: number, icon: React.ReactNode, accent: 'amber' | 'rose') => {
    const active = effectiveTab === id;
    const accentText = accent === 'amber'
      ? (isDark ? 'text-amber-300' : 'text-amber-600')
      : (isDark ? 'text-rose-300' : 'text-rose-600');
    const baseText = isDark ? 'text-slate-400 hover:text-slate-200' : 'text-gray-500 hover:text-gray-800';
    const underline = active
      ? (accent === 'amber'
          ? (isDark ? 'border-amber-400' : 'border-amber-500')
          : (isDark ? 'border-rose-400' : 'border-rose-500'))
      : 'border-transparent';
    return (
      <button
        key={id}
        type="button"
        role="tab"
        aria-selected={active}
        onClick={() => setActivityTab(id)}
        className={`flex items-center gap-2 px-4 py-2.5 text-sm font-semibold border-b-2 transition-colors ${underline} ${active ? accentText : baseText}`}
      >
        {icon}
        <span>{label}</span>
        <span
          className={`text-xs font-normal px-1.5 py-0.5 rounded ${
            count === 0
              ? (isDark ? 'bg-slate-700/60 text-slate-500' : 'bg-gray-100 text-gray-400')
              : active
                ? (accent === 'amber'
                    ? (isDark ? 'bg-amber-500/20 text-amber-200' : 'bg-amber-100 text-amber-700')
                    : (isDark ? 'bg-rose-500/20 text-rose-200' : 'bg-rose-100 text-rose-700'))
                : (isDark ? 'bg-slate-700 text-slate-300' : 'bg-gray-100 text-gray-600')
          }`}
        >
          {count}
        </span>
      </button>
    );
  };
  return (
    <div className={`rounded-xl border ${isDark ? 'bg-slate-800 border-slate-700' : 'bg-white border-gray-200'}`}>
      <div role="tablist" className={`flex items-center gap-1 px-3 pt-1 border-b ${isDark ? 'border-slate-700' : 'border-gray-200'}`}>
        {tabBtn('anomalies', 'Anomalies in window', anomalyCount,
          <AlertTriangle size={16} className={isDark ? 'text-amber-400' : 'text-amber-500'} />, 'amber')}
        {tabBtn('issues', 'Recent issues', issueCount,
          <Activity size={16} className={isDark ? 'text-rose-400' : 'text-rose-500'} />, 'rose')}
      </div>

      {effectiveTab === 'anomalies' && (
        <div role="tabpanel" className="p-5">
          {anomalyCount === 0 ? (
            <div className={`py-8 text-center text-sm ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>
              No anomalies detected in this window.
            </div>
          ) : (
            <>
              {kpi.anomalies_total != null && kpi.anomalies_total > 0 && (
                <div className={`text-xs mb-3 ${isDark ? 'text-slate-500' : 'text-gray-500'}`}>
                  {kpi.anomalies_total} total across {anomalyCount} session{anomalyCount === 1 ? '' : 's'}
                </div>
              )}
              {kpi.anomalies_by_kind && Object.keys(kpi.anomalies_by_kind).length > 0 && (
                <div className="flex flex-wrap gap-1.5 mb-4">
                  {Object.entries(kpi.anomalies_by_kind)
                    .sort((a, b) => b[1] - a[1])
                    .map(([kind, n]) => (
                      <span
                        key={kind}
                        className={`text-xs px-2 py-1 rounded-md border ${
                          isDark
                            ? 'bg-amber-500/10 border-amber-500/30 text-amber-300'
                            : 'bg-amber-50 border-amber-200 text-amber-700'
                        }`}
                      >
                        <span className="font-mono">{kind}</span>
                        <span className="ml-1.5 font-semibold">{n}</span>
                      </span>
                    ))}
                </div>
              )}
              <ul className={`divide-y overflow-auto max-h-[28rem] ${isDark ? 'divide-slate-700' : 'divide-gray-100'}`}>
                {anomalyPaged.pageRows.map((s) => (
                  <li
                    key={s.session_id}
                    onClick={() => navigate(`/sessions/${s.session_id}`)}
                    className={`py-2 px-1 flex items-center justify-between gap-3 cursor-pointer rounded-md ${
                      isDark ? 'hover:bg-slate-700/40' : 'hover:bg-gray-50'
                    }`}
                  >
                    <div className="min-w-0 flex-1">
                      <div className={`text-xs ${isDark ? 'text-slate-400' : 'text-gray-500'} tabular-nums`}>
                        {s.timestamp ? new Date(s.timestamp).toLocaleString() : '—'}
                      </div>
                      <div className="flex items-center gap-2 mt-0.5">
                        <span
                          className="inline-block w-2 h-2 rounded-full shrink-0"
                          style={{ background: pipelineColor(s.pipeline_name || 'unknown') }}
                        />
                        <span className={`text-sm font-medium truncate ${isDark ? 'text-slate-200' : 'text-gray-800'}`}>
                          {s.pipeline_name || 'unknown'}
                        </span>
                        <span className={`text-xs font-mono truncate ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>
                          {s.session_id.slice(0, 8)}
                        </span>
                      </div>
                    </div>
                    <div className="flex items-center gap-1.5 flex-wrap justify-end shrink-0">
                      {Object.entries(s.kind_counts)
                        .sort((a, b) => b[1] - a[1])
                        .slice(0, 4)
                        .map(([k, n]) => (
                          <span
                            key={k}
                            className={`text-[10px] px-1.5 py-0.5 rounded font-mono ${
                              isDark ? 'bg-slate-700 text-slate-300' : 'bg-gray-100 text-gray-600'
                            }`}
                            title={`${n} × ${k}`}
                          >
                            {/* Strip the component prefix — including snake_case
                                ones such as `audio_post.over_budget` — the full
                                kind stays in the tooltip. */}
                            {k.replace(/^[a-z_]+\./, '')}·{n}
                          </span>
                        ))}
                      <span className={`ml-1 text-sm font-semibold tabular-nums ${isDark ? 'text-amber-300' : 'text-amber-700'}`}>
                        {s.anomalies_total}
                      </span>
                    </div>
                  </li>
                ))}
              </ul>
              <TablePager
                page={anomalyPaged.page}
                totalPages={anomalyPaged.totalPages}
                total={anomalyPaged.total}
                start={anomalyPaged.start}
                end={anomalyPaged.end}
                setPage={anomalyPaged.setPage}
                isDark={isDark}
                unit="sessions"
                className="-mx-5 px-5 mt-1"
              />
            </>
          )}
        </div>
      )}

      {effectiveTab === 'issues' && (
        <div role="tabpanel" className="p-5">
          {issueCount === 0 ? (
            <div className={`py-8 text-center text-sm ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>
              {issuesError
                ? `Issues are not being recorded: ${issuesError}.`
                : `No recorded issues in the last ${days}d.`}
            </div>
          ) : (
            <>
              <div className={`text-xs mb-3 ${isDark ? 'text-slate-500' : 'text-gray-500'}`}>
                {issueCount} in last {days}d
              </div>
              <ul className={`divide-y overflow-auto max-h-[28rem] ${isDark ? 'divide-slate-700' : 'divide-gray-100'}`}>
                {issuesPaged.pageRows.map((iss) => {
                  const sevColor = (() => {
                    if (iss.severity === 'error') {
                      return isDark ? 'bg-rose-500/15 text-rose-300 border-rose-500/40' : 'bg-rose-50 text-rose-700 border-rose-200';
                    }
                    if (iss.severity === 'warn') {
                      return isDark ? 'bg-amber-500/15 text-amber-300 border-amber-500/40' : 'bg-amber-50 text-amber-700 border-amber-200';
                    }
                    return isDark ? 'bg-slate-700 text-slate-300 border-slate-600' : 'bg-gray-100 text-gray-600 border-gray-200';
                  })();
                  const clickable = !!iss.session_id;
                  return (
                    <li
                      key={iss.id}
                      onClick={() => clickable && navigate(`/sessions/${iss.session_id}`)}
                      className={`py-2 px-1 flex items-start gap-3 rounded-md ${
                        clickable
                          ? `cursor-pointer ${isDark ? 'hover:bg-slate-700/40' : 'hover:bg-gray-50'}`
                          : ''
                      }`}
                    >
                      <span
                        className={`text-[10px] px-1.5 py-0.5 rounded border font-mono uppercase shrink-0 mt-0.5 ${sevColor}`}
                        title={iss.severity}
                      >
                        {iss.kind}
                      </span>
                      <div className="min-w-0 flex-1">
                        <div className={`text-sm truncate ${isDark ? 'text-slate-200' : 'text-gray-800'}`}>
                          {iss.summary || iss.kind}
                        </div>
                        <div className={`text-xs flex items-center gap-2 mt-0.5 ${isDark ? 'text-slate-500' : 'text-gray-500'}`}>
                          <span className="tabular-nums">{new Date(iss.ts * 1000).toLocaleString()}</span>
                          {iss.pipeline_name && (
                            <span className="flex items-center gap-1">
                              <span
                                className="inline-block w-1.5 h-1.5 rounded-full"
                                style={{ background: pipelineColor(iss.pipeline_name) }}
                              />
                              {iss.pipeline_name}
                            </span>
                          )}
                          {iss.session_id && (
                            <span className="font-mono">{iss.session_id.slice(0, 8)}</span>
                          )}
                        </div>
                      </div>
                    </li>
                  );
                })}
              </ul>
              <TablePager
                page={issuesPaged.page}
                totalPages={issuesPaged.totalPages}
                total={issuesPaged.total}
                start={issuesPaged.start}
                end={issuesPaged.end}
                setPage={issuesPaged.setPage}
                isDark={isDark}
                unit="issues"
                className="-mx-5 px-5 mt-1"
              />
            </>
          )}
        </div>
      )}
    </div>
  );
}
