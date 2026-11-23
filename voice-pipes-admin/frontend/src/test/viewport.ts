type Listener = (e: MediaQueryListEvent) => void;

let currentWidth = 1440;
let currentCoarse = false;
const listeners = new Set<{ query: string; fn: Listener }>();

function evaluateQuery(query: string): boolean {
  const minMatch = query.match(/\(min-width:\s*(\d+)px\)/);
  if (minMatch) return currentWidth >= Number(minMatch[1]);
  const maxMatch = query.match(/\(max-width:\s*(\d+)px\)/);
  if (maxMatch) return currentWidth <= Number(maxMatch[1]);
  if (query.includes('pointer: coarse')) return currentCoarse;
  if (query.includes('hover: none')) return currentCoarse;
  return false;
}

export function installMatchMediaMock(): void {
  Object.defineProperty(window, 'innerWidth', {
    configurable: true,
    writable: true,
    value: currentWidth,
  });
  Object.defineProperty(window, 'matchMedia', {
    configurable: true,
    writable: true,
    value: (query: string): MediaQueryList => ({
      matches: evaluateQuery(query),
      media: query,
      onchange: null,
      addListener: () => {},
      removeListener: () => {},
      addEventListener: (_type: string, fn: EventListenerOrEventListenerObject) => {
        if (typeof fn === 'function') {
          listeners.add({ query, fn: fn as Listener });
        }
      },
      removeEventListener: (_type: string, fn: EventListenerOrEventListenerObject) => {
        for (const entry of listeners) {
          if (entry.query === query && entry.fn === fn) listeners.delete(entry);
        }
      },
      dispatchEvent: () => true,
    }),
  });
}

/**
 * Set the simulated viewport width (in px) and notify all mounted
 * `useMediaQuery` subscribers immediately.
 */
export function setMockViewport(widthPx: number, opts?: { coarsePointer?: boolean }): void {
  currentWidth = widthPx;
  currentCoarse = opts?.coarsePointer ?? widthPx < 1024;
  if (typeof window !== 'undefined') {
    Object.defineProperty(window, 'innerWidth', {
      configurable: true,
      writable: true,
      value: currentWidth,
    });
  }
  for (const { query, fn } of Array.from(listeners)) {
    fn({ matches: evaluateQuery(query), media: query } as MediaQueryListEvent);
  }
}
