import { Link, useNavigate } from 'react-router-dom';
import { ClipboardList, Loader2, Plus, Trash2, WifiOff } from 'lucide-react';
import { useTheme } from '../../contexts/ThemeContext';
import { API_BASE } from '../../config/api';
import { useSuites, deleteSuite } from './useBenchmarkApi';

export default function SuitesList() {
  const { isDark } = useTheme();
  const navigate = useNavigate();
  const { suites, loading, error, setSuites } = useSuites();

  const handleDelete = async (id: string, name: string, e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    if (!window.confirm(`Delete test suite "${name}"? This cannot be undone.`)) return;
    try {
      await deleteSuite(id);
      setSuites(prev => prev.filter(s => s.id !== id));
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      window.alert(`Failed to delete suite: ${msg}`);
    }
  };

  return (
    <div className="p-8 overflow-y-auto h-full">
      {/* Header */}
      <div className="flex items-center justify-between mb-6">
        <div className="flex items-center gap-3">
          <ClipboardList size={24} className="text-indigo-400" />
          <h1 className={`text-2xl font-bold ${isDark ? 'text-white' : 'text-gray-900'}`}>Test Suites</h1>
        </div>
        <button
          onClick={() => navigate('/suites/new')}
          className={`inline-flex items-center gap-2 px-3 py-2 rounded-lg text-sm font-medium transition-colors ${
            isDark ? 'bg-indigo-500 hover:bg-indigo-600 text-white' : 'bg-indigo-500 hover:bg-indigo-600 text-white'
          }`}
        >
          <Plus size={16} /> New Suite
        </button>
      </div>

      {/* Loading */}
      {loading && (
        <div className="flex items-center justify-center py-20">
          <Loader2 size={24} className="animate-spin text-indigo-400" />
        </div>
      )}

      {/* Orchestrator offline */}
      {!loading && error && suites.length === 0 && (
        <div className={`flex flex-col items-center justify-center py-24 rounded-xl border gap-3 ${isDark ? 'bg-slate-800/50 border-slate-700' : 'bg-white border-gray-200'}`}>
          <div className={`w-14 h-14 rounded-2xl flex items-center justify-center ${isDark ? 'bg-slate-700/60' : 'bg-gray-100'}`}>
            <WifiOff size={24} className={isDark ? 'text-slate-500' : 'text-gray-400'} />
          </div>
          <span className={`text-base font-semibold ${isDark ? 'text-slate-300' : 'text-gray-600'}`}>Orchestrator Offline</span>
          <span className={`text-sm text-center max-w-md leading-relaxed ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>
            Test suites are served by the orchestrator. Could not reach{' '}
            <code className={`px-1.5 py-0.5 rounded text-xs ${isDark ? 'bg-slate-700 text-slate-300' : 'bg-gray-100 text-gray-600'}`}>{API_BASE}</code>.
            Start the orchestrator and this list will refresh automatically.
          </span>
        </div>
      )}

      {/* Empty state */}
      {!loading && !error && suites.length === 0 && (
        <div className={`flex flex-col items-center justify-center py-24 rounded-xl border gap-3 ${isDark ? 'bg-slate-800/50 border-slate-700' : 'bg-white border-gray-200'}`}>
          <div className={`w-14 h-14 rounded-2xl flex items-center justify-center ${isDark ? 'bg-slate-700/60' : 'bg-gray-100'}`}>
            <ClipboardList size={24} className={isDark ? 'text-slate-500' : 'text-gray-400'} />
          </div>
          <span className={`text-base font-semibold ${isDark ? 'text-slate-300' : 'text-gray-600'}`}>No test suites yet</span>
          <span className={`text-sm text-center max-w-xs leading-relaxed ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>
            Define scenarios and expected responses to run automated benchmarks
          </span>
          <button
            onClick={() => navigate('/suites/new')}
            className="mt-3 inline-flex items-center gap-2 px-4 py-2 rounded-lg text-sm font-medium bg-indigo-500 hover:bg-indigo-600 text-white"
          >
            <Plus size={16} /> Create your first suite
          </button>
        </div>
      )}

      {/* Suite list */}
      {!loading && suites.length > 0 && (
        <div className="space-y-3">
          {suites.map(suite => (
            <Link
              key={suite.id}
              to={`/suites/${suite.id}`}
              className={`block rounded-xl border p-5 transition-colors ${
                isDark
                  ? 'bg-slate-800 border-slate-700 hover:border-slate-600'
                  : 'bg-white border-gray-200 hover:border-gray-300'
              }`}
            >
              <div className="flex items-start justify-between">
                <div className="min-w-0">
                  <h2 className={`text-lg font-semibold ${isDark ? 'text-white' : 'text-gray-900'}`}>
                    {suite.name}
                  </h2>
                  {suite.description && (
                    <p className={`text-sm mt-1 ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
                      {suite.description}
                    </p>
                  )}
                  <p className={`text-xs mt-2 ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>
                    Created {new Date(suite.created_at).toLocaleDateString()}
                  </p>
                </div>
                <div className="flex items-center gap-3 shrink-0 ml-4">
                  <span className={`text-sm ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
                    {suite.scenarios.length} scenario{suite.scenarios.length !== 1 ? 's' : ''}
                  </span>
                  <button
                    onClick={e => handleDelete(suite.id, suite.name, e)}
                    title="Delete suite"
                    className={`p-1.5 rounded transition-colors ${
                      isDark ? 'text-slate-500 hover:text-red-400 hover:bg-slate-700' : 'text-gray-400 hover:text-red-500 hover:bg-gray-100'
                    }`}
                  >
                    <Trash2 size={16} />
                  </button>
                </div>
              </div>
            </Link>
          ))}
        </div>
      )}
    </div>
  );
}
