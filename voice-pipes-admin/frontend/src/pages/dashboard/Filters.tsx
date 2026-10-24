import type React from 'react';
import { ChevronDown, Check } from 'lucide-react';
import { pipelineColor } from '../../lib/pipelineColor';
import type { DashboardMetrics } from './useDashboardMetrics';

interface PipelineFilterProps {
  isDark: boolean;
  selectedPipelines: Set<string>;
  setSelectedPipelines: React.Dispatch<React.SetStateAction<Set<string>>>;
  pipelineMenuOpen: boolean;
  setPipelineMenuOpen: React.Dispatch<React.SetStateAction<boolean>>;
  pipelineMenuRef: React.RefObject<HTMLDivElement | null>;
  pipelineNames: DashboardMetrics['pipelineNames'];
}

/** Pipeline multi-select filter. */
export function PipelineFilter({ isDark, selectedPipelines, setSelectedPipelines, pipelineMenuOpen, setPipelineMenuOpen, pipelineMenuRef, pipelineNames }: PipelineFilterProps) {
  return (
    <div className={`rounded-xl p-4 border ${isDark ? 'bg-slate-800/50 border-slate-700' : 'bg-white border-gray-200'}`}>
      <div className="flex items-center justify-between gap-4 flex-wrap">
        <div>
          <h3 className={`text-sm font-semibold ${isDark ? 'text-slate-300' : 'text-gray-700'}`}>
            Pipelines
          </h3>
          <p className={`text-xs mt-0.5 ${isDark ? 'text-slate-500' : 'text-gray-500'}`}>
            Select one or more pipelines to compare metrics. Empty selection shows all.
          </p>
        </div>
        <div className="relative" ref={pipelineMenuRef}>
          <button
            type="button"
            onClick={() => setPipelineMenuOpen((o) => !o)}
            className={`flex items-center gap-2 px-3 py-1.5 text-sm rounded-lg border transition-colors cursor-pointer min-w-[220px] justify-between ${
              isDark
                ? 'bg-slate-900 border-slate-700 text-slate-300 hover:bg-slate-800'
                : 'bg-gray-50 border-gray-300 text-gray-700 hover:bg-gray-100'
            }`}
          >
            <span className="truncate">
              {selectedPipelines.size === 0
                ? `All Pipelines (${pipelineNames.length})`
                : selectedPipelines.size === 1
                  ? Array.from(selectedPipelines)[0]
                  : `${selectedPipelines.size} of ${pipelineNames.length} selected`}
            </span>
            <ChevronDown size={14} className={`shrink-0 transition-transform ${pipelineMenuOpen ? 'rotate-180' : ''}`} />
          </button>
          {pipelineMenuOpen && (
            <div
              className={`absolute right-0 mt-1 z-20 min-w-[260px] max-h-80 overflow-y-auto rounded-lg border shadow-lg ${
                isDark
                  ? 'bg-slate-800 border-slate-700'
                  : 'bg-white border-gray-200'
              }`}
            >
              <div className={`flex items-center justify-between px-3 py-2 text-xs border-b ${
                isDark ? 'border-slate-700 text-slate-400' : 'border-gray-200 text-gray-500'
              }`}>
                <button
                  type="button"
                  onClick={() => setSelectedPipelines(new Set(pipelineNames))}
                  className={`text-xs font-medium ${isDark ? 'text-indigo-400 hover:text-indigo-300' : 'text-indigo-600 hover:text-indigo-700'}`}
                >
                  Select all
                </button>
                {selectedPipelines.size > 0 && (
                  <button
                    type="button"
                    onClick={() => setSelectedPipelines(new Set())}
                    className={`text-xs font-medium ${isDark ? 'text-indigo-400 hover:text-indigo-300' : 'text-indigo-600 hover:text-indigo-700'}`}
                  >
                    Clear
                  </button>
                )}
              </div>
              <ul className="py-1">
                {pipelineNames.map((name) => {
                  const checked = selectedPipelines.has(name);
                  return (
                    <li key={name}>
                      <button
                        type="button"
                        onClick={() =>
                          setSelectedPipelines((prev) => {
                            const next = new Set(prev);
                            if (next.has(name)) next.delete(name);
                            else next.add(name);
                            return next;
                          })
                        }
                        className={`w-full flex items-center gap-2 px-3 py-1.5 text-sm text-left transition-colors ${
                          isDark
                            ? 'hover:bg-slate-700 text-slate-200'
                            : 'hover:bg-gray-100 text-gray-700'
                        }`}
                      >
                        <span
                          className={`w-4 h-4 rounded border flex items-center justify-center shrink-0 ${
                            checked
                              ? 'bg-indigo-500 border-indigo-500'
                              : isDark
                                ? 'border-slate-500 bg-slate-900'
                                : 'border-gray-300 bg-white'
                          }`}
                        >
                          {checked && <Check size={12} className="text-white" strokeWidth={3} />}
                        </span>
                        <span
                          className="inline-block w-2.5 h-2.5 rounded-full shrink-0"
                          style={{ background: pipelineColor(name) }}
                        />
                        <span className="truncate">{name}</span>
                      </button>
                    </li>
                  );
                })}
              </ul>
            </div>
          )}
        </div>
      </div>
      {selectedPipelines.size > 0 && (
        <div className="flex flex-wrap gap-1.5 mt-3">
          {Array.from(selectedPipelines).map((name) => (
            <span
              key={name}
              className={`inline-flex items-center gap-1.5 text-xs px-2 py-1 rounded-md border ${
                isDark
                  ? 'bg-indigo-500/10 border-indigo-500/30 text-indigo-300'
                  : 'bg-indigo-50 border-indigo-200 text-indigo-700'
              }`}
            >
              <span
                className="inline-block w-2 h-2 rounded-full"
                style={{ background: pipelineColor(name) }}
              />
              {name}
              <button
                type="button"
                aria-label={`Remove ${name}`}
                onClick={() =>
                  setSelectedPipelines((prev) => {
                    const next = new Set(prev);
                    next.delete(name);
                    return next;
                  })
                }
                className={`leading-none ${isDark ? 'hover:text-white' : 'hover:text-indigo-900'}`}
              >
                ×
              </button>
            </span>
          ))}
        </div>
      )}
    </div>
  );
}

