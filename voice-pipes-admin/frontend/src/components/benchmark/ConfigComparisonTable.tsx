import { Settings2 } from 'lucide-react';
import { useTheme } from '../../contexts/ThemeContext';
import type { VariantResults, PipelineConfigSummary } from './types';

interface Props {
  variantResults: VariantResults[];
  onViewRawConfig?: (experimentId: string, variantId: string) => void;
}

type ConfigKey = keyof PipelineConfigSummary;

const CONFIG_ROWS: { key: ConfigKey; label: string; group: string }[] = [
  { key: 'pipeline_name', label: 'Pipeline', group: 'General' },
  { key: 'vad_mode', label: 'VAD Mode', group: 'Audio' },
  { key: 'stt_provider', label: 'STT Provider', group: 'STT' },
  { key: 'stt_model', label: 'STT Model', group: 'STT' },
  { key: 'llm_model', label: 'LLM Model', group: 'LLM' },
  { key: 'llm_temperature', label: 'Temperature', group: 'LLM' },
  { key: 'llm_max_tokens', label: 'Max Tokens', group: 'LLM' },
  { key: 'tts_provider', label: 'TTS Provider', group: 'TTS' },
  { key: 'tts_model', label: 'TTS Model', group: 'TTS' },
  { key: 'turn_detection_mode', label: 'Turn Detection', group: 'Turn' },
  { key: 'silence_timeout_ms', label: 'Silence Timeout', group: 'Turn' },
  { key: 'barge_in_enabled', label: 'Barge-in', group: 'Turn' },
];

function formatValue(val: unknown): string {
  if (val == null) return '—';
  if (typeof val === 'boolean') return val ? 'Yes' : 'No';
  if (typeof val === 'number') return String(val);
  return String(val);
}

export default function ConfigComparisonTable({ variantResults }: Props) {
  const { isDark } = useTheme();
  const summaries = variantResults.map(vr => vr.config_summary);

  // Skip if no summaries at all
  if (summaries.every(s => !s)) return null;

  // Only show rows where at least one variant has a value
  const visibleRows = CONFIG_ROWS.filter(row =>
    summaries.some(s => s && s[row.key] != null)
  );

  if (visibleRows.length === 0) return null;

  // Check which rows have differing values across variants
  const isDiff = (key: ConfigKey): boolean => {
    const vals = summaries.map(s => s ? formatValue(s[key]) : '—');
    return new Set(vals).size > 1;
  };

  return (
    <div className={`rounded-xl border overflow-hidden ${isDark ? 'bg-slate-800 border-slate-700' : 'bg-white border-gray-200'}`}>
      <div className={`flex items-center gap-2 px-5 py-4 border-b ${isDark ? 'border-slate-700' : 'border-gray-200'}`}>
        <Settings2 size={16} className={isDark ? 'text-slate-400' : 'text-gray-500'} />
        <h3 className={`text-sm font-semibold ${isDark ? 'text-white' : 'text-gray-900'}`}>
          Pipeline Configuration Comparison
        </h3>
      </div>

      <div className="overflow-x-auto">
        <table className="w-full text-sm">
          <thead>
            <tr className={isDark ? 'bg-slate-900/50' : 'bg-gray-50'}>
              <th className={`text-left px-4 py-2 font-medium ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>Setting</th>
              {variantResults.map(vr => (
                <th key={vr.variant_id} className={`text-left px-4 py-2 font-medium ${isDark ? 'text-slate-300' : 'text-gray-700'}`}>
                  {vr.variant_name}
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {visibleRows.map((row, i) => {
              // Label the group on its first visible row only.
              const showGroupLabel = i === 0 || visibleRows[i - 1].group !== row.group;
              const hasDiff = isDiff(row.key);

              return (
                <tr
                  key={row.key}
                  className={`border-t ${isDark ? 'border-slate-700/50' : 'border-gray-100'} ${
                    hasDiff ? (isDark ? 'bg-amber-500/5' : 'bg-amber-50/50') : ''
                  }`}
                >
                  <td className={`px-4 py-2 ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
                    {showGroupLabel && (
                      <span className={`text-[10px] uppercase tracking-wider font-semibold block -mb-0.5 ${isDark ? 'text-slate-600' : 'text-gray-300'}`}>
                        {row.group}
                      </span>
                    )}
                    {row.label}
                  </td>
                  {summaries.map((s, i) => {
                    const val = s ? formatValue(s[row.key]) : '—';
                    return (
                      <td
                        key={variantResults[i].variant_id}
                        className={`px-4 py-2 font-mono text-xs ${
                          hasDiff
                            ? isDark ? 'text-amber-300 font-semibold' : 'text-amber-700 font-semibold'
                            : isDark ? 'text-slate-300' : 'text-gray-700'
                        }`}
                      >
                        {val}
                      </td>
                    );
                  })}
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>
    </div>
  );
}
