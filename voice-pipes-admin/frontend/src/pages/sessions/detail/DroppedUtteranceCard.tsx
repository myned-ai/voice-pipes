import { MicOff } from 'lucide-react';
import { useTheme } from '../../../contexts/ThemeContext';
import { API_BASE } from '../../../config/api';
import type { DroppedUtterance } from './types';
import { describeDrop } from './droppedUtterance';

/** Speech the caller made that became no turn. Shown before the turn that
 *  took its id, with the microphone recording when the capture kept it, so
 *  noise the caller made can be told from speech the recogniser missed. */
export const DroppedUtteranceCard = ({ drop, sessionId, utteranceId }: {
  drop: DroppedUtterance;
  sessionId: string;
  utteranceId: number;
}) => {
  const { isDark } = useTheme();
  const fault = drop.had_interim_text;
  const border = fault
    ? (isDark ? 'border-red-500/40' : 'border-red-300')
    : (isDark ? 'border-amber-500/30' : 'border-amber-200');
  const bg = isDark ? 'bg-slate-800/60' : 'bg-amber-50/40';
  const src = drop.file
    ? `${API_BASE}/api/v1/sessions/${encodeURIComponent(sessionId)}/audio/${encodeURIComponent(drop.file)}`
    : null;
  return (
    <div className={`rounded-xl px-5 py-3 border ${bg} ${border} flex flex-wrap items-center gap-x-4 gap-y-2`} data-testid="dropped-utterance">
      <div className={`flex items-center gap-2 text-sm font-medium ${isDark ? 'text-amber-200' : 'text-amber-800'}`}>
        <MicOff className="w-4 h-4" />
        Heard, not recognised
        <span className={`text-xs font-normal ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>before turn {utteranceId}</span>
      </div>
      <div className={`text-sm ${isDark ? 'text-slate-300' : 'text-gray-700'}`}>{describeDrop(drop)}</div>
      {src ? (
        <audio controls preload="none" src={src} className="h-8 ml-auto" aria-label={`Recording of dropped speech before turn ${utteranceId}`} />
      ) : (
        <span className={`text-xs ml-auto ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>no recording kept</span>
      )}
    </div>
  );
};
