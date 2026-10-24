import { useEffect, useState } from 'react';
import { X, FileText, Loader2 } from 'lucide-react';
import axios from 'axios';
import { useTheme } from '../../contexts/ThemeContext';
import { API_BASE } from '../../config/api';

interface Props {
  experimentId: string;
  variantId: string;
  variantName: string;
  open: boolean;
  onClose: () => void;
}

/** Outcome of fetching one variant's config snapshot. */
interface SnapshotResult {
  /** `${experimentId}/${variantId}` this result belongs to. */
  key: string;
  content: string | null;
  error: string | null;
}

export default function ConfigDrawer({ experimentId, variantId, variantName, open, onClose }: Props) {
  const { isDark } = useTheme();
  const key = open ? `${experimentId}/${variantId}` : null;
  const [result, setResult] = useState<SnapshotResult | null>(null);

  useEffect(() => {
    if (!open) return;
    const target = `${experimentId}/${variantId}`;
    let cancelled = false;
    axios.get<string>(`${API_BASE}/api/v1/benchmark/experiments/${encodeURIComponent(experimentId)}/variants/${encodeURIComponent(variantId)}/config`, {
      responseType: 'text',
    })
      .then(res => { if (!cancelled) setResult({ key: target, content: res.data, error: null }); })
      .catch(() => { if (!cancelled) setResult({ key: target, content: null, error: 'Config snapshot not available.' }); });
    return () => { cancelled = true; };
  }, [open, experimentId, variantId]);

  if (!open) return null;

  // Only show a result that belongs to the variant currently open; until it
  // lands we are loading. Derived, so no loading flag has to be set in the effect.
  const current = result?.key === key ? result : null;
  const loading = current == null;
  const content = current?.content ?? null;
  const error = current?.error ?? null;

  return (
    <>
      {/* Backdrop */}
      <div className="fixed inset-0 bg-black/40 z-40" onClick={onClose} />

      {/* Drawer */}
      <div className={`fixed top-0 right-0 h-full w-[600px] max-w-[90vw] z-50 shadow-2xl flex flex-col ${
        isDark ? 'bg-slate-900 border-l border-slate-700' : 'bg-white border-l border-gray-200'
      }`}>
        {/* Header */}
        <div className={`flex items-center justify-between px-6 py-4 border-b ${isDark ? 'border-slate-700' : 'border-gray-200'}`}>
          <div className="flex items-center gap-2">
            <FileText size={16} className={isDark ? 'text-slate-400' : 'text-gray-500'} />
            <span className={`text-sm font-semibold ${isDark ? 'text-white' : 'text-gray-900'}`}>
              {variantName} — Full Config
            </span>
          </div>
          <button onClick={onClose} className={`p-1 rounded hover:bg-slate-700/50 ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
            <X size={18} />
          </button>
        </div>

        {/* Body */}
        <div className="flex-1 overflow-y-auto p-6">
          {loading && (
            <div className="flex items-center justify-center py-20">
              <Loader2 size={20} className="animate-spin text-indigo-400" />
            </div>
          )}
          {error && (
            <p className={`text-sm ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>{error}</p>
          )}
          {content && (
            <pre className={`text-xs font-mono leading-relaxed whitespace-pre-wrap break-words ${
              isDark ? 'text-slate-300' : 'text-gray-700'
            }`}>{content}</pre>
          )}
        </div>
      </div>
    </>
  );
}
