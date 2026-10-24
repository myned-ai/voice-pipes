import { ReferenceLine } from 'recharts';
import { fmtMs, fmtNum } from './format';
import type { ProviderChange } from './types';

const FIELD_COLORS: Record<string, string> = {
  stt_provider: '#3b82f6',   // blue
  tts_provider: '#10b981',   // green
  pipeline_name: '#a78bfa',  // purple
};

/* ── provider-change reference lines ────────────────────── */

export function ChangeLines({ changes, dataKey }: { changes: ProviderChange[]; dataKey: string }) {
  return (
    <>
      {changes.map((c, i) => {
        const x = new Date(c.timestamp).getTime();
        const color = FIELD_COLORS[c.field] ?? '#94a3b8';
        return (
          <ReferenceLine
            key={`${dataKey}-${i}`}
            x={x}
            stroke={color}
            strokeDasharray="4 3"
            strokeWidth={1.5}
            label={{
              value: `${i + 1}`,
              position: 'top',
              fill: color,
              fontSize: 10,
              fontWeight: 700,
            }}
          />
        );
      })}
    </>
  );
}

/* ── custom tooltip ─────────────────────────────────────── */

/** The per-point datum recharts hands back — the subset the tooltips read. */
export interface TooltipDatum {
  _timeLabel?: string;
  pipeline_name?: string | null;
  model?: string | null;
  stt_provider?: string | null;
  stt_speech_model?: string | null;
  tts_provider?: string | null;
  tts_voice?: string | null;
  session_id?: string;
}
export interface TooltipEntry {
  dataKey?: string | number;
  name?: string | number;
  value?: number | string | null;
  color?: string;
  payload?: TooltipDatum;
}
/** recharts injects `active`/`payload` when it clones the `content` element. */
export interface TooltipProps {
  active?: boolean;
  payload?: TooltipEntry[];
  isDark: boolean;
}
const numericValue = (v: TooltipEntry["value"]): number | null => (typeof v === 'number' ? v : null);

export function ChartTooltip({ active, payload, isDark }: TooltipProps) {
  if (!active || !payload?.length) return null;
  const d = payload[0]?.payload;
  return (
    <div className={`rounded-lg p-3 text-xs shadow-lg border ${isDark ? 'bg-slate-800 border-slate-700 text-slate-200' : 'bg-white border-gray-200 text-gray-800'}`}>
      <p className="font-semibold mb-1">{d?._timeLabel}</p>
      {d?.pipeline_name && (
        <p className={`mb-1 ${isDark ? 'text-indigo-400' : 'text-indigo-600'}`}>Pipeline: {d.pipeline_name}</p>
      )}
      {d?.model && (
        <p className={`mb-1 ${isDark ? 'text-sky-400' : 'text-sky-600'}`}>Model: {d.model}</p>
      )}
      {(d?.stt_provider || d?.tts_provider) && (
        <p className={`mb-1 ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
          {d.stt_provider && `STT: ${d.stt_provider}`}
          {d.stt_speech_model && ` (${d.stt_speech_model})`}
          {d.stt_provider && d.tts_provider && ' · '}
          {d.tts_provider && `TTS: ${d.tts_provider}`}
          {d.tts_voice && ` (${d.tts_voice})`}
        </p>
      )}
      {payload.map((p) => (
        <p key={String(p.dataKey)} style={{ color: p.color }}>
          {p.name}: {fmtMs(numericValue(p.value))}
        </p>
      ))}
      {d?.session_id && (
        <p className={`mt-1 ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>
          Session: {d.session_id.slice(0, 8)}…
        </p>
      )}
    </div>
  );
}

export function LlmTooltip({ active, payload, isDark }: TooltipProps) {
  if (!active || !payload?.length) return null;
  const d = payload[0]?.payload;
  return (
    <div className={`rounded-lg p-3 text-xs shadow-lg border ${isDark ? 'bg-slate-800 border-slate-700 text-slate-200' : 'bg-white border-gray-200 text-gray-800'}`}>
      <p className="font-semibold mb-1">{d?._timeLabel}</p>
      {d?.pipeline_name && (
        <p className={`mb-1 ${isDark ? 'text-indigo-400' : 'text-indigo-600'}`}>Pipeline: {d.pipeline_name}</p>
      )}
      {d?.model && (
        <p className={`mb-1 ${isDark ? 'text-sky-400' : 'text-sky-600'}`}>Model: {d.model}</p>
      )}
      {payload.map((p) => (
        <p key={String(p.dataKey)} style={{ color: p.color }}>
          {p.name}: {p.dataKey === 'tokens_per_sec' ? fmtNum(numericValue(p.value)) + ' tok/s' : fmtMs(numericValue(p.value))}
        </p>
      ))}
    </div>
  );
}
