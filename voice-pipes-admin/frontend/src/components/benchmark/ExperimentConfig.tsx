import { Link } from 'react-router-dom';
import { Calendar, Hash, Repeat, FlaskConical, FileText } from 'lucide-react';
import { useTheme } from '../../contexts/ThemeContext';
import type { Experiment } from './types';

interface Props {
  experiment: Experiment;
  suiteName?: string;
}

function fmtDate(iso?: string): string {
  if (!iso) return '—';
  return new Date(iso).toLocaleString(undefined, {
    year: 'numeric', month: 'short', day: 'numeric',
    hour: '2-digit', minute: '2-digit',
  });
}

export default function ExperimentConfig({ experiment, suiteName }: Props) {
  const { isDark } = useTheme();

  const rows: { icon: React.ReactNode; label: string; value: React.ReactNode }[] = [
    {
      icon: <FileText size={14} />,
      label: 'Test Suite',
      value: suiteName ? (
        <Link
          to={`/suites/${experiment.test_suite_id}`}
          className="text-indigo-400 hover:underline"
        >
          {suiteName}
        </Link>
      ) : (
        <span className={isDark ? 'text-slate-400' : 'text-gray-500'}>
          {experiment.test_suite_id}
        </span>
      ),
    },
    {
      icon: <Repeat size={14} />,
      label: 'Repetitions',
      value: experiment.repetitions,
    },
    {
      icon: <FlaskConical size={14} />,
      label: 'Status',
      value: experiment.status,
    },
    {
      icon: <Calendar size={14} />,
      label: 'Created',
      value: fmtDate(experiment.created_at),
    },
    {
      icon: <Calendar size={14} />,
      label: 'Started',
      value: fmtDate(experiment.started_at),
    },
    {
      icon: <Calendar size={14} />,
      label: 'Completed',
      value: fmtDate(experiment.completed_at),
    },
  ];

  return (
    <div className={`rounded-xl border p-5 ${isDark ? 'bg-slate-800 border-slate-700' : 'bg-white border-gray-200'}`}>
      <h3 className={`text-sm font-semibold mb-4 flex items-center gap-2 ${isDark ? 'text-white' : 'text-gray-900'}`}>
        <Hash size={16} className={isDark ? 'text-slate-400' : 'text-gray-500'} />
        Experiment Configuration
      </h3>
      <div className="grid gap-3">
        {rows.map(row => (
          <div key={row.label} className="flex items-center gap-3">
            <span className={isDark ? 'text-slate-500' : 'text-gray-400'}>{row.icon}</span>
            <span className={`text-sm font-medium w-28 shrink-0 ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
              {row.label}
            </span>
            <span className={`text-sm ${isDark ? 'text-slate-200' : 'text-gray-800'}`}>
              {row.value}
            </span>
          </div>
        ))}
      </div>
    </div>
  );
}
