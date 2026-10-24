import { Link, useParams, useNavigate } from 'react-router-dom';
import { ArrowLeft, ClipboardList, Loader2, FlaskConical, Trash2, Copy, Pencil } from 'lucide-react';
import { useTheme } from '../../contexts/ThemeContext';
import { useSuite, useExperiments, deleteSuite, duplicateSuite } from './useBenchmarkApi';
import ScenariosTable from '../../components/benchmark/ScenariosTable';

export default function SuiteDetail() {
  const { id } = useParams<{ id: string }>();
  const { isDark } = useTheme();
  const navigate = useNavigate();
  const { suite, loading: suiteLoading } = useSuite(id);
  const { experiments } = useExperiments();

  // Experiments that reference this suite
  const linkedExperiments = experiments.filter(e => e.test_suite_id === id);

  const handleDelete = async () => {
    if (!suite) return;
    const warn = linkedExperiments.length > 0
      ? `\n\nWARNING: ${linkedExperiments.length} experiment${linkedExperiments.length !== 1 ? 's' : ''} reference this suite. Their config snapshots remain, but new runs will fail.`
      : '';
    if (!window.confirm(`Delete test suite "${suite.name}"? This cannot be undone.${warn}`)) return;
    try {
      await deleteSuite(suite.id);
      navigate('/suites');
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      window.alert(`Failed to delete suite: ${msg}`);
    }
  };

  const handleDuplicate = async () => {
    if (!suite) return;
    try {
      const copy = await duplicateSuite(suite.id);
      navigate(`/suites/${copy.id}`);
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      window.alert(`Failed to duplicate suite: ${msg}`);
    }
  };

  if (suiteLoading) {
    return (
      <div className="flex items-center justify-center h-full">
        <Loader2 size={24} className="animate-spin text-indigo-400" />
      </div>
    );
  }

  if (!suite) {
    return (
      <div className="p-8">
        <Link to="/suites" className={`flex items-center gap-2 text-sm mb-4 ${isDark ? 'text-slate-400 hover:text-white' : 'text-gray-500 hover:text-gray-900'}`}>
          <ArrowLeft size={16} /> Back to Test Suites
        </Link>
        <p className={isDark ? 'text-slate-400' : 'text-gray-500'}>Suite not found.</p>
      </div>
    );
  }

  return (
    <div className="p-8 overflow-y-auto h-full">
      {/* Header */}
      <Link to="/suites" className={`flex items-center gap-2 text-sm mb-4 ${isDark ? 'text-slate-400 hover:text-white' : 'text-gray-500 hover:text-gray-900'}`}>
        <ArrowLeft size={16} /> Back to Test Suites
      </Link>

      <div className="flex items-start justify-between mb-1 gap-4">
        <div className="flex items-center gap-3 min-w-0">
          <ClipboardList size={24} className="text-indigo-400 shrink-0" />
          <h1 className={`text-2xl font-bold truncate ${isDark ? 'text-white' : 'text-gray-900'}`}>{suite.name}</h1>
        </div>
        <div className="flex items-center gap-2 shrink-0">
          <button
            onClick={() => navigate(`/suites/${suite.id}/edit`)}
            title="Edit suite (paste JSON)"
            className={`inline-flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-xs font-medium transition-colors ${
              isDark ? 'bg-slate-700 text-slate-200 hover:bg-slate-600' : 'bg-gray-100 text-gray-700 hover:bg-gray-200'
            }`}
          >
            <Pencil size={14} /> Edit
          </button>
          <button
            onClick={handleDuplicate}
            title="Duplicate suite"
            className={`inline-flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-xs font-medium transition-colors ${
              isDark ? 'bg-slate-700 text-slate-200 hover:bg-slate-600' : 'bg-gray-100 text-gray-700 hover:bg-gray-200'
            }`}
          >
            <Copy size={14} /> Duplicate
          </button>
          <button
            onClick={handleDelete}
            title="Delete suite"
            className={`inline-flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-xs font-medium transition-colors ${
              isDark ? 'bg-red-500/20 text-red-300 hover:bg-red-500/30' : 'bg-red-50 text-red-700 hover:bg-red-100'
            }`}
          >
            <Trash2 size={14} /> Delete
          </button>
        </div>
      </div>

      {suite.description && (
        <p className={`text-sm mb-2 ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>{suite.description}</p>
      )}
      <p className={`text-xs mb-6 ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>
        {suite.scenarios.length} scenario{suite.scenarios.length !== 1 ? 's' : ''} · Created {new Date(suite.created_at).toLocaleDateString()}
      </p>

      {/* Scenarios */}
      <div className="mb-8">
        <ScenariosTable scenarios={suite.scenarios} />
      </div>

      {/* Used By Experiments */}
      {linkedExperiments.length > 0 && (
        <div className={`rounded-xl border p-5 ${isDark ? 'bg-slate-800 border-slate-700' : 'bg-white border-gray-200'}`}>
          <h3 className={`text-sm font-semibold mb-3 flex items-center gap-2 ${isDark ? 'text-white' : 'text-gray-900'}`}>
            <FlaskConical size={16} className={isDark ? 'text-slate-400' : 'text-gray-500'} />
            Used By Experiments
          </h3>
          <div className="flex flex-wrap gap-2">
            {linkedExperiments.map(exp => (
              <Link
                key={exp.id}
                to={`/benchmarks/${exp.id}`}
                className={`inline-flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-xs font-medium transition-colors ${
                  isDark
                    ? 'bg-slate-700 text-slate-300 hover:bg-slate-600'
                    : 'bg-gray-100 text-gray-700 hover:bg-gray-200'
                }`}
              >
                {exp.name}
                <span className={`text-[10px] ${
                  exp.status === 'complete' ? 'text-emerald-400'
                  : exp.status === 'running' ? 'text-amber-400'
                  : exp.status === 'failed' ? 'text-red-400'
                  : isDark ? 'text-slate-500' : 'text-gray-400'
                }`}>
                  ({exp.status})
                </span>
              </Link>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}
