import { useEffect, useMemo, useState } from 'react';
import { Link, useNavigate } from 'react-router-dom';
import axios from 'axios';
import {
  ArrowLeft, FlaskConical, Plus, Trash2, Save, AlertTriangle, Server,
} from 'lucide-react';
import { useTheme } from '../../contexts/ThemeContext';
import { API_BASE } from '../../config/api';
import type { Variant } from '../../components/benchmark/types';
import { createExperiment, useConfigs, useSuites } from './useBenchmarkApi';

interface WorkerInfo {
  id: string;
  status?: string;
  pipeline_name?: string | null;
  tags?: string[];
  capacity?: { max_concurrent?: number; active_sessions?: number };
}

interface VariantDraft {
  id: string;
  name: string;
  config_path: string;
  tags: string; // comma-separated for the form; split on save
}

function emptyVariant(): VariantDraft {
  return { id: '', name: '', config_path: '', tags: '' };
}

function slugify(s: string): string {
  return s.toLowerCase().trim()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '') || 'variant';
}

/**
 * Structured form for creating a new benchmark experiment.
 *
 * Picks a TestSuite + one or more Variants (each variant points at a
 * pipeline TOML config under examples/). Optionally pre-fills variant
 * tags from a registered worker so the runner's tag-match selector lands
 * the run on that worker without needing the explicit `worker_id` pin.
 *
 * After Save, redirects to the new experiment's detail page where the
 * Run button (with worker dropdown) can launch it.
 */
