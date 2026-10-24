import { useState } from 'react';
import { List, ChevronsDown, ChevronsUp } from 'lucide-react';
import { useTheme } from '../../contexts/ThemeContext';
import type { TestScenario } from './types';
import ScenarioRow from './ScenarioRow';

interface Props {
  scenarios: TestScenario[];
}

export default function ScenariosTable({ scenarios }: Props) {
  const { isDark } = useTheme();
  const [allExpanded, setAllExpanded] = useState(false);
  // Force re-render of ScenarioRow components with new defaultExpanded
  const [expandKey, setExpandKey] = useState(0);

  const toggleAll = () => {
    setAllExpanded(!allExpanded);
    setExpandKey(k => k + 1);
  };

  if (scenarios.length === 0) {
    return (
      <div className={`rounded-xl border p-5 text-center ${isDark ? 'bg-slate-800 border-slate-700 text-slate-500' : 'bg-white border-gray-200 text-gray-400'}`}>
        No scenarios in this suite
      </div>
    );
  }

  return (
    <div className={`rounded-xl border overflow-hidden ${isDark ? 'bg-slate-800 border-slate-700' : 'bg-white border-gray-200'}`}>
      <div className={`flex items-center justify-between px-5 py-3 border-b ${isDark ? 'border-slate-700' : 'border-gray-200'}`}>
        <h3 className={`text-sm font-semibold flex items-center gap-2 ${isDark ? 'text-white' : 'text-gray-900'}`}>
          <List size={16} className={isDark ? 'text-slate-400' : 'text-gray-500'} />
          Test Scenarios
          <span className={`text-xs font-normal ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>
            ({scenarios.length})
          </span>
        </h3>
        <button
          onClick={toggleAll}
          className={`flex items-center gap-1 text-xs font-medium transition-colors ${
            isDark ? 'text-slate-400 hover:text-white' : 'text-gray-500 hover:text-gray-900'
          }`}
        >
          {allExpanded ? <ChevronsUp size={14} /> : <ChevronsDown size={14} />}
          {allExpanded ? 'Collapse All' : 'Expand All'}
        </button>
      </div>

      {/* Header row */}
      <div className={`flex items-center gap-3 px-4 py-2 text-xs font-medium uppercase tracking-wide border-b ${
        isDark ? 'border-slate-700 text-slate-500 bg-slate-900/30' : 'border-gray-200 text-gray-400 bg-gray-50'
      }`}>
        <span className="w-4" /> {/* chevron space */}
        <span className="flex-1">Name</span>
        <span className="w-16 text-center">Input</span>
        <span className="w-[200px]">Expected</span>
      </div>

      {scenarios.map(s => (
        <ScenarioRow key={`${s.id}-${expandKey}`} scenario={s} defaultExpanded={allExpanded} />
      ))}
    </div>
  );
}
