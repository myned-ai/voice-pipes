import type { DroppedUtterance } from './types';

/** One line of words for the reason the engine gives for dropping speech. */
export function describeDrop(d: DroppedUtterance): string {
  const heard = d.audio_ms != null ? `${(d.audio_ms / 1000).toFixed(1)} s of speech` : 'speech';
  switch (d.reason) {
    case 'empty_transcript':
      return d.had_interim_text
        ? `The recogniser produced words for ${heard} and then returned nothing for it`
        : `The recogniser found no words in ${heard}`;
    case 'too_short':
      return d.text
        ? `Transcribed as “${d.text}”, under the minimum length, and discarded`
        : `${heard}, too short to be a turn`;
    case 'stt_timeout':
      return `The recogniser never answered ${heard}; it was restarted`;
    default:
      return `${heard} made no turn (${d.reason})`;
  }
}
