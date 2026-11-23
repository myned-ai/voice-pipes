import { useCallback, useSyncExternalStore } from 'react';

export const BP_SM = '(min-width: 640px)';
export const BP_MD = '(min-width: 768px)';
export const BP_LG = '(min-width: 1024px)';
export const BP_XL = '(min-width: 1280px)';

/**
 * Live media-query subscription backed by `useSyncExternalStore`.
 * Returns `true` when `window.matchMedia(query).matches` holds.
 */
export function useMediaQuery(query: string): boolean {
  const subscribe = useCallback((onChange: () => void) => {
    if (typeof window === 'undefined' || typeof window.matchMedia !== 'function') {
      return () => {};
    }
    const mq = window.matchMedia(query);
    mq.addEventListener('change', onChange);
    return () => mq.removeEventListener('change', onChange);
  }, [query]);

  const getSnapshot = () => {
    if (typeof window === 'undefined' || typeof window.matchMedia !== 'function') {
      return true;
    }
    return window.matchMedia(query).matches;
  };

  return useSyncExternalStore(subscribe, getSnapshot, () => true);
}
