import { Trash2, XCircle } from 'lucide-react';
import { Link } from 'react-router-dom';
import { pipelineColor } from '../../lib/pipelineColor';
import { fmtMs, fmtNum, fmtUsd } from './format';
import { CollapseToggle, TablePager } from './TableControls';
import type { FailedSession, KPI, PipelineRow, ProviderSpend } from './types';
import type { PagedTable } from './tableState';
import type { DashboardMetrics } from './useDashboardMetrics';

interface FailedSessionsTableProps {
  isDark: boolean;
  kpi: KPI;
  failedSessionsInView: FailedSession[];
  failedCollapsed: boolean;
  toggleFailed: () => void;
  failedPaged: PagedTable<FailedSession>;
  onDismissSession?: (sessionId: string) => void;
}

/** Failed sessions in the window. */
export function FailedSessionsTable({
  isDark,
  kpi,
  failedSessionsInView,
  failedCollapsed,
  toggleFailed,
  failedPaged,
  onDismissSession,
}: FailedSessionsTableProps) {
  return (
    <div className={`rounded-xl border ${isDark ? 'bg-rose-950/20 border-rose-900/50' : 'bg-rose-50 border-rose-200'}`}>
      <div className={`px-5 py-3 flex items-center justify-between border-b ${isDark ? 'border-rose-900/50' : 'border-rose-200'}`}>
        <CollapseToggle collapsed={failedCollapsed} onToggle={toggleFailed} isDark={isDark}>
          <h3 className={`text-sm font-semibold flex items-center gap-2 ${isDark ? 'text-rose-300' : 'text-rose-700'}`}>
            <XCircle size={16} />
            Failed sessions ({failedSessionsInView.length})
          </h3>
        </CollapseToggle>
        {kpi.errors_by_component && (
          <div className={`flex flex-wrap gap-2 text-xs ${isDark ? 'text-rose-300/80' : 'text-rose-700/80'}`}>
            {Object.entries(kpi.errors_by_component)
              .filter(([, n]) => n > 0)
              .map(([k, n]) => (
                <span key={k} className={`px-2 py-0.5 rounded border ${isDark ? 'border-rose-900/60 bg-rose-950/40' : 'border-rose-300 bg-rose-100'}`}>
                  {k}: {n}
                </span>
              ))}
          </div>
        )}
      </div>
      {!failedCollapsed && (<>
      <div className="overflow-auto max-h-[28rem]">
        <table className="w-full text-sm">
          <thead className={`sticky top-0 z-10 ${isDark ? 'bg-rose-950/60' : 'bg-rose-50'}`}>
            <tr className={isDark ? 'text-rose-300/70' : 'text-rose-700/70'}>
              <th className="text-left px-5 py-2 font-medium">When</th>
              <th className="text-left px-3 py-2 font-medium">Pipeline</th>
              <th className="text-left px-3 py-2 font-medium">Reason</th>
              <th className="text-right px-3 py-2 font-medium">Errors</th>
              <th className="text-right px-3 py-2 font-medium">Failed / Turns</th>
              <th className="text-left px-3 py-2 font-medium">Last error</th>
              <th className="px-5 py-2"></th>
            </tr>
          </thead>
          <tbody className={`divide-y ${isDark ? 'divide-rose-900/40' : 'divide-rose-200'}`}>
            {failedPaged.pageRows
              .map((fs) => {
                const compChips = Object.entries(fs.errors_by_component)
                  .filter(([, n]) => n > 0)
                  .map(([k, n]) => `${k}:${n}`)
                  .join(' · ');
                return (
                  <tr key={fs.session_id} className={isDark ? 'text-slate-200' : 'text-gray-800'}>
                    <td className="px-5 py-2 tabular-nums whitespace-nowrap">
                      {fs.timestamp ? new Date(fs.timestamp).toLocaleString() : '—'}
                    </td>
                    <td className="px-3 py-2">
                      <span className="inline-flex items-center gap-1.5">
                        <span
                          className="inline-block w-2.5 h-2.5 rounded-full"
                          style={{ background: pipelineColor(fs.pipeline_name) }}
                        />
                        {fs.pipeline_name || '—'}
                      </span>
                    </td>
                    <td className="px-3 py-2">
                      <span className={`text-xs px-2 py-0.5 rounded border ${
                        fs.reason === 'no_turns'
                          ? (isDark ? 'border-amber-700 bg-amber-900/30 text-amber-300' : 'border-amber-300 bg-amber-50 text-amber-700')
                          : (isDark ? 'border-rose-700 bg-rose-900/30 text-rose-300' : 'border-rose-300 bg-rose-100 text-rose-700')
                      }`}>
                        {fs.reason === 'no_turns' ? 'no turns' : 'errors'}
                      </span>
                    </td>
                    <td className="px-3 py-2 text-right tabular-nums">
                      {fs.errors_total}
                      {compChips && (
                        <div className={`text-xs ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>{compChips}</div>
                      )}
                    </td>
                    <td className="px-3 py-2 text-right tabular-nums">
                      {fs.failed_turn_count} / {fs.turn_count}
                    </td>
                    <td className={`px-3 py-2 max-w-[24rem] truncate ${isDark ? 'text-slate-400' : 'text-gray-500'}`} title={fs.last_error ?? ''}>
                      {fs.last_error ?? '—'}
                    </td>
                    <td className="px-5 py-2 text-right whitespace-nowrap">
                      <div className="inline-flex items-center gap-3">
                        <Link
                          to={`/sessions/${fs.session_id}`}
                          className={`text-xs underline ${isDark ? 'text-indigo-400 hover:text-indigo-300' : 'text-indigo-600 hover:text-indigo-700'}`}
                        >
                          open
                        </Link>
                        {onDismissSession && (
                          <button
                            type="button"
                            onClick={() => onDismissSession(fs.session_id)}
                            title={`Dismiss session ${fs.session_id} from dashboard`}
                            className={`text-xs inline-flex items-center gap-1 ${isDark ? 'text-slate-400 hover:text-rose-300' : 'text-gray-500 hover:text-rose-600'}`}
                          >
                            <Trash2 size={12} />
                            dismiss
                          </button>
                        )}
                      </div>
                    </td>
                  </tr>
                );
              })}
          </tbody>
        </table>
      </div>
      <TablePager
        page={failedPaged.page}
        totalPages={failedPaged.totalPages}
        total={failedPaged.total}
        start={failedPaged.start}
        end={failedPaged.end}
        setPage={failedPaged.setPage}
        isDark={isDark}
        unit="sessions"
        className={isDark ? 'border-rose-900/40' : 'border-rose-200'}
      />
      </>)}
    </div>
  );
}

interface PipelineMatrixTableProps {
  isDark: boolean;
  pipelineMatrix: PipelineRow[];
  matrixCollapsed: boolean;
  toggleMatrix: () => void;
  matrixPaged: PagedTable<PipelineRow>;
  onClearPipeline?: (pipelineName: string) => void;
}

/** Per-pipeline comparison matrix. */
export function PipelineMatrixTable({
  isDark,
  pipelineMatrix,
  matrixCollapsed,
  toggleMatrix,
  matrixPaged,
  onClearPipeline,
}: PipelineMatrixTableProps) {
  return (
    <div className={`rounded-xl border overflow-hidden ${isDark ? 'bg-slate-800/60 border-slate-700' : 'bg-white border-gray-200'}`}>
      <div className={`px-5 py-3 flex items-center justify-between border-b ${isDark ? 'border-slate-700' : 'border-gray-200'}`}>
        <CollapseToggle collapsed={matrixCollapsed} onToggle={toggleMatrix} isDark={isDark}>
          <h3 className={`text-sm font-semibold ${isDark ? 'text-slate-200' : 'text-gray-800'}`}>
            Per-pipeline comparison
          </h3>
        </CollapseToggle>
        <span className={`text-xs ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>
          {pipelineMatrix.length} pipeline{pipelineMatrix.length === 1 ? '' : 's'} · sorted by p95
        </span>
      </div>
      {!matrixCollapsed && (<>
      <div className="overflow-auto max-h-[28rem]">
        <table className="w-full text-sm">
          <thead className={`sticky top-0 z-10 text-xs uppercase tracking-wide ${isDark ? 'text-slate-400 bg-slate-900/95' : 'text-gray-500 bg-gray-50'}`}>
            <tr>
              <th className="text-left px-5 py-2">Pipeline</th>
              <th className="text-right px-3 py-2">Sessions</th>
              <th className="text-right px-3 py-2">Turns</th>
              <th className="text-right px-3 py-2">p50</th>
              <th className="text-right px-3 py-2">p95</th>
              <th className="text-right px-3 py-2">TTFT avg</th>
              <th className="text-right px-3 py-2">Endpointing avg</th>
              <th className="text-right px-3 py-2">tok/s</th>
              <th className="text-right px-3 py-2">Error rate</th>
              <th className="text-right px-5 py-2">Failed sessions</th>
              {onClearPipeline && <th className="px-4 py-2"></th>}
            </tr>
          </thead>
          <tbody className={isDark ? 'text-slate-200' : 'text-gray-800'}>
            {matrixPaged.pageRows.map((row) => {
              const errBad = row.errorRatePct != null && row.errorRatePct > 1;
              return (
                <tr key={row.name} className={isDark ? 'border-t border-slate-700/60' : 'border-t border-gray-100'}>
                  <td className="px-5 py-2">
                    <span className="inline-flex items-center gap-2">
                      <span
                        className="inline-block w-2.5 h-2.5 rounded-full"
                        style={{ background: pipelineColor(row.name) }}
                      />
                      <span className="font-medium">{row.name}</span>
                    </span>
                  </td>
                  <td className="px-3 py-2 text-right tabular-nums">{row.sessions}</td>
                  <td className="px-3 py-2 text-right tabular-nums">{row.turns}</td>
                  <td className="px-3 py-2 text-right tabular-nums">{fmtMs(row.p50)}</td>
                  <td className="px-3 py-2 text-right tabular-nums">{fmtMs(row.p95)}</td>
                  <td className="px-3 py-2 text-right tabular-nums">{fmtMs(row.avgTtft)}</td>
                  <td className="px-3 py-2 text-right tabular-nums">{fmtMs(row.avgEp)}</td>
                  <td className="px-3 py-2 text-right tabular-nums">
                    {row.avgTps != null ? `${fmtNum(row.avgTps)}` : '—'}
                  </td>
                  <td className={`px-3 py-2 text-right tabular-nums ${errBad ? (isDark ? 'text-rose-300' : 'text-rose-700') : ''}`}>
                    {row.errorRatePct != null ? `${row.errorRatePct.toFixed(1)}%` : '—'}
                  </td>
                  <td className={`px-5 py-2 text-right tabular-nums ${row.failedSessions > 0 ? (isDark ? 'text-rose-300' : 'text-rose-700') : ''}`}>
                    {row.failedSessions}
                  </td>
                  {onClearPipeline && (
                    <td className="px-4 py-2 text-right">
                      <button
                        type="button"
                        onClick={() => onClearPipeline(row.name)}
                        title={`Clear metrics for pipeline ${row.name}`}
                        className={`text-xs inline-flex items-center gap-1 ${isDark ? 'text-slate-400 hover:text-rose-300' : 'text-gray-500 hover:text-rose-600'}`}
                      >
                        <Trash2 size={12} />
                        clear
                      </button>
                    </td>
                  )}
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>
      <TablePager
        page={matrixPaged.page}
        totalPages={matrixPaged.totalPages}
        total={matrixPaged.total}
        start={matrixPaged.start}
        end={matrixPaged.end}
        setPage={matrixPaged.setPage}
        isDark={isDark}
        unit="pipelines"
      />
      </>)}
    </div>
  );
}

interface TokenUsageTableProps {
  isDark: boolean;
  tokenCollapsed: boolean;
  toggleToken: () => void;
  tokenPaged: PagedTable<DashboardMetrics['tokensByPipeline'][number]>;
}

/** Token and API usage per pipeline. */
export function TokenUsageTable({ isDark, tokenCollapsed, toggleToken, tokenPaged }: TokenUsageTableProps) {
  return (
    <div className={`rounded-xl p-5 border ${isDark ? 'bg-slate-800 border-slate-700' : 'bg-white border-gray-200'}`}>
      <div className="mb-4">
        <CollapseToggle collapsed={tokenCollapsed} onToggle={toggleToken} isDark={isDark}>
          <h3 className={`text-sm font-semibold ${isDark ? 'text-slate-300' : 'text-gray-700'}`}>
            Token &amp; API Usage by Pipeline
          </h3>
        </CollapseToggle>
      </div>
      {!tokenCollapsed && (<>
      <div className="overflow-auto max-h-[28rem]">
        <table className="w-full text-sm">
          <thead className={`sticky top-0 z-10 ${isDark ? 'bg-slate-800' : 'bg-white'}`}>
            <tr className={isDark ? 'text-slate-400' : 'text-gray-500'}>
              <th className="text-left pb-2 font-medium">Pipeline</th>
              <th className="text-left pb-2 font-medium">Model</th>
              <th className="text-right pb-2 font-medium">Prompt</th>
              <th className="text-right pb-2 font-medium">Output</th>
              <th className="text-right pb-2 font-medium">Total</th>
              <th className="text-right pb-2 font-medium">Turns</th>
              <th className="text-right pb-2 font-medium">Avg / Turn</th>
              <th className="text-right pb-2 font-medium">LLM Time</th>
              <th className="text-right pb-2 font-medium">STT Audio</th>
              <th className="text-right pb-2 font-medium">TTS Audio</th>
            </tr>
          </thead>
          <tbody className={`divide-y ${isDark ? 'divide-slate-700' : 'divide-gray-100'}`}>
            {tokenPaged.pageRows.map((row) => (
              <tr key={row.name} className={isDark ? 'text-slate-300' : 'text-gray-700'}>
                <td className="py-2 font-medium">{row.name}</td>
                <td className={`py-2 ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
                  {row.models.length > 0 ? row.models.join(', ') : '—'}
                </td>
                <td className="py-2 text-right tabular-nums">{row.prompt.toLocaleString()}</td>
                <td className="py-2 text-right tabular-nums">{row.output.toLocaleString()}</td>
                <td className="py-2 text-right tabular-nums font-semibold">{row.total.toLocaleString()}</td>
                <td className="py-2 text-right tabular-nums">{row.turns}</td>
                <td className="py-2 text-right tabular-nums">
                  {row.turns > 0 ? Math.round(row.total / row.turns).toLocaleString() : '—'}
                </td>
                <td className="py-2 text-right tabular-nums">{fmtMs(row.totalGenMs || null)}</td>
                <td className="py-2 text-right tabular-nums">
                  {row.sttAudioS > 0 ? `${Math.round(row.sttAudioS)}s` : '—'}
                  {row.sttSpeechModels.length > 0 && (
                    <span className={`ml-1 text-xs ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>
                      ({row.sttSpeechModels.join(', ')})
                    </span>
                  )}
                </td>
                <td className="py-2 text-right tabular-nums">
                  {row.ttsAudioS > 0 ? `${Math.round(row.ttsAudioS)}s` : '—'}
                  {row.ttsChars > 0 && (
                    <span className={`ml-1 text-xs ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>
                      ({row.ttsChars.toLocaleString()} chars{row.ttsVoices.length > 0 ? `, ${row.ttsVoices.join(', ')}` : ''})
                    </span>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      <TablePager
        page={tokenPaged.page}
        totalPages={tokenPaged.totalPages}
        total={tokenPaged.total}
        start={tokenPaged.start}
        end={tokenPaged.end}
        setPage={tokenPaged.setPage}
        isDark={isDark}
        unit="pipelines"
        className="-mx-5 px-5"
      />
      <p className={`text-xs mt-3 ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>
        Token counts from LLM providers. STT/TTS Audio shows total audio processed (per session, deduplicated).
      </p>
      </>)}
    </div>
  );
}

interface ProviderSpendTableProps {
  isDark: boolean;
  days: number;
  spendCollapsed: boolean;
  toggleSpend: () => void;
  spendPaged: PagedTable<[string, ProviderSpend]>;
}

/** Spend per provider over the window. */
export function ProviderSpendTable({ isDark, days, spendCollapsed, toggleSpend, spendPaged }: ProviderSpendTableProps) {
  return (
    <div className={`rounded-xl p-5 border ${isDark ? 'bg-slate-800 border-slate-700' : 'bg-white border-gray-200'}`}>
      <div className="flex items-center justify-between mb-4 gap-3 flex-wrap">
        <CollapseToggle collapsed={spendCollapsed} onToggle={toggleSpend} isDark={isDark}>
          <h3 className={`text-sm font-semibold ${isDark ? 'text-slate-300' : 'text-gray-700'}`}>
            Provider Spend
          </h3>
        </CollapseToggle>
        <span className={`text-xs ${isDark ? 'text-slate-500' : 'text-gray-500'}`}>
          last {days === 1 ? 'day' : `${days} days`} · USD · prices in <code className="font-mono">backend/app/pricing.yaml</code>
        </span>
      </div>
      {!spendCollapsed && (<>
      <div className="overflow-auto max-h-[28rem]">
        <table className="w-full text-sm">
          <thead className={`sticky top-0 z-10 ${isDark ? 'bg-slate-800' : 'bg-white'}`}>
            <tr className={isDark ? 'text-slate-400' : 'text-gray-500'}>
              <th className="text-left pb-2 font-medium">Provider</th>
              <th className="text-right pb-2 font-medium">LLM</th>
              <th className="text-right pb-2 font-medium">STT</th>
              <th className="text-right pb-2 font-medium">TTS</th>
              <th className="text-right pb-2 font-medium">Total</th>
              <th className="text-right pb-2 font-medium">Monthly Cap</th>
              <th className="text-left pb-2 font-medium pl-3">Quota</th>
            </tr>
          </thead>
          <tbody className={`divide-y ${isDark ? 'divide-slate-700' : 'divide-gray-100'}`}>
            {spendPaged.pageRows
              .map(([name, v]) => {
                const capPct = v.cap_pct;
                let chipColor: string;
                if (capPct == null) chipColor = isDark ? 'bg-slate-700/40 text-slate-400' : 'bg-gray-100 text-gray-500';
                else if (capPct >= 100) chipColor = isDark ? 'bg-rose-900/40 text-rose-300' : 'bg-rose-100 text-rose-700';
                else if (capPct >= 80) chipColor = isDark ? 'bg-amber-900/40 text-amber-300' : 'bg-amber-100 text-amber-700';
                else chipColor = isDark ? 'bg-emerald-900/30 text-emerald-300' : 'bg-emerald-50 text-emerald-700';
                return (
                  <tr key={name} className={isDark ? 'text-slate-300' : 'text-gray-700'}>
                    <td className="py-2 font-medium">{name}</td>
                    <td className="py-2 text-right tabular-nums">{fmtUsd(v.llm)}</td>
                    <td className="py-2 text-right tabular-nums">{fmtUsd(v.stt)}</td>
                    <td className="py-2 text-right tabular-nums">{fmtUsd(v.tts)}</td>
                    <td className="py-2 text-right tabular-nums font-semibold">{fmtUsd(v.total)}</td>
                    <td className="py-2 text-right tabular-nums">
                      {v.monthly_cap_usd != null ? fmtUsd(v.monthly_cap_usd) : <span className={isDark ? 'text-slate-600' : 'text-gray-400'}>—</span>}
                    </td>
                    <td className="py-2 pl-3">
                      <span className={`inline-block px-2 py-0.5 rounded text-xs font-semibold ${chipColor}`}>
                        {capPct != null ? `${capPct.toFixed(1)}%` : 'no cap'}
                      </span>
                    </td>
                  </tr>
                );
              })}
          </tbody>
        </table>
      </div>
      <TablePager
        page={spendPaged.page}
        totalPages={spendPaged.totalPages}
        total={spendPaged.total}
        start={spendPaged.start}
        end={spendPaged.end}
        setPage={spendPaged.setPage}
        isDark={isDark}
        unit="providers"
        className="-mx-5 px-5"
      />
      </>)}
    </div>
  );
}
