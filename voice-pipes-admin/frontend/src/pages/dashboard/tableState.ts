import { useEffect, useMemo, useState } from 'react';

/* ── Collapsible-table helpers ──────────────────────────────
 * `useCollapsed` persists a table's open/closed state per `storageKey`.
 * `usePagedTable` slices an array into pages (auto-clamps the current page
 * when the underlying rows shrink). `TablePager` renders the footer controls
 * and auto-hides when everything fits on one page. */
export function useCollapsed(storageKey: string, initial = false): [boolean, () => void] {
  const [collapsed, setCollapsed] = useState<boolean>(() => {
    try {
      const v = localStorage.getItem(storageKey);
      return v != null ? v === '1' : initial;
    } catch {
      return initial;
    }
  });
  useEffect(() => {
    try {
      localStorage.setItem(storageKey, collapsed ? '1' : '0');
    } catch {
      /* localStorage unavailable — non-fatal */
    }
  }, [storageKey, collapsed]);
  return [collapsed, () => setCollapsed((c) => !c)];
}

/** One page of a table's rows plus what the pager needs to move between pages. */
export interface PagedTable<T> { pageRows: T[]; page: number; setPage: (p: number) => void; totalPages: number; total: number; start: number; end: number }

export function usePagedTable<T>(
  rows: T[],
  pageSize: number,
): PagedTable<T> {
  const [page, setPage] = useState(0);
  const total = rows.length;
  const totalPages = Math.max(1, Math.ceil(total / pageSize));
  const clamped = Math.min(Math.max(page, 0), totalPages - 1);
  const start = clamped * pageSize;
  const end = Math.min(start + pageSize, total);
  const pageRows = useMemo(() => rows.slice(start, start + pageSize), [rows, start, pageSize]);
  return { pageRows, page: clamped, setPage, totalPages, total, start, end };
}