interface ProviderChipFiltersProps {
  isDark: boolean;
  selectedSttProviders: Set<string>;
  setSelectedSttProviders: React.Dispatch<React.SetStateAction<Set<string>>>;
  selectedTtsProviders: Set<string>;
  setSelectedTtsProviders: React.Dispatch<React.SetStateAction<Set<string>>>;
  selectedLlmModels: Set<string>;
  setSelectedLlmModels: React.Dispatch<React.SetStateAction<Set<string>>>;
  sttProviderOptions: DashboardMetrics['sttProviderOptions'];
  ttsProviderOptions: DashboardMetrics['ttsProviderOptions'];
  llmModelOptions: DashboardMetrics['llmModelOptions'];
}

/** STT provider, TTS provider and LLM model chip filters. */
export function ProviderChipFilters({ isDark, selectedSttProviders, setSelectedSttProviders, selectedTtsProviders, setSelectedTtsProviders, selectedLlmModels, setSelectedLlmModels, sttProviderOptions, ttsProviderOptions, llmModelOptions }: ProviderChipFiltersProps) {
  return (
    <div className={`rounded-xl p-4 border space-y-3 ${isDark ? 'bg-slate-800/50 border-slate-700' : 'bg-white border-gray-200'}`}>
      {([
        ['STT', sttProviderOptions, selectedSttProviders, setSelectedSttProviders] as const,
        ['LLM', llmModelOptions, selectedLlmModels, setSelectedLlmModels] as const,
        ['TTS', ttsProviderOptions, selectedTtsProviders, setSelectedTtsProviders] as const,
      ]).filter(([, opts]) => opts.length > 1).map(([label, opts, sel, setter]) => (
        <div key={label} className="flex items-start gap-3 flex-wrap">
          <span className={`text-xs font-semibold tracking-wide pt-1 min-w-[36px] ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
            {label}
          </span>
          <div className="flex flex-wrap gap-1.5 flex-1">
            {opts.map((opt) => {
              const checked = sel.has(opt);
              return (
                <button
                  key={opt}
                  type="button"
                  onClick={() =>
                    setter((prev) => {
                      const next = new Set(prev);
                      if (next.has(opt)) next.delete(opt);
                      else next.add(opt);
                      return next;
                    })
                  }
                  className={`text-xs px-2 py-1 rounded-md border transition-colors cursor-pointer ${
                    checked
                      ? isDark
                        ? 'bg-indigo-500/20 border-indigo-500/50 text-indigo-200'
                        : 'bg-indigo-50 border-indigo-300 text-indigo-700'
                      : isDark
                        ? 'bg-slate-900 border-slate-700 text-slate-400 hover:bg-slate-800'
                        : 'bg-gray-50 border-gray-300 text-gray-600 hover:bg-gray-100'
                  }`}
                >
                  {opt}
                </button>
              );
            })}
            {sel.size > 0 && (
              <button
                type="button"
                onClick={() => setter(new Set())}
                className={`text-xs px-2 py-1 ${isDark ? 'text-slate-500 hover:text-slate-300' : 'text-gray-400 hover:text-gray-600'}`}
              >
                clear
              </button>
            )}
          </div>
        </div>
      ))}
    </div>
  );
}
