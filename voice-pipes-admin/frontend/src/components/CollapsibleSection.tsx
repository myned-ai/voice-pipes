import { useState } from 'react';
import { ChevronDown } from 'lucide-react';
import { useTheme } from '../contexts/ThemeContext';

export function CollapsibleSection({ title, defaultOpen = true, children }: { title: string; defaultOpen?: boolean; children: React.ReactNode }) {
  const { isDark } = useTheme();
  const [open, setOpen] = useState(defaultOpen);
  return (
    <div className={`rounded-xl border ${isDark ? 'bg-slate-900 border-slate-800' : 'bg-white border-gray-200'}`}>
      <button
        onClick={() => setOpen(o => !o)}
        className={`w-full flex items-center justify-between px-4 py-3 text-left transition-colors rounded-xl ${
          open ? 'rounded-b-none' : ''
        } ${isDark ? 'hover:bg-slate-800/60' : 'hover:bg-gray-50'}`}
      >
        <span className={`text-xs uppercase tracking-widest font-bold ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>{title}</span>
        <ChevronDown
          size={14}
          className={`transition-transform duration-200 ${open ? '' : '-rotate-90'} ${isDark ? 'text-slate-500' : 'text-gray-400'}`}
        />
      </button>
      {open && (
        <div className={`px-4 pb-4 border-t ${isDark ? 'border-slate-800' : 'border-gray-100'}`}>
          <div className="pt-3">{children}</div>
        </div>
      )}
    </div>
  );
}
