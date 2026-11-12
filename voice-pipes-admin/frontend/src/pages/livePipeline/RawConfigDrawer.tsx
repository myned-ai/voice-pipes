import { useState } from 'react';
import { X, FileCode, Copy, Check } from 'lucide-react';
import { useTheme } from '../../contexts/ThemeContext';

interface Props {
  open: boolean;
  onClose: () => void;
  pipelineName: string;
  workerId: string;
  tomlContent: string | null;
}

export function RawConfigDrawer({ open, onClose, pipelineName, workerId, tomlContent }: Props) {
  const { isDark } = useTheme();
  const [copied, setCopied] = useState(false);

  if (!open) return null;

  const handleCopy = () => {
    if (!tomlContent) return;
    navigator.clipboard.writeText(tomlContent).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    });
  };

  const lines = tomlContent ? tomlContent.split('\n') : [];

  return (
    <>
      {/* Backdrop */}
      <div 
        className="fixed inset-0 bg-black/40 z-40 backdrop-blur-xs transition-opacity" 
        onClick={onClose} 
      />

      {/* Drawer */}
      <div 
        className={`fixed top-0 right-0 h-full w-[650px] max-w-[92vw] z-50 shadow-2xl flex flex-col transition-transform ${
          isDark ? 'bg-slate-900 border-l border-slate-700 text-slate-100' : 'bg-white border-l border-gray-200 text-gray-900'
        }`}
      >
        {/* Header */}
        <div className={`flex items-center justify-between px-6 py-4 border-b ${isDark ? 'border-slate-800 bg-slate-900/80' : 'border-gray-200 bg-gray-50/80'}`}>
          <div className="flex items-center gap-3">
            <div className={`p-2 rounded-lg ${isDark ? 'bg-indigo-950/60 text-indigo-400' : 'bg-indigo-50 text-indigo-600'}`}>
              <FileCode size={18} />
            </div>
            <div>
              <div className="flex items-center gap-2">
                <h3 className="text-sm font-semibold tracking-tight">Raw Pipeline Config</h3>
                <span className={`text-xs px-2 py-0.5 rounded-full font-mono font-medium ${
                  isDark ? 'bg-slate-800 text-slate-300' : 'bg-gray-200 text-gray-700'
                }`}>
                  TOML
                </span>
              </div>
              <p className={`text-xs mt-0.5 ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
                {pipelineName} {workerId ? `• worker: ${workerId}` : ''}
              </p>
            </div>
          </div>

          <div className="flex items-center gap-2">
            {tomlContent && (
              <button
                onClick={handleCopy}
                className={`flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-xs font-medium transition-colors ${
                  copied
                    ? 'bg-emerald-600 text-white'
                    : isDark
                    ? 'bg-slate-800 hover:bg-slate-700 text-slate-200 border border-slate-700'
                    : 'bg-white hover:bg-gray-100 text-gray-700 border border-gray-300 shadow-xs'
                }`}
                title="Copy TOML to clipboard"
              >
                {copied ? <Check size={14} /> : <Copy size={14} />}
                {copied ? 'Copied!' : 'Copy'}
              </button>
            )}
            <button
              onClick={onClose}
              className={`p-1.5 rounded-lg transition-colors ${
                isDark ? 'hover:bg-slate-800 text-slate-400 hover:text-white' : 'hover:bg-gray-100 text-gray-500 hover:text-gray-900'
              }`}
              title="Close drawer"
            >
              <X size={18} />
            </button>
          </div>
        </div>

        {/* Content Body */}
        <div className="flex-1 overflow-y-auto p-6 font-mono text-xs">
          {tomlContent ? (
            <div className={`rounded-lg border overflow-x-auto ${
              isDark ? 'bg-slate-950 border-slate-800' : 'bg-slate-50 border-gray-200'
            }`}>
              <table className="w-full border-collapse">
                <tbody>
                  {lines.map((line, idx) => (
                    <tr key={idx} className={isDark ? 'hover:bg-slate-900/50' : 'hover:bg-gray-100/60'}>
                      <td className={`py-0.5 px-3 text-right select-none w-10 border-r ${
                        isDark ? 'text-slate-600 border-slate-800/80' : 'text-gray-400 border-gray-200'
                      }`}>
                        {idx + 1}
                      </td>
                      <td className={`py-0.5 px-4 whitespace-pre ${
                        line.trim().startsWith('#')
                          ? isDark ? 'text-slate-500 italic' : 'text-gray-400 italic'
                          : line.trim().startsWith('[')
                          ? isDark ? 'text-cyan-400 font-semibold' : 'text-indigo-600 font-semibold'
                          : line.includes('=')
                          ? isDark ? 'text-slate-300' : 'text-gray-800'
                          : isDark ? 'text-slate-400' : 'text-gray-600'
                      }`}>
                        {line || ' '}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          ) : (
            <div className="flex flex-col items-center justify-center h-full text-center py-16">
              <FileCode size={32} className={`mb-3 ${isDark ? 'text-slate-600' : 'text-gray-300'}`} />
              <p className={`text-sm font-medium ${isDark ? 'text-slate-400' : 'text-gray-600'}`}>
                No raw configuration available
              </p>
              <p className={`text-xs mt-1 ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>
                Select an active worker to inspect its running pipeline configuration.
              </p>
            </div>
          )}
        </div>
      </div>
    </>
  );
}
