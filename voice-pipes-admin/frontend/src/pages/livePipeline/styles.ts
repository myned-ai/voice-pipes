import type { HealthState } from '../../lib/fleet';

// Chip palette for the fleet-overview bar (P15), keyed by fraction-aware status.
export type FleetChipPalette = { dot: string; text: string; bg: string; border: string; activeBorder: string };
export const FLEET_COLORS: Record<HealthState, { dark: FleetChipPalette; light: FleetChipPalette }> = {
  healthy: {
    dark: { dot: '#34d399', text: '#a7f3d0', bg: '#064e3b66', border: '#10b98155', activeBorder: '#10b981' },
    light: { dot: '#059669', text: '#065f46', bg: '#d1fae5', border: '#6ee7b7', activeBorder: '#059669' },
  },
  degraded: {
    dark: { dot: '#fbbf24', text: '#fde68a', bg: '#78350f66', border: '#f59e0b55', activeBorder: '#f59e0b' },
    light: { dot: '#d97706', text: '#92400e', bg: '#fef3c7', border: '#fcd34d', activeBorder: '#d97706' },
  },
  unhealthy: {
    dark: { dot: '#f87171', text: '#fecaca', bg: '#7f1d1d66', border: '#ef444455', activeBorder: '#ef4444' },
    light: { dot: '#dc2626', text: '#991b1b', bg: '#fee2e2', border: '#fca5a5', activeBorder: '#dc2626' },
  },
  not_configured: {
    dark: { dot: '#94a3b8', text: '#cbd5e1', bg: '#33415566', border: '#47556955', activeBorder: '#64748b' },
    light: { dot: '#94a3b8', text: '#475569', bg: '#f1f5f9', border: '#cbd5e1', activeBorder: '#94a3b8' },
  },
};

// Inline pill style for component / aggregate health badges. Matches the
// snake_case wire values emitted by the orchestrator (HealthState).
export function healthBadgeStyle(state: string, isDark: boolean): React.CSSProperties {
  const base: React.CSSProperties = {
    fontSize: '10px',
    fontWeight: 700,
    padding: '2px 6px',
    borderRadius: '6px',
  };
  switch (state) {
    case 'healthy':
      return { ...base, background: isDark ? '#065f46' : '#d1fae5', color: isDark ? '#34d399' : '#059669' };
    case 'degraded':
      return { ...base, background: isDark ? '#78350f' : '#fef3c7', color: isDark ? '#fbbf24' : '#b45309' };
    case 'unhealthy':
      return { ...base, background: isDark ? '#7f1d1d' : '#fee2e2', color: isDark ? '#fca5a5' : '#dc2626' };
    case 'not_configured':
    default:
      return { ...base, background: isDark ? '#1e293b' : '#f1f5f9', color: isDark ? '#94a3b8' : '#64748b' };
  }
}
