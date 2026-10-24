import { useEffect, useLayoutEffect, useRef } from "react";
import { useTheme } from "../../contexts/ThemeContext";

interface Props {
  /** 0..1 RMS level */
  level: number;
  /** Label shown below the visualiser */
  label?: string;
  /** Named color variant */
  colorVariant?: "indigo" | "emerald" | "red";
  /** Bar count */
  bars?: number;
}

const COLOR_MAP: Record<"indigo" | "emerald" | "red", { r: number; g: number; b: number }> = {
  indigo:  { r: 99,  g: 102, b: 241 },
  emerald: { r: 52,  g: 211, b: 153 },
  red:     { r: 248, g: 113, b: 113 },
};

export default function AudioVisualiser({
  level,
  label,
  colorVariant = "indigo",
  bars = 32,
}: Props) {
  const { isDark } = useTheme();
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const wrapRef = useRef<HTMLDivElement>(null);
  const historyRef = useRef<number[]>(new Array(bars).fill(0));
  const rafRef = useRef(0);
  // Latest `level` for the RAF draw loop.  Synced in a layout effect (not
  // during render): it runs synchronously after commit, before the browser can
  // deliver the next animation frame, so every frame sees the newest level.
  const levelRef = useRef(0);
  useLayoutEffect(() => {
    levelRef.current = level;
  }, [level]);

  // Keep canvas backing store in sync with CSS layout size + devicePixelRatio.
  // NOTE: do NOT call ctx.scale() here — it's cumulative and stacks on every
  // resize call. Instead we use setTransform in the draw loop each frame.
  useEffect(() => {
    const canvas = canvasRef.current;
    const wrap = wrapRef.current;
    if (!canvas || !wrap) return;

    const resync = () => {
      const dpr = window.devicePixelRatio || 1;
      const cssW = wrap.clientWidth;
      const cssH = wrap.clientHeight;
      const backingW = Math.round(cssW * dpr);
      const backingH = Math.round(cssH * dpr);
      if (canvas.width !== backingW || canvas.height !== backingH) {
        canvas.width = backingW;
        canvas.height = backingH;
        // No ctx.scale() here — draw loop uses setTransform each frame.
      }
    };

    resync();
    const ro = new ResizeObserver(resync);
    ro.observe(wrap);
    return () => ro.disconnect();
  }, []);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;

    const { r, g, b } = COLOR_MAP[colorVariant] ?? COLOR_MAP.indigo;

    const draw = () => {
      const dpr = window.devicePixelRatio || 1;
      // Reset transform each frame — avoids stale/cumulative scale from resizes.
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);

      const history = historyRef.current;
      history.shift();
      history.push(levelRef.current);

      // Draw in CSS pixel space (transform above handles DPR mapping).
      const w = canvas.width / dpr;
      const h = canvas.height / dpr;
      const barW = w / bars;
      const gap = Math.max(1, barW * 0.15);

      ctx.clearRect(0, 0, w, h);

      for (let i = 0; i < bars; i++) {
        const val = history[i];
        const jitter = val > 0.01 ? 0.75 + Math.random() * 0.25 : 1;
        const minH = 3;
        const barH = val > 0.005
          ? Math.max(minH, val * jitter * h * 0.88)
          : minH;
        const x = i * barW + gap / 2;
        const y = (h - barH) / 2;

        // Active bars are vibrant; idle bars are subtle
        const alpha = val > 0.005
          ? Math.max(0.55, Math.min(1, val * 4))
          : isDark ? 0.12 : 0.18;

        ctx.fillStyle = `rgba(${r}, ${g}, ${b}, ${alpha})`;
        ctx.beginPath();
        if (typeof ctx.roundRect === "function") {
          ctx.roundRect(x, y, barW - gap, barH, 2);
        } else {
          ctx.rect(x, y, barW - gap, barH);
        }
        ctx.fill();
      }

      rafRef.current = requestAnimationFrame(draw);
    };

    rafRef.current = requestAnimationFrame(draw);
    return () => cancelAnimationFrame(rafRef.current);
  }, [bars, colorVariant, isDark]);

  return (
    <div className="flex flex-col items-center gap-1">
      <div ref={wrapRef} className="w-full h-10">
        <canvas
          ref={canvasRef}
          style={{ width: "100%", height: "100%" }}
        />
      </div>
      {label && (
        <span className={`text-[10px] uppercase tracking-wider ${isDark ? "text-slate-500" : "text-gray-400"}`}>
          {label}
        </span>
      )}
    </div>
  );
}
