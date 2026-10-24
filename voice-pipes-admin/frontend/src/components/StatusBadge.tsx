import { CheckCircle, Loader, AlertCircle, XCircle } from 'lucide-react';
import { useTheme } from '../contexts/ThemeContext';

interface Props {
  status: string;
}

/**
 * Renders a coloured pill badge for a session or turn status.
 * Reads dark/light mode from ThemeContext so callers don't need to pass isDark.
 */
export function StatusBadge({ status }: Props) {
  const { isDark } = useTheme();
  switch (status) {
    case 'completed':
      return (
        <span className={`flex items-center gap-1 text-xs font-medium px-2 py-0.5 rounded-full ${isDark ? 'bg-emerald-500/20 text-emerald-400 border border-emerald-500/30' : 'bg-emerald-50 text-emerald-700 border border-emerald-200'}`}>
          <CheckCircle size={10} /> Completed
        </span>
      );
    case 'in_progress':
      return (
        <span className={`flex items-center gap-1 text-xs font-medium px-2 py-0.5 rounded-full ${isDark ? 'bg-sky-500/20 text-sky-400 border border-sky-500/30' : 'bg-sky-50 text-sky-700 border border-sky-200'}`}>
          <Loader size={10} className="animate-spin" /> In Progress
        </span>
      );
    case 'error':
      return (
        <span className={`flex items-center gap-1 text-xs font-medium px-2 py-0.5 rounded-full ${isDark ? 'bg-red-500/20 text-red-400 border border-red-500/30' : 'bg-red-50 text-red-700 border border-red-200'}`}>
          <AlertCircle size={10} /> Error
        </span>
      );
    case 'terminated':
      return (
        <span className={`flex items-center gap-1 text-xs font-medium px-2 py-0.5 rounded-full ${isDark ? 'bg-orange-500/20 text-orange-400 border border-orange-500/30' : 'bg-orange-50 text-orange-700 border border-orange-200'}`}>
          <XCircle size={10} /> Terminated
        </span>
      );
    default:
      return <span className="text-xs text-slate-500">{status}</span>;
  }
}
