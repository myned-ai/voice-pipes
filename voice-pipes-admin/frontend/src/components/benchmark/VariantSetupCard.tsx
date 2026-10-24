import { FileText, Hash, Tag } from 'lucide-react';
import { useTheme } from '../../contexts/ThemeContext';
import type { Variant } from './types';

interface Props {
  variant: Variant;
  onViewConfig?: () => void;
}

export default function VariantSetupCard({ variant, onViewConfig }: Props) {
  const { isDark } = useTheme();

  const truncatedHash = variant.config_hash.length > 8
    ? variant.config_hash.slice(0, 8)
    : variant.config_hash;

  return (
    <div className={`rounded-xl border p-5 ${isDark ? 'bg-slate-800 border-slate-700' : 'bg-white border-gray-200'}`}>
      <h4 className={`text-base font-bold mb-3 ${isDark ? 'text-white' : 'text-gray-900'}`}>
        {variant.name}
      </h4>

      <div className="grid gap-2.5 mb-3">
        <div className="flex items-center gap-2">
          <FileText size={13} className={isDark ? 'text-slate-500' : 'text-gray-400'} />
          <span className={`text-xs font-medium w-16 shrink-0 ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
            Config
          </span>
          <span className={`text-xs font-mono ${isDark ? 'text-slate-300' : 'text-gray-700'}`}>
            {variant.config_path}
          </span>
        </div>
        <div className="flex items-center gap-2">
          <Hash size={13} className={isDark ? 'text-slate-500' : 'text-gray-400'} />
          <span className={`text-xs font-medium w-16 shrink-0 ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
            Hash
          </span>
          <span className={`text-xs font-mono ${isDark ? 'text-slate-300' : 'text-gray-700'}`} title={variant.config_hash}>
            {truncatedHash}
          </span>
        </div>
      </div>

      {/* Tags */}
      {variant.tags.length > 0 && (
        <div className="flex items-center gap-2 mb-3">
          <Tag size={13} className={isDark ? 'text-slate-500' : 'text-gray-400'} />
          <div className="flex flex-wrap gap-1">
            {variant.tags.map(tag => (
              <span
                key={tag}
                className={`inline-flex items-center px-2 py-0.5 rounded-full text-[10px] font-medium ${
                  isDark ? 'bg-slate-700 text-slate-300' : 'bg-gray-100 text-gray-600'
                }`}
              >
                {tag}
              </span>
            ))}
          </div>
        </div>
      )}

      {/* View Config button */}
      {onViewConfig && (
        <button
          onClick={onViewConfig}
          className={`flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-xs font-medium transition-colors ${
            isDark
              ? 'bg-indigo-500/15 text-indigo-300 hover:bg-indigo-500/25'
              : 'bg-indigo-50 text-indigo-600 hover:bg-indigo-100'
          }`}
        >
          <FileText size={12} />
          View Config
        </button>
      )}
    </div>
  );
}
