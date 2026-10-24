// Segment bar under a turn's audio player: one segment per TTS fragment,
// sized by duration, with the client's underrun silences and barge-in cut
// drawn in. Click seeks the shared <audio>; a playhead follows it.
//
// The bar answers the questions the old 373-chip grid could not: *where* in
// the response did playback break, how much was lost, and what was playing
// at the moment the user interrupted. It shows the track the player is
// actually on — server (ideal) or as-heard — so what you click is what you
// hear. Geometry lives in `heardTimeline.ts`, which mirrors the backend's
// as-heard splice.

import { useEffect, useMemo, useRef, useState } from 'react';
import type { FragmentInfo } from './types';
import {
  formatMs,
  gapAt,
  layoutTimeline,
  segmentAt,
  type HeardModel,
  type TimelineMode,
} from './heardTimeline';
import { fragmentSnippet } from './fragments';

export interface FragmentTimelineProps {
  fragments: FragmentInfo[];
  fragmentTexts?: Record<string, string>;
  heard: HeardModel;
  /** Which track the shared audio element is playing. */
  mode: TimelineMode;
  /** Set when the player is on a single fragment file rather than a track:
   *  the playhead is offset to that fragment, and clicking selects. */
  activeFragment: number | null;
  audioRef: React.RefObject<HTMLAudioElement | null>;
  onSelectFragment: (idx: number) => void;
  isDark: boolean;
}

const BAR_HEIGHT = 16;

