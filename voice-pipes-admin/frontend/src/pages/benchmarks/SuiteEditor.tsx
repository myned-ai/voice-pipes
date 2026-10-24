import { useMemo, useState } from 'react';
import { Link, useNavigate, useParams } from 'react-router-dom';
import { ArrowLeft, ClipboardList, Save, CheckCircle2, AlertTriangle, FlaskConical } from 'lucide-react';
import { useTheme } from '../../contexts/ThemeContext';
import type { TestSuite } from '../../components/benchmark/types';
import { useExperiments, useSuite, createSuite, updateSuite } from './useBenchmarkApi';
import { SUITE_TEMPLATES } from './suiteTemplates';

interface Props {
  /** When true, the page expects an :id param and PUTs the result. */
  editMode: boolean;
}

/**
 * Minimal create/edit page for a TestSuite. The MVP UX is paste-JSON + validate
 * + save — not a structured form. The textarea is the source of truth; we only
 * parse on validate / save.
 */
export default function SuiteEditor({ editMode }: Props) {
  const { isDark } = useTheme();
  const navigate = useNavigate();
  const { id } = useParams<{ id: string }>();
  const { suite, loading } = useSuite(editMode ? id : undefined);
  const { experiments } = useExperiments();

  // The first template in SUITE_TEMPLATES is the bare-bones scaffold, used as
  // the default seed when creating a new suite.
  const defaultTemplate = SUITE_TEMPLATES[0];
  const [templateId, setTemplateId] = useState<string>(defaultTemplate.id);
  const [text, setText] = useState<string>(editMode ? '' : defaultTemplate.build());
  const [validation, setValidation] = useState<ValidationResult | null>(null);
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);

  // Once the suite loads in edit mode, prefill the textarea: set while
  // rendering, once per suite object (React's pattern for state that follows
  // a prop), so a later edit is not overwritten and no effect re-renders.
  const [prefilledFrom, setPrefilledFrom] = useState<TestSuite | null>(null);
  if (editMode && suite && prefilledFrom !== suite) {
    setPrefilledFrom(suite);
    setText(JSON.stringify(suite, null, 2));
  }

  const linkedExperiments = useMemo(
    () => editMode && id ? experiments.filter(e => e.test_suite_id === id) : [],
    [editMode, id, experiments],
  );

  const validate = (): TestSuite | null => {
    const result = validateSuiteJson(text, { editMode, expectedId: id });
    setValidation(result);
    return result.suite;
  };

  const handleSave = async () => {
    const parsed = validate();
    if (!parsed) return;
    setSaving(true);
    setSaveError(null);
    try {
      if (editMode) {
        const updated = await updateSuite(parsed.id, parsed);
        navigate(`/suites/${updated.id}`);
      } else {
        const created = await createSuite(parsed);
        navigate(`/suites/${created.id}`);
      }
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      setSaveError(msg);
    } finally {
      setSaving(false);
    }
  };

  if (editMode && loading) {
    return <div className={`p-8 ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>Loading suite…</div>;
  }
  if (editMode && !loading && !suite) {
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
    <div className="p-8 overflow-y-auto h-full max-w-5xl">
      <Link
        to={editMode && id ? `/suites/${id}` : '/suites'}
        className={`flex items-center gap-2 text-sm mb-4 ${isDark ? 'text-slate-400 hover:text-white' : 'text-gray-500 hover:text-gray-900'}`}
      >
        <ArrowLeft size={16} /> Back
      </Link>

      <div className="flex items-center gap-3 mb-2">
        <ClipboardList size={24} className="text-indigo-400" />
        <h1 className={`text-2xl font-bold ${isDark ? 'text-white' : 'text-gray-900'}`}>
          {editMode ? `Edit suite${suite ? `: ${suite.name}` : ''}` : 'New Test Suite'}
        </h1>
      </div>
      <p className={`text-sm mb-4 ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
        {editMode
          ? 'Replace the suite definition. Existing experiments that reference this suite will use the new definition on their next run.'
          : 'Paste a TestSuite JSON document. See the BACKLOG / docs for the schema.'}
      </p>

      {/* Template selector (create mode only). */}
      {!editMode && (
        <div className={`mb-3 rounded-lg border p-3 flex flex-col sm:flex-row sm:items-center gap-3 ${
          isDark ? 'bg-slate-800/40 border-slate-700' : 'bg-gray-50 border-gray-200'
        }`}>
          <label
            htmlFor="suite-template-select"
            className={`text-sm font-medium ${isDark ? 'text-slate-300' : 'text-gray-700'}`}
          >
            Start from template
          </label>
          <select
            id="suite-template-select"
            value={templateId}
            onChange={e => {
              const next = SUITE_TEMPLATES.find(t => t.id === e.target.value);
              if (!next) return;
              setTemplateId(next.id);
              setText(next.build());
              setValidation(null);
              setSaveError(null);
            }}
            className={`text-sm rounded-md border px-2 py-1.5 sm:min-w-[280px] ${
              isDark ? 'bg-slate-900 border-slate-700 text-slate-200' : 'bg-white border-gray-300 text-gray-800'
            }`}
          >
            {SUITE_TEMPLATES.map(t => (
              <option key={t.id} value={t.id}>{t.label}</option>
            ))}
          </select>
          <span className={`text-xs ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
            {SUITE_TEMPLATES.find(t => t.id === templateId)?.description}
          </span>
        </div>
      )}

      {/* Edit-mode warning when other experiments depend on this suite. */}
      {editMode && linkedExperiments.length > 0 && (
        <div className={`mb-4 rounded-lg border p-3 flex items-start gap-2 text-sm ${
          isDark ? 'bg-amber-500/10 border-amber-500/40 text-amber-200' : 'bg-amber-50 border-amber-200 text-amber-800'
        }`}>
          <AlertTriangle size={16} className="shrink-0 mt-0.5" />
          <div>
            <strong>{linkedExperiments.length}</strong> experiment{linkedExperiments.length !== 1 ? 's' : ''} reference this suite.
            Their previous runs are preserved, but new runs will use the updated definition.
            <div className="mt-1 flex flex-wrap gap-1.5">
              {linkedExperiments.slice(0, 5).map(e => (
                <span key={e.id} className={`inline-flex items-center gap-1 px-2 py-0.5 rounded text-xs ${
                  isDark ? 'bg-amber-500/20' : 'bg-amber-100'
                }`}>
                  <FlaskConical size={11} /> {e.name}
                </span>
              ))}
              {linkedExperiments.length > 5 && (
                <span className="text-xs">+{linkedExperiments.length - 5} more</span>
              )}
            </div>
          </div>
        </div>
      )}

      <textarea
        value={text}
        onChange={e => { setText(e.target.value); setValidation(null); setSaveError(null); }}
        spellCheck={false}
        rows={28}
        className={`w-full font-mono text-xs rounded-lg border p-3 resize-y ${
          isDark ? 'bg-slate-900 border-slate-700 text-slate-200' : 'bg-white border-gray-300 text-gray-800'
        }`}
        placeholder='{ "id": "...", "name": "...", "scenarios": [...] }'
      />

      {validation && (
        <div
          className={`mt-3 rounded-lg border p-3 text-sm flex items-start gap-2 ${
            validation.suite
              ? isDark ? 'bg-emerald-500/10 border-emerald-500/40 text-emerald-200' : 'bg-emerald-50 border-emerald-200 text-emerald-800'
              : isDark ? 'bg-red-500/10 border-red-500/40 text-red-200' : 'bg-red-50 border-red-200 text-red-700'
          }`}
        >
          {validation.suite
            ? <CheckCircle2 size={16} className="shrink-0 mt-0.5" />
            : <AlertTriangle size={16} className="shrink-0 mt-0.5" />}
          <div>
            {validation.suite
              ? <span>Looks good — <strong>{validation.suite.scenarios.length}</strong> scenario{validation.suite.scenarios.length !== 1 ? 's' : ''}.</span>
              : <span>{validation.error}</span>}
          </div>
        </div>
      )}

      {saveError && (
        <div className={`mt-3 rounded-lg border p-3 text-sm ${
          isDark ? 'bg-red-500/10 border-red-500/40 text-red-200' : 'bg-red-50 border-red-200 text-red-700'
        }`}>
          Save failed: {saveError}
        </div>
      )}

      <div className="mt-4 flex items-center gap-2">
        <button
          onClick={validate}
          className={`inline-flex items-center gap-2 px-4 py-2 rounded-lg text-sm font-medium transition-colors ${
            isDark ? 'bg-slate-700 text-slate-200 hover:bg-slate-600' : 'bg-gray-100 text-gray-700 hover:bg-gray-200'
          }`}
        >
          Validate
        </button>
        <button
          onClick={handleSave}
          disabled={saving}
          className="inline-flex items-center gap-2 px-4 py-2 rounded-lg text-sm font-medium bg-indigo-500 hover:bg-indigo-600 disabled:opacity-50 text-white"
        >
          <Save size={16} /> {saving ? 'Saving…' : editMode ? 'Save Changes' : 'Create Suite'}
        </button>
      </div>
    </div>
  );
}

interface ValidationResult {
  suite: TestSuite | null;
  error?: string;
}

function validateSuiteJson(
  text: string,
  opts: { editMode: boolean; expectedId?: string },
): ValidationResult {
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    return { suite: null, error: `Invalid JSON: ${msg}` };
  }
  if (typeof parsed !== 'object' || parsed === null) {
    return { suite: null, error: 'Top-level value must be an object.' };
  }
  const obj = parsed as Record<string, unknown>;
  if (typeof obj.id !== 'string' || obj.id.length === 0) {
    return { suite: null, error: 'Missing required string field "id".' };
  }
  if (typeof obj.name !== 'string' || obj.name.length === 0) {
    return { suite: null, error: 'Missing required string field "name".' };
  }
  if (typeof obj.created_at !== 'string') {
    return { suite: null, error: 'Missing required string field "created_at" (ISO 8601).' };
  }
  if (!Array.isArray(obj.scenarios)) {
    return { suite: null, error: 'Field "scenarios" must be an array.' };
  }
  if (opts.editMode && opts.expectedId && obj.id !== opts.expectedId) {
    return { suite: null, error: `Suite "id" must match the URL id (${opts.expectedId}).` };
  }
  return { suite: obj as unknown as TestSuite };
}
