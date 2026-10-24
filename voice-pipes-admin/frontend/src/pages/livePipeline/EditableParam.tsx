import { useState } from 'react';
import { Check, X } from 'lucide-react';
import type { ParamDescriptor } from '../../lib/orchestrator';
import { clientValidate, parseParamInput } from './params';

// ── Inline-edit primitive for a single mutable param ─────────────────────────

export interface EditableParamProps {
  param: ParamDescriptor;
  value: unknown;
  /** Read-only display colour for the value text. */
  displayColor: string;
  /** Hover/active accent colour (mirrors the surrounding node theme). */
  accentColor: string;
  /** Background of the inline input/select. */
  inputBg: string;
  /** Border applied to the inline input/select. */
  inputBorder: string;
  /** Border colour used to flag a client-side validation error. */
  errorColor: string;
  /** Disabled when no worker is selected (nothing to PATCH). */
  disabled: boolean;
  /** Async submit. Resolves with `null` on success, error string on failure. */
  onSubmit: (newValue: unknown) => Promise<string | null>;
}

export function EditableParam({
  param,
  value,
  displayColor,
  accentColor,
  inputBg,
  inputBorder,
  errorColor,
  disabled,
  onSubmit,
}: EditableParamProps) {
  const [editing, setEditing] = useState(false);
  // Store the in-progress text as a string so number/text inputs share state.
  const [draft, setDraft] = useState<string>(() => String(value ?? ''));
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const start = () => {
    if (disabled) return;
    setDraft(
      param.value_type === 'bool'
        ? String(Boolean(value))
        : String(value ?? ''),
    );
    setError(null);
    setEditing(true);
  };

  const cancel = () => {
    setEditing(false);
    setDraft(String(value ?? ''));
    setError(null);
  };

  const commit = async () => {
    const parsed = parseParamInput(draft, param.value_type);
    if (parsed === null) {
      setError('invalid');
      return;
    }
    const local = clientValidate(parsed, param);
    if (local) {
      setError(local);
      return;
    }
    setSaving(true);
    setError(null);
    const remote = await onSubmit(parsed);
    setSaving(false);
    if (remote) {
      setError(remote);
      return;
    }
    setEditing(false);
  };

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'Enter') {
      e.preventDefault();
      void commit();
    } else if (e.key === 'Escape') {
      e.preventDefault();
      cancel();
    }
  };

  if (!editing) {
    return (
      <span
        onClick={start}
        title={
          disabled
            ? `Select a worker to edit runtime params\n\n${String(value)}`
            : `Click to edit (${param.value_type})\n\n${String(value)}`
        }
        style={{
          color: displayColor,
          fontWeight: 500,
          fontFamily: 'monospace',
          overflow: 'hidden',
          textOverflow: 'ellipsis',
          whiteSpace: 'nowrap',
          maxWidth: '140px',
          textAlign: 'right',
          cursor: disabled ? 'not-allowed' : 'pointer',
          padding: '0 4px',
          borderRadius: '4px',
          border: '1px solid transparent',
          transition: 'border-color 0.15s, background 0.15s',
          opacity: disabled ? 0.6 : 1,
        }}
        onMouseEnter={e => {
          if (!disabled) e.currentTarget.style.borderColor = accentColor;
        }}
        onMouseLeave={e => {
          e.currentTarget.style.borderColor = 'transparent';
        }}
      >
        {String(value)}
      </span>
    );
  }

  // ── Editing modes ─────────────────────────────────────────
  const fieldStyle: React.CSSProperties = {
    background: inputBg,
    border: `1px solid ${error ? errorColor : inputBorder}`,
    color: displayColor,
    padding: '2px 6px',
    borderRadius: '4px',
    fontSize: '11px',
    fontFamily: 'monospace',
    width: '100px',
    outline: 'none',
  };

  let editor: React.ReactNode;
  if (param.value_type === 'bool') {
    editor = (
      <select
        autoFocus
        value={draft}
        disabled={saving}
        onChange={e => setDraft(e.target.value)}
        onKeyDown={onKeyDown}
        style={fieldStyle}
      >
        <option value="true">true</option>
        <option value="false">false</option>
      </select>
    );
  } else if (param.value_type === 'enum' && param.constraints?.options) {
    editor = (
      <select
        autoFocus
        value={draft}
        disabled={saving}
        onChange={e => setDraft(e.target.value)}
        onKeyDown={onKeyDown}
        style={fieldStyle}
      >
        {param.constraints.options.map(opt => (
          <option key={opt} value={opt}>
            {opt}
          </option>
        ))}
      </select>
    );
  } else if ((param.key === 'system_prompt' || param.key === 'instructions') && param.value_type === 'str') {
    editor = (
      <textarea
        autoFocus
        value={draft}
        disabled={saving}
        rows={12}
        onChange={e => setDraft(e.target.value)}
        onKeyDown={e => {
          // Ctrl+Enter commits, Escape cancels (Enter alone allows newlines).
          if ((e.ctrlKey || e.metaKey) && e.key === 'Enter') {
            e.preventDefault();
            void commit();
          } else if (e.key === 'Escape') {
            e.preventDefault();
            cancel();
          }
        }}
        style={{ 
          ...fieldStyle, 
          width: '420px', 
          minHeight: '220px', 
          resize: 'both',
          border: 'none',
          outline: 'none',
          padding: 0,
          background: 'transparent'
        }}
      />
    );
  } else {
    editor = (
      <input
        autoFocus
        type={param.value_type === 'float' || param.value_type === 'int' ? 'number' : 'text'}
        value={draft}
        disabled={saving}
        step={param.value_type === 'int' ? 1 : 'any'}
        min={param.constraints?.min}
        max={param.constraints?.max}
        onChange={e => setDraft(e.target.value)}
        onKeyDown={onKeyDown}
        style={fieldStyle}
      />
    );
  }

  const isTextarea = (param.key === 'system_prompt' || param.key === 'instructions') && param.value_type === 'str';

  return (
    <span
      style={{
        display: 'inline-flex',
        alignItems: 'center',
        gap: '4px',
        position: 'relative',
      }}
      title={error ?? undefined}
    >
      {isTextarea ? (
        <div
          style={{
            position: 'absolute',
            zIndex: 1000,
            top: '0',
            right: '0',
            background: inputBg,
            padding: '8px',
            borderRadius: '6px',
            boxShadow: '0 4px 12px rgba(0,0,0,0.3)',
            border: `1px solid ${error ? errorColor : inputBorder}`,
            display: 'flex',
            flexDirection: 'column',
            gap: '8px',
            alignItems: 'flex-end',
          }}
        >
          {editor}
          <div style={{ display: 'flex', gap: '8px' }}>
            <button
              onClick={cancel}
              disabled={saving}
              style={{
                background: 'transparent',
                border: `1px solid ${inputBorder}`,
                borderRadius: '4px',
                padding: '4px 8px',
                cursor: 'pointer',
                color: displayColor,
                fontSize: '11px',
              }}
            >
              Cancel
            </button>
            <button
              onClick={() => void commit()}
              disabled={saving}
              style={{
                background: accentColor,
                border: 'none',
                borderRadius: '4px',
                padding: '4px 8px',
                cursor: saving ? 'wait' : 'pointer',
                color: '#fff',
                fontSize: '11px',
              }}
            >
              {saving ? 'Saving...' : 'Save'}
            </button>
          </div>
        </div>
      ) : (
        <>
          {editor}
          <button
            onClick={() => void commit()}
            disabled={saving}
            title="Save (Enter)"
            style={{
              background: 'transparent',
              border: 'none',
              cursor: saving ? 'wait' : 'pointer',
              color: error ? errorColor : accentColor,
              display: 'flex',
              alignItems: 'center',
              padding: 0,
            }}
          >
            <Check size={12} />
          </button>
          <button
            onClick={cancel}
            disabled={saving}
            title="Cancel (Esc)"
            style={{
              background: 'transparent',
              border: 'none',
              cursor: 'pointer',
              color: errorColor,
              display: 'flex',
              alignItems: 'center',
              padding: 0,
            }}
          >
            <X size={12} />
          </button>
        </>
      )}
    </span>
  );
}