export const FragmentTimeline = ({
  fragments,
  fragmentTexts,
  heard,
  mode,
  activeFragment,
  audioRef,
  onSelectFragment,
  isDark,
}: FragmentTimelineProps) => {
  const layout = useMemo(() => layoutTimeline(fragments, mode, heard), [fragments, mode, heard]);
  const barRef = useRef<HTMLDivElement>(null);
  const [playheadMs, setPlayheadMs] = useState<number | null>(null);
  const [hover, setHover] = useState<{ x: number; trackMs: number } | null>(null);

  // Playhead. `timeupdate` alone is ~4 Hz, visibly steppy on a 15 s bar;
  // run a rAF loop while playing and fall back to events otherwise. In
  // single-fragment mode the element's clock starts at that fragment, so
  // offset it to the fragment's segment on the (server) layout.
  useEffect(() => {
    const el = audioRef.current;
    if (!el) return;
    const fragmentStart = activeFragment !== null ? layout.segments[activeFragment]?.startMs ?? 0 : 0;
    const read = () => setPlayheadMs(fragmentStart + el.currentTime * 1000);
    let raf = 0;
    const tick = () => {
      read();
      if (!el.paused && !el.ended) raf = requestAnimationFrame(tick);
    };
    const onPlay = () => { cancelAnimationFrame(raf); tick(); };
    const onStop = () => { cancelAnimationFrame(raf); read(); };
    el.addEventListener('play', onPlay);
    el.addEventListener('pause', onStop);
    el.addEventListener('ended', onStop);
    el.addEventListener('seeked', read);
    el.addEventListener('timeupdate', read);
    read();
    if (!el.paused) tick();
    return () => {
      cancelAnimationFrame(raf);
      el.removeEventListener('play', onPlay);
      el.removeEventListener('pause', onStop);
      el.removeEventListener('ended', onStop);
      el.removeEventListener('seeked', read);
      el.removeEventListener('timeupdate', read);
    };
  }, [audioRef, activeFragment, layout]);

  const total = layout.totalMs;
  if (fragments.length === 0 || total <= 0) return null;

  const pct = (ms: number) => `${Math.min(100, Math.max(0, (ms / total) * 100))}%`;

  const trackMsAt = (clientX: number): number => {
    const rect = barRef.current?.getBoundingClientRect();
    const width = rect && rect.width > 0 ? rect.width : 1;
    const x = rect ? clientX - rect.left : clientX;
    return Math.min(total, Math.max(0, (x / width) * total));
  };

  const handleClick = (e: React.MouseEvent<HTMLDivElement>) => {
    const trackMs = trackMsAt(e.clientX);
    if (activeFragment !== null) {
      const seg = segmentAt(layout, trackMs);
      if (seg) onSelectFragment(seg.idx);
      return;
    }
    const el = audioRef.current;
    if (!el) return;
    el.currentTime = trackMs / 1000;
    el.play().catch(() => {/* autoplay blocked */});
  };

  const handleMove = (e: React.MouseEvent<HTMLDivElement>) => {
    const rect = barRef.current?.getBoundingClientRect();
    setHover({ x: rect ? e.clientX - rect.left : e.clientX, trackMs: trackMsAt(e.clientX) });
  };

  // Tooltip text for the hovered position.
  const hoverLabel = (() => {
    if (!hover) return null;
    const gap = gapAt(layout, hover.trackMs);
    if (gap) return `underrun · ${formatMs(gap.silenceMs)} silent · at ${formatMs(gap.atAudioMs)} of audio`;
    const seg = segmentAt(layout, hover.trackMs);
    if (!seg) return layout.cutMs !== null && hover.trackMs >= layout.cutMs ? 'after barge-in' : null;
    const text = fragmentTexts?.[seg.kind];
    const dur = fragments[seg.idx].duration_ms;
    const base = `#${seg.idx + 1} · ${formatMs(fragments[seg.idx].offset_ms)} · ${formatMs(dur)}`;
    const cut = seg.afterCut ? ' · not delivered (after barge-in)' : '';
    return text ? `${base} · “${fragmentSnippet(text, 6)}”${cut}` : `${base}${cut}`;
  })();

  const segFill = (i: number, afterCut: boolean, active: boolean) => {
    if (active) return isDark ? 'rgba(167,139,250,0.95)' : 'rgba(124,58,237,0.9)';
    if (afterCut) return isDark ? 'rgba(148,163,184,0.18)' : 'rgba(100,116,139,0.18)';
    return i % 2 === 0
      ? (isDark ? 'rgba(139,92,246,0.55)' : 'rgba(139,92,246,0.5)')
      : (isDark ? 'rgba(139,92,246,0.35)' : 'rgba(139,92,246,0.3)');
  };
  const gapFill = isDark ? 'rgba(245,158,11,0.85)' : 'rgba(217,119,6,0.85)';
  const cutStroke = isDark ? '#f87171' : '#dc2626';
  const playheadStroke = isDark ? '#f8fafc' : '#0f172a';
  const muted = isDark ? 'text-slate-500' : 'text-gray-500';

  const gapCount = heard.gaps.length;
  const silenceTotal = heard.gaps.reduce((s, g) => s + g.silence_ms, 0);

  return (
    <div className="space-y-0.5" data-testid="fragment-timeline">
      <div className="relative" style={{ paddingTop: hoverLabel ? 18 : 0 }}>
        {hoverLabel && hover && (
          <div
            className={`absolute top-0 text-[10px] font-mono whitespace-nowrap px-1.5 py-0.5 rounded pointer-events-none z-10 ${
              isDark ? 'bg-slate-900 text-slate-200 ring-1 ring-slate-700' : 'bg-white text-gray-800 ring-1 ring-gray-200 shadow-sm'
            }`}
            style={{ left: hover.x, transform: 'translateX(-50%)' }}
          >
            {hoverLabel}
          </div>
        )}
        <div
          ref={barRef}
          role="img"
          aria-label={`${fragments.length} fragments over ${formatMs(total)}${gapCount ? `, ${gapCount} underruns` : ''}${layout.cutMs !== null ? ', interrupted' : ''}`}
          data-mode={mode}
          onClick={handleClick}
          onMouseMove={handleMove}
          onMouseLeave={() => setHover(null)}
          className={`relative w-full rounded overflow-hidden cursor-pointer ${isDark ? 'bg-slate-900/70' : 'bg-gray-100'}`}
          style={{ height: BAR_HEIGHT }}
        >
          {layout.segments.map(seg => (
            <div
              key={seg.kind}
              data-seg={seg.idx}
              className="absolute top-0 bottom-0"
              style={{
                left: pct(seg.startMs),
                width: pct(seg.endMs - seg.startMs),
                background: segFill(seg.idx, seg.afterCut, activeFragment === seg.idx),
              }}
            />
          ))}
          {layout.gaps.map((g, i) => (
            g.endMs > g.startMs ? (
              <div
                key={`gap-${i}`}
                data-gap={i}
                className="absolute top-0 bottom-0"
                style={{ left: pct(g.startMs), width: pct(g.endMs - g.startMs), background: gapFill }}
              />
            ) : (
              <div
                key={`gap-${i}`}
                data-gap={i}
                className="absolute top-0"
                style={{ left: pct(g.startMs), width: 2, height: BAR_HEIGHT / 2, background: gapFill, transform: 'translateX(-1px)' }}
              />
            )
          ))}
          {layout.cutMs !== null && (
            <div
              data-cut=""
              className="absolute top-0 bottom-0"
              style={{ left: pct(layout.cutMs), width: 2, background: cutStroke, transform: 'translateX(-1px)' }}
            />
          )}
          {playheadMs !== null && playheadMs > 0 && (
            <div
              data-playhead=""
              className="absolute top-0 bottom-0 pointer-events-none"
              style={{ left: pct(playheadMs), width: 2, background: playheadStroke, transform: 'translateX(-1px)' }}
            />
          )}
        </div>
      </div>
      <div className={`flex flex-wrap items-center gap-x-3 text-[10px] font-mono ${muted}`}>
        <span>
          <span className="inline-block w-2 h-2 rounded-sm align-middle mr-1" style={{ background: segFill(0, false, false) }} />
          {fragments.length} fragments
        </span>
        {gapCount > 0 && (
          <span title={mode === 'heard' ? 'Drawn at their real length on this track' : 'Marked where they occurred; the server track has no silence in it'}>
            <span className="inline-block w-2 h-2 rounded-sm align-middle mr-1" style={{ background: gapFill }} />
            {gapCount} underrun{gapCount === 1 ? '' : 's'} · {formatMs(silenceTotal)} silent
          </span>
        )}
        {layout.cutMs !== null && (
          <span>
            <span className="inline-block w-0.5 h-2.5 align-middle mr-1" style={{ background: cutStroke }} />
            barge-in at {formatMs(heard.cutMs ?? 0)}
          </span>
        )}
      </div>
    </div>
  );
};
