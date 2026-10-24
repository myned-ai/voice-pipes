import type React from 'react';
import { ChevronDown } from 'lucide-react';

export function CollapseToggle({
  collapsed,
  onToggle,
  isDark,
  children,
}: {
  collapsed: boolean;
  onToggle: () => void;
  isDark: boolean;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onToggle}
      aria-expanded={!collapsed}
      className={`flex items-center gap-2 min-w-0 text-left ${isDark ? 'hover:text-white' : 'hover:text-gray-900'} transition-colors`}
    >
      <ChevronDown
        size={16}
        className={`shrink-0 transition-transform duration-150 ${collapsed ? '-rotate-90' : ''} ${isDark ? 'text-slate-400' : 'text-gray-400'}`}
      />
      {children}
    </button>
  );
}

export function TablePager({
  page,
  totalPages,
  total,
  start,
  end,
  setPage,
  isDark,
  unit,
  className,
}: {
  page: number;
  totalPages: number;
  total: number;
  start: number;
  end: number;
  setPage: (p: number) => void;
  isDark: boolean;
  unit?: string;
  className?: string;
}) {
  if (totalPages <= 1) return null;
  const btn = `px-2 py-1 rounded border text-xs transition-colors disabled:opacity-40 disabled:cursor-not-allowed ${
    isDark ? 'border-slate-700 hover:bg-slate-700/50 text-slate-300' : 'border-gray-200 hover:bg-gray-100 text-gray-600'
  }`;
  return (
    <div
      className={`flex items-center justify-between px-5 py-2 text-xs border-t ${
        isDark ? 'text-slate-400 border-slate-700/60' : 'text-gray-500 border-gray-100'
      } ${className ?? ''}`}
    >
      <span className="tabular-nums">
        {total === 0 ? 'No rows' : `${start + 1}–${end} of ${total}${unit ? ` ${unit}` : ''}`}
      </span>
      <div className="flex items-center gap-2">
        <button type="button" className={btn} disabled={page <= 0} onClick={() => setPage(page - 1)}>
          Prev
        </button>
        <span className="tabular-nums">
          {page + 1} / {totalPages}
        </span>
        <button type="button" className={btn} disabled={page >= totalPages - 1} onClick={() => setPage(page + 1)}>
          Next
        </button>
      </div>
    </div>
  );
}