export default function ExperimentEditor() {
  const { isDark } = useTheme();
  const navigate = useNavigate();
  const { suites, loading: suitesLoading } = useSuites();
  const { configs, loading: configsLoading } = useConfigs();

  const [name, setName] = useState('');
  const [description, setDescription] = useState('');
  // The suite the user picked; until they pick one, the default below.
  const [chosenSuiteId, setSuiteId] = useState('');
  const [repetitions, setRepetitions] = useState(1);
  const [variants, setVariants] = useState<VariantDraft[]>([emptyVariant()]);
  const [workers, setWorkers] = useState<WorkerInfo[]>([]);
  const [tagSource, setTagSource] = useState<string>(''); // worker id whose tags pre-fill new variants

  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Default to the first synthetic-caller suite if present, otherwise the
  // first suite: derived while rendering, so it follows the suites as they
  // load without an effect writing state back.
  const defaultSuiteId =
    (suites.find(s => s.id.includes('synthetic-caller')) ?? suites[0])?.id ?? '';
  const suiteId = chosenSuiteId || defaultSuiteId;

  // Fetch the worker fleet so the user can scope variants to a specific worker's tags.
  useEffect(() => {
    const fetchWorkers = () => {
      axios.get(`${API_BASE}/api/v1/workers`)
        .then(res => {
          const payload = res.data?.data ?? res.data;
          const items: WorkerInfo[] = payload?.items ?? payload?.workers ?? (Array.isArray(payload) ? payload : []);
          setWorkers(items);
        })
        .catch(() => {});
    };
    fetchWorkers();
    const id = setInterval(fetchWorkers, 5000);
    return () => clearInterval(id);
  }, []);

  const selectedWorker = useMemo(
    () => workers.find(w => w.id === tagSource),
    [workers, tagSource],
  );

  const updateVariant = (idx: number, patch: Partial<VariantDraft>) => {
    setVariants(prev => prev.map((v, i) => (i === idx ? { ...v, ...patch } : v)));
  };

  const addVariant = () => {
    const seed = emptyVariant();
    // Pre-fill tags from the selected worker (if any) so the runner's
    // tag-matcher prefers it. Variant tags are a SUBSET requirement, so
    // we use all of the worker's tags.
    if (selectedWorker?.tags?.length) {
      seed.tags = selectedWorker.tags.join(', ');
    }
    setVariants(prev => [...prev, seed]);
  };

  const removeVariant = (idx: number) => {
    setVariants(prev => prev.filter((_, i) => i !== idx));
  };

  const handleSave = async () => {
    setError(null);

    if (!name.trim()) {
      setError('Experiment name is required.');
      return;
    }
    if (!suiteId) {
      setError('Pick a test suite.');
      return;
    }
    if (variants.length === 0) {
      setError('Add at least one variant.');
      return;
    }
    const cleaned: Variant[] = [];
    const seenIds = new Set<string>();
    for (const [i, v] of variants.entries()) {
      const vname = v.name.trim();
      if (!vname) { setError(`Variant #${i + 1}: name is required.`); return; }
      if (!v.config_path.trim()) { setError(`Variant #${i + 1}: pick a config TOML.`); return; }
      const vid = (v.id.trim() || slugify(vname));
      if (seenIds.has(vid)) { setError(`Variant id "${vid}" is duplicated.`); return; }
      seenIds.add(vid);
      cleaned.push({
        id: vid,
        name: vname,
        config_path: v.config_path.trim(),
        config_hash: '', // server overwrites at run time via freeze_variant_config
        tags: v.tags.split(',').map(t => t.trim()).filter(Boolean),
      });
    }

    setSaving(true);
    try {
      const created = await createExperiment({
        name: name.trim(),
        description: description.trim() || undefined,
        test_suite_id: suiteId,
        variants: cleaned,
        repetitions: Math.max(1, Math.floor(repetitions)),
      });
      navigate(`/benchmarks/${created.id}`);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  };

  const inputBase = `w-full rounded-lg border px-3 py-2 text-sm ${
    isDark
      ? 'bg-slate-900 border-slate-700 text-slate-200 focus:border-indigo-500'
      : 'bg-white border-gray-300 text-gray-800 focus:border-indigo-500'
  }`;

  return (
    <div className="p-8 overflow-y-auto h-full max-w-5xl">
      <Link
        to="/benchmarks"
        className={`flex items-center gap-2 text-sm mb-4 ${
          isDark ? 'text-slate-400 hover:text-white' : 'text-gray-500 hover:text-gray-900'
        }`}
      >
        <ArrowLeft size={16} /> Back to Benchmarks
      </Link>

      <div className="flex items-center gap-3 mb-2">
        <FlaskConical size={24} className="text-indigo-400" />
        <h1 className={`text-2xl font-bold ${isDark ? 'text-white' : 'text-gray-900'}`}>
          New Experiment
        </h1>
      </div>
      <p className={`text-sm mb-6 ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
        Pick a test suite, define one or more pipeline variants, then save.
        Run it from the Benchmarks list — the run button lets you pin a worker.
      </p>

      {/* ── Basic info ───────────────────────────────────── */}
      <div className={`rounded-xl border p-5 mb-4 ${
        isDark ? 'bg-slate-800/50 border-slate-700' : 'bg-white border-gray-200'
      }`}>
        <label className={`block text-xs font-semibold uppercase tracking-wide mb-1 ${
          isDark ? 'text-slate-400' : 'text-gray-500'
        }`}>Name</label>
        <input
          type="text"
          value={name}
          onChange={e => setName(e.target.value)}
          placeholder="e.g. OpenAI Realtime synthetic caller — text vs audio bridge"
          className={inputBase}
        />

        <label className={`block text-xs font-semibold uppercase tracking-wide mb-1 mt-4 ${
          isDark ? 'text-slate-400' : 'text-gray-500'
        }`}>Description (optional)</label>
        <textarea
          value={description}
          onChange={e => setDescription(e.target.value)}
          rows={2}
          placeholder="What is this experiment validating?"
          className={`${inputBase} resize-y`}
        />

        <div className="grid grid-cols-2 gap-4 mt-4">
          <div>
            <label className={`block text-xs font-semibold uppercase tracking-wide mb-1 ${
              isDark ? 'text-slate-400' : 'text-gray-500'
            }`}>Test suite</label>
            {suitesLoading ? (
              <div className={`text-sm ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>Loading…</div>
            ) : suites.length === 0 ? (
              <div className={`text-sm ${isDark ? 'text-amber-400' : 'text-amber-600'}`}>
                No suites yet — <Link to="/suites/new" className="underline">create one</Link>.
              </div>
            ) : (
              <select
                value={suiteId}
                onChange={e => setSuiteId(e.target.value)}
                className={inputBase}
              >
                {suites.map(s => (
                  <option key={s.id} value={s.id}>{s.name} ({s.id})</option>
                ))}
              </select>
            )}
          </div>

          <div>
            <label className={`block text-xs font-semibold uppercase tracking-wide mb-1 ${
              isDark ? 'text-slate-400' : 'text-gray-500'
            }`}>Repetitions per scenario</label>
            <input
              type="number"
              min={1}
              max={50}
              value={repetitions}
              onChange={e => setRepetitions(Number.parseInt(e.target.value, 10) || 1)}
              className={inputBase}
            />
          </div>
        </div>

        {/* Optional: scope variants to a worker's tags ─────── */}
        <div className="mt-4">
          <label className={`block text-xs font-semibold uppercase tracking-wide mb-1 ${
            isDark ? 'text-slate-400' : 'text-gray-500'
          }`}>
            Pre-fill new variant tags from worker (optional)
          </label>
          <div className="flex items-center gap-2">
            <Server size={16} className={isDark ? 'text-slate-400' : 'text-gray-500'} />
            <select
              value={tagSource}
              onChange={e => setTagSource(e.target.value)}
              className={inputBase}
            >
              <option value="">— none (tags will be empty by default) —</option>
              {workers.map(w => {
                const tagsLabel = w.tags?.length ? ` [${w.tags.join(', ')}]` : ' [no tags]';
                return (
                  <option key={w.id} value={w.id}>
                    {w.id} · {w.pipeline_name ?? 'pipeline?'}{tagsLabel}
                  </option>
                );
              })}
            </select>
          </div>
          <p className={`mt-1 text-xs ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>
            Variant tags are a subset filter — the runner picks the least-loaded worker
            whose tags include all variant tags. You can also override per-run with the
            worker dropdown on the Play button.
          </p>
        </div>
      </div>

      {/* ── Variants ─────────────────────────────────────── */}
      <div className={`rounded-xl border p-5 mb-4 ${
        isDark ? 'bg-slate-800/50 border-slate-700' : 'bg-white border-gray-200'
      }`}>
        <div className="flex items-center justify-between mb-3">
          <h2 className={`text-sm font-semibold uppercase tracking-wide ${
            isDark ? 'text-slate-300' : 'text-gray-700'
          }`}>Variants ({variants.length})</h2>
          <button
            onClick={addVariant}
            className={`inline-flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-xs font-medium transition-colors ${
              isDark ? 'bg-slate-700 hover:bg-slate-600 text-slate-200' : 'bg-gray-100 hover:bg-gray-200 text-gray-700'
            }`}
          >
            <Plus size={14} /> Add variant
          </button>
        </div>

        <div className="space-y-3">
          {variants.map((v, idx) => (
            <div
              key={idx}
              className={`rounded-lg border p-3 ${
                isDark ? 'bg-slate-900/50 border-slate-700' : 'bg-gray-50 border-gray-200'
              }`}
            >
              <div className="flex items-center justify-between mb-2">
                <span className={`text-xs font-semibold ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
                  Variant #{idx + 1}
                </span>
                {variants.length > 1 && (
                  <button
                    onClick={() => removeVariant(idx)}
                    className="p-1 rounded text-red-400 hover:bg-red-500/10"
                    title="Remove variant"
                  >
                    <Trash2 size={14} />
                  </button>
                )}
              </div>

              <div className="grid grid-cols-2 gap-3">
                <div>
                  <label className={`block text-[11px] uppercase tracking-wide mb-1 ${
                    isDark ? 'text-slate-500' : 'text-gray-400'
                  }`}>Name</label>
                  <input
                    type="text"
                    value={v.name}
                    onChange={e => updateVariant(idx, { name: e.target.value })}
                    placeholder="e.g. OpenAI GPT-4o-mini"
                    className={inputBase}
                  />
                </div>
                <div>
                  <label className={`block text-[11px] uppercase tracking-wide mb-1 ${
                    isDark ? 'text-slate-500' : 'text-gray-400'
                  }`}>ID (auto if blank)</label>
                  <input
                    type="text"
                    value={v.id}
                    onChange={e => updateVariant(idx, { id: e.target.value })}
                    placeholder={v.name ? slugify(v.name) : 'variant-id'}
                    className={inputBase}
                  />
                </div>
              </div>

              <div className="mt-3">
                <label className={`block text-[11px] uppercase tracking-wide mb-1 ${
                  isDark ? 'text-slate-500' : 'text-gray-400'
                }`}>Pipeline config (TOML)</label>
                {configsLoading ? (
                  <div className={`text-xs ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>Loading configs…</div>
                ) : (
                  <select
                    value={v.config_path}
                    onChange={e => updateVariant(idx, { config_path: e.target.value })}
                    className={inputBase}
                  >
                    <option value="">— pick a config —</option>
                    {configs.map(c => (
                      <option key={c.path} value={c.path}>{c.path}</option>
                    ))}
                  </select>
                )}
                <p className={`mt-1 text-[11px] ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>
                  Path is resolved relative to the orchestrator's working directory.
                </p>
              </div>

              <div className="mt-3">
                <label className={`block text-[11px] uppercase tracking-wide mb-1 ${
                  isDark ? 'text-slate-500' : 'text-gray-400'
                }`}>Tags (comma-separated, used to pick a worker)</label>
                <input
                  type="text"
                  value={v.tags}
                  onChange={e => updateVariant(idx, { tags: e.target.value })}
                  placeholder="e.g. gpu, llama"
                  className={inputBase}
                />
              </div>
            </div>
          ))}
        </div>
      </div>

      {error && (
        <div className={`mb-4 rounded-lg border p-3 text-sm flex items-start gap-2 ${
          isDark ? 'bg-red-500/10 border-red-500/40 text-red-200' : 'bg-red-50 border-red-200 text-red-700'
        }`}>
          <AlertTriangle size={16} className="shrink-0 mt-0.5" />
          <div>{error}</div>
        </div>
      )}

      <div className="flex items-center gap-2">
        <Link
          to="/benchmarks"
          className={`px-4 py-2 rounded-lg text-sm font-medium transition-colors ${
            isDark ? 'bg-slate-700 text-slate-200 hover:bg-slate-600' : 'bg-gray-100 text-gray-700 hover:bg-gray-200'
          }`}
        >
          Cancel
        </Link>
        <button
          onClick={handleSave}
          disabled={saving}
          className="inline-flex items-center gap-2 px-4 py-2 rounded-lg text-sm font-medium bg-indigo-500 hover:bg-indigo-600 disabled:opacity-50 text-white"
        >
          <Save size={16} /> {saving ? 'Creating…' : 'Create Experiment'}
        </button>
      </div>
    </div>
  );
}
