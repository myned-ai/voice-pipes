import { TerminalSquare, Volume2 } from 'lucide-react';
import { useTheme } from '../../../contexts/ThemeContext';
import type { ClientToolCallRecord } from './types';

/** One client-side tool call + its resolution, rendered as a compact
 *  card. Used inline in the session-detail turns view (interleaved at
 *  the call's chronological slot via `after_utterance_id`) and by the
 *  aggregate {@link ClientToolsPanel} fallback. */
export function ClientToolCallCard({ call }: { call: ClientToolCallRecord }) {
  const { isDark } = useTheme();

  const statusChip = (status: ClientToolCallRecord['status']) => {
    const styles: Record<string, string> = {
      pending: isDark
        ? 'bg-amber-500/15 text-amber-300 ring-amber-500/30'
        : 'bg-amber-50 text-amber-700 ring-amber-300',
      completed: isDark
        ? 'bg-emerald-500/15 text-emerald-300 ring-emerald-500/30'
        : 'bg-emerald-50 text-emerald-700 ring-emerald-300',
      failed: isDark
        ? 'bg-red-500/15 text-red-300 ring-red-500/30'
        : 'bg-red-50 text-red-700 ring-red-300',
    };
    const title: Record<string, string> = {
      pending: 'The client never reported a terminal result (or the call expired server-side).',
      completed: 'The client reported Completed; the result was injected into the model context.',
      failed: 'The client reported Failed; the failure was injected into the model context.',
    };
    return (
      <span
        title={title[status]}
        className={`px-2 py-0.5 rounded-full text-[10px] font-medium uppercase tracking-wide ring-1 ${styles[status] ?? styles.pending}`}
      >
        {status}
      </span>
    );
  };

  const fmtTime = (ms?: number | null) =>
    typeof ms === 'number' && ms > 0 ? new Date(ms).toLocaleTimeString() : null;

  return (
    <div
      className={`rounded-lg border px-4 py-3 ${
        isDark ? 'bg-indigo-950/30 border-indigo-800/50' : 'bg-indigo-50/60 border-indigo-200'
      }`}
    >
      <div className="flex items-center gap-2 flex-wrap">
        <TerminalSquare size={13} className={isDark ? 'text-indigo-400' : 'text-indigo-500'} />
        <span
          className={`text-[10px] font-bold uppercase tracking-widest ${
            isDark ? 'text-indigo-400' : 'text-indigo-500'
          }`}
        >
          Client Tool
        </span>
        <span className={`font-mono text-sm ${isDark ? 'text-indigo-300' : 'text-indigo-600'}`}>
          {call.name ?? '?'}
        </span>
        <span className={`font-mono text-xs truncate max-w-[50%] ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
          ({JSON.stringify(call.arguments ?? {})})
        </span>
        {call.speak_on_result && (
          <span
            title="Declared with speak_on_result = true: the server speaks a generated confirmation once the call completes."
            className={`inline-flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px] ring-1 ${
              isDark ? 'bg-indigo-500/15 text-indigo-300 ring-indigo-500/30' : 'bg-indigo-50 text-indigo-600 ring-indigo-300'
            }`}
          >
            <Volume2 size={10} /> speaks on result
          </span>
        )}
        <span className="ml-auto flex items-center gap-2">
          {call.status !== 'pending' && call.spoken && (
            <span
              title={
                call.speak_requested
                  ? 'The resolution was spoken because the client explicitly requested it (speak flag on the result event).'
                  : "The resolution was spoken because the tool is declared with speak_on_result = true — the client did NOT request it."
              }
              className={`inline-flex items-center gap-1 text-[10px] ${isDark ? 'text-indigo-300' : 'text-indigo-600'}`}
            >
              <Volume2 size={10} /> spoken · {call.speak_requested ? 'client request' : 'tool policy'}
            </span>
          )}
          {statusChip(call.status)}
        </span>
      </div>
      <div className={`mt-1.5 flex items-center gap-3 flex-wrap font-mono text-[10px] ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>
        <span>call_id: {call.call_id}</span>
        {fmtTime(call.called_at_ms) && <span>called {fmtTime(call.called_at_ms)}</span>}
        {fmtTime(call.resolved_at_ms) && <span>resolved {fmtTime(call.resolved_at_ms)}</span>}
      </div>
      {call.result !== undefined && call.result !== null && (
        <details className={`mt-1.5 text-xs ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
          <summary className="cursor-pointer select-none">result</summary>
          <pre className="mt-1 overflow-x-auto whitespace-pre-wrap font-mono text-[11px]">
            {JSON.stringify(call.result, null, 2)}
          </pre>
        </details>
      )}
    </div>
  );
}

/** Aggregate panel listing all client-side tool activity. Fallback for
 *  views that cannot interleave the cards chronologically (sessions with
 *  no local per-utterance captures). The turns view renders
 *  {@link ClientToolCallCard}s inline instead. */
export function ClientToolsPanel({ calls }: { calls: ClientToolCallRecord[] }) {
  const { isDark } = useTheme();
  if (!calls.length) return null;

  return (
    <div className={`rounded-xl p-6 shadow-xl border ${isDark ? 'bg-slate-800 border-slate-700/50' : 'bg-white border-gray-200'}`}>
      <h3 className="font-bold text-indigo-400 mb-4 flex items-center gap-2">
        <TerminalSquare size={18} /> Client Tools ({calls.length})
      </h3>
      <div className="space-y-3">
        {calls.map((c) => (
          <ClientToolCallCard key={c.call_id} call={c} />
        ))}
      </div>
    </div>
  );
}
