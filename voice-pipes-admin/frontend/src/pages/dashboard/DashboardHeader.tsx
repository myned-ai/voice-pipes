import type React from 'react';
import { BarChart3, Check, SlidersHorizontal, RotateCcw, Trash2 } from 'lucide-react';
import { getAvailableSections } from './sections';

interface DashboardHeaderProps {
  isDark: boolean;
  days: number;
  setDays: React.Dispatch<React.SetStateAction<number>>;
  hiddenSections: Set<string>;
  setHiddenSections: React.Dispatch<React.SetStateAction<Set<string>>>;
  customizeOpen: boolean;
  setCustomizeOpen: React.Dispatch<React.SetStateAction<boolean>>;
  customizeMenuRef: React.RefObject<HTMLDivElement | null>;
  dayOptions: { value: number; label: string }[];
  productionMode?: boolean;
  onOpenClearModal?: () => void;
}

/** Page title, day-range buttons, Clear Metrics action, and the Customize menu that shows or hides sections. */
export function DashboardHeader({
  isDark,
  days,
  setDays,
  hiddenSections,
  setHiddenSections,
  customizeOpen,
  setCustomizeOpen,
  customizeMenuRef,
  dayOptions,
  productionMode = false,
  onOpenClearModal,
}: DashboardHeaderProps) {
  const availableSections = getAvailableSections(productionMode);
  const hiddenCount = availableSections.filter((s) => hiddenSections.has(s.id)).length;

  return (
    <div className="flex items-center justify-between flex-wrap gap-3">
      <h1 className={`text-3xl font-bold flex items-center gap-3 ${isDark ? 'text-white' : 'text-gray-900'}`}>
        <BarChart3 className="text-indigo-400" />
        Dashboard
      </h1>
      <div className="flex items-center gap-3 flex-wrap">
        {/* Day range */}
        <div className="flex gap-2">
        {dayOptions.map((opt) => (
          <button
            key={opt.value}
            onClick={() => setDays(opt.value)}
            className={`px-3 py-1 text-sm rounded-lg border transition-colors ${
              days === opt.value
                ? 'bg-indigo-500/20 border-indigo-500/50 text-indigo-300'
                : isDark
                  ? 'bg-slate-800 border-slate-700 text-slate-400 hover:bg-slate-700'
                  : 'bg-white border-gray-300 text-gray-500 hover:bg-gray-100'
            }`}
          >
            {opt.label}
          </button>
        ))}
        </div>

        {/* Clear Metrics */}
        {onOpenClearModal && (
          <button
            type="button"
            onClick={onOpenClearModal}
            title="Clear dashboard metrics or remove broken test pipelines"
            className={`flex items-center gap-1.5 px-3 py-1 text-sm rounded-lg border transition-colors ${
              isDark
                ? 'bg-slate-800 border-slate-700 text-rose-300 hover:bg-rose-950/40 hover:border-rose-700/60'
                : 'bg-white border-gray-300 text-rose-600 hover:bg-rose-50 hover:border-rose-300'
            }`}
          >
            <Trash2 size={14} />
            <span>Clear Metrics</span>
          </button>
        )}

        {/* Customize: per-operator section visibility (persisted) */}
        <div className="relative" ref={customizeMenuRef}>
          <button
            type="button"
            onClick={() => setCustomizeOpen((o) => !o)}
            title="Show or hide dashboard sections"
            className={`flex items-center gap-2 px-3 py-1 text-sm rounded-lg border transition-colors ${
              hiddenCount > 0
                ? 'bg-indigo-500/20 border-indigo-500/50 text-indigo-300'
                : isDark
                  ? 'bg-slate-800 border-slate-700 text-slate-400 hover:bg-slate-700'
                  : 'bg-white border-gray-300 text-gray-500 hover:bg-gray-100'
            }`}
          >
            <SlidersHorizontal size={14} />
            <span>Customize</span>
            {hiddenCount > 0 && (
              <span className={`text-[11px] px-1.5 rounded-full ${isDark ? 'bg-indigo-500/30 text-indigo-200' : 'bg-indigo-100 text-indigo-700'}`}>
                {availableSections.length - hiddenCount}/{availableSections.length}
              </span>
            )}
          </button>
          {customizeOpen && (
            <div
              className={`absolute right-0 mt-1 z-30 w-72 max-h-[70vh] overflow-y-auto rounded-lg border shadow-xl ${
                isDark ? 'bg-slate-800 border-slate-700' : 'bg-white border-gray-200'
              }`}
            >
              <div className={`flex items-center justify-between px-3 py-2 text-xs border-b ${isDark ? 'border-slate-700 text-slate-400' : 'border-gray-200 text-gray-500'}`}>
                <span className="font-semibold uppercase tracking-wide">Sections</span>
                {hiddenCount > 0 && (
                  <button
                    type="button"
                    onClick={() => setHiddenSections(new Set())}
                    className={`flex items-center gap-1 font-medium ${isDark ? 'text-indigo-400 hover:text-indigo-300' : 'text-indigo-600 hover:text-indigo-700'}`}
                  >
                    <RotateCcw size={12} /> Show all
                  </button>
                )}
              </div>
              <ul className="py-1">
                {availableSections.map((section, i) => {
                  const prev = availableSections[i - 1];
                  const showGroup = !prev || prev.group !== section.group;
                  const shown = !hiddenSections.has(section.id);
                  return (
                    <li key={section.id}>
                      {showGroup && (
                        <div className={`px-3 pt-2 pb-1 text-[10px] font-semibold uppercase tracking-wider ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>
                          {section.group}
                        </div>
                      )}
                      <button
                        type="button"
                        onClick={() =>
                          setHiddenSections((prev2) => {
                            const next = new Set(prev2);
                            if (next.has(section.id)) next.delete(section.id);
                            else next.add(section.id);
                            return next;
                          })
                        }
                        className={`w-full flex items-center gap-2 px-3 py-1.5 text-sm text-left transition-colors ${
                          isDark ? 'hover:bg-slate-700 text-slate-200' : 'hover:bg-gray-100 text-gray-700'
                        }`}
                      >
                        <span
                          className={`w-4 h-4 rounded border flex items-center justify-center shrink-0 ${
                            shown
                              ? 'bg-indigo-500 border-indigo-500'
                              : isDark
                                ? 'border-slate-500 bg-slate-900'
                                : 'border-gray-300 bg-white'
                          }`}
                        >
                          {shown && <Check size={12} className="text-white" strokeWidth={3} />}
                        </span>
                        <span className={shown ? '' : isDark ? 'text-slate-500' : 'text-gray-400'}>{section.label}</span>
                      </button>
                    </li>
                  );
                })}
              </ul>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
