import type { ParamDescriptor } from '../../lib/orchestrator';

/** Returns true when the param's visibility condition is satisfied. */
export function isParamVisible(param: ParamDescriptor, metadata: Record<string, unknown>): boolean {
  if (!param.only_when) return true;
  const actual = String(metadata[param.only_when.field] ?? '');
  return param.only_when.values.includes(actual);
}

/** Format one metadata value for a node card row.
 *
 *  Plain `String(v)` renders any object as "[object Object]", which is what an
 *  `options` bag looks like on the card — the contents are lost entirely.
 *  Arrays join, nested objects fall back to compact JSON, and scalars pass
 *  through. One-level objects are handled by `flattenMetadata` instead, which
 *  gives each of their keys its own row. */
export function formatMetaValue(v: unknown): string {
  if (v === null || v === undefined) return '';
  if (Array.isArray(v)) return v.map(formatMetaValue).join(', ');
  if (typeof v === 'object') return JSON.stringify(v);
  return String(v);
}

/** Full value for the row's `title` tooltip — pretty JSON for objects, so the
 *  truncated row still has the complete value one hover away. */
export function metaTooltip(v: unknown): string {
  if (v !== null && typeof v === 'object') return JSON.stringify(v, null, 2);
  return String(v ?? '');
}

/** Flatten metadata into the `[label, value, raw]` rows shown under
 *  CONFIGURATION, skipping keys the param schema already renders.
 *
 *  A one-level object (`options = { kind: "blendshapes" }`) expands to one row
 *  per key (`options.kind → blendshapes`) rather than a single truncated blob:
 *  the rows are width-limited, so several short rows read far better than one
 *  clipped JSON string. */
export function flattenMetadata(
  metadata: Record<string, unknown>,
  skip: Set<string>,
): Array<[string, string, unknown]> {
  const rows: Array<[string, string, unknown]> = [];
  for (const key of Object.keys(metadata)) {
    if (skip.has(key)) continue;
    const value = metadata[key];
    if (value === null || value === undefined || value === '') continue;

    const isPlainObject = typeof value === 'object' && !Array.isArray(value);
    if (isPlainObject) {
      for (const [subKey, subValue] of Object.entries(value as Record<string, unknown>)) {
        if (subValue === null || subValue === undefined || subValue === '') continue;
        rows.push([`${key}.${subKey}`, formatMetaValue(subValue), subValue]);
      }
      continue;
    }
    rows.push([key, formatMetaValue(value), value]);
  }
  return rows;
}

/** Parse a string from a number/bool/str input back to the JSON value the
 *  worker expects. Returns `null` if the input cannot be coerced. */
export function parseParamInput(
  raw: string,
  ty: ParamDescriptor['value_type'],
): unknown | null {
  switch (ty) {
    case 'float': {
      const n = Number(raw);
      return Number.isFinite(n) ? n : null;
    }
    case 'int': {
      const n = Number(raw);
      return Number.isFinite(n) && Number.isInteger(n) ? n : null;
    }
    case 'bool':
      return raw === 'true';
    case 'str':
    case 'enum':
    case 'path':
      return raw;
  }
}

/** Lightweight client-side range/options check — same rules the worker
 *  enforces, only used for UX (red border, blocked submit). The server is
 *  still the source of truth. */
export function clientValidate(value: unknown, p: ParamDescriptor): string | null {
  if (p.constraints) {
    if (typeof value === 'number') {
      if (p.constraints.min != null && value < p.constraints.min)
        return `min ${p.constraints.min}`;
      if (p.constraints.max != null && value > p.constraints.max)
        return `max ${p.constraints.max}`;
    }
    if (typeof value === 'string' && p.constraints.options) {
      if (!p.constraints.options.includes(value))
        return `not in [${p.constraints.options.join(', ')}]`;
    }
  }
  return null;
}
