import { createContext, useContext } from 'react';

export type Theme = 'light' | 'dark';

export interface ThemeContextValue {
  theme: Theme;
  isDark: boolean;
  toggleTheme: () => void;
}

// The provider component lives in ./ThemeProvider so this module exports only
// the context object and the hook (Fast Refresh needs component modules to
// export nothing but components).  Every consumer keeps importing `useTheme`
// (and tests `ThemeContext`) from here.
export const ThemeContext = createContext<ThemeContextValue>({
  theme: 'dark',
  isDark: true,
  toggleTheme: () => {},
});

export function useTheme() {
  return useContext(ThemeContext);
}
