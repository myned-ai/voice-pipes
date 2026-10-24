import { useTheme } from '../../../contexts/ThemeContext';
import type { TurnSummary } from './types';
import { splitAtInterrupt, buildInterruptTooltip } from './utils';

interface Props {
  /** The full LLM-generated text for this turn. */
  fullText: string;
  /** Server-truncated retained text, present only when the turn was interrupted. */
  retainedText?: string;
  /** Turn summary used to build the ✂ scissors tooltip. */
  summary?: TurnSummary;
}

/**
 * Renders agent turn text with an inline interruption indicator.
 *
 * When the turn was heard in full, renders a plain italic quote.
 * When interrupted, splits the text at the interruption point and renders:
 *   • heard portion — normal colour
 *   • ✂ scissors — with tooltip explaining the interruption details
 *   • unheard portion — grey strikethrough
 */
export function AgentTurnText({ fullText, retainedText, summary }: Props) {
  const { isDark } = useTheme();
  const { heardPart, unheardPart } = splitAtInterrupt(fullText, retainedText);

  // No split point available — but the turn is marked as interrupted.
  // Show a ✂ appended after the full text so the user knows playback was cut.
  if (!unheardPart && (summary?.interrupted || summary?.client_interrupted)) {
    return (
      <p className="text-sm italic leading-relaxed">
        &ldquo;
        <span className={isDark ? 'text-emerald-300' : 'text-gray-800'}>{fullText}</span>
        <span
          className={`not-italic text-[10px] ml-1 align-middle cursor-help ${isDark ? 'text-rose-400/70' : 'text-rose-500/60'}`}
          title={buildInterruptTooltip(summary)}
        >
          ✂
        </span>
        &rdquo;
      </p>
    );
  }

  if (!unheardPart) {
    return (
      <p className={`text-sm italic ${isDark ? 'text-emerald-300' : 'text-gray-800'}`}>
        &ldquo;{fullText}&rdquo;
      </p>
    );
  }

  return (
    <p className="text-sm italic leading-relaxed">
      &ldquo;
      <span className={isDark ? 'text-emerald-300' : 'text-gray-800'}>{heardPart}</span>
      <span
        className={`not-italic text-[10px] mx-1 align-middle cursor-help ${isDark ? 'text-rose-400/70' : 'text-rose-500/60'}`}
        title={buildInterruptTooltip(summary)}
      >
        ✂
      </span>
      <span className={`line-through ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>
        {unheardPart}
      </span>
      &rdquo;
    </p>
  );
}
