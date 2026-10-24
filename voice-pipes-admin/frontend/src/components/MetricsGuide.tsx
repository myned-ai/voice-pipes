import { useTheme } from "../contexts/ThemeContext";
import { METRIC_DOCS } from "./metricsDocs";

/** Shared metrics explanation body — used in both Playground legend popup and SessionDetail telemetry sidebar. */
export function MetricsGuide() {
  const { isDark } = useTheme();

  return (
    <>
      {/* Pipeline order */}
      <div className={`text-[11px] mb-3 leading-relaxed ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
        <strong>Pipeline order:</strong> VAD end-of-speech → STT → <em>utterance commit (t=0)</em> → LLM streams tokens → TTS synthesises audio → browser plays.
        {" "}All server timings start at t=0. STT runs <em>before</em> t=0 and only appears in <strong>E2E</strong>.
        {" "}That is why <strong>TTFT &lt; TTFB</strong>: text precedes audio in the pipeline.
      </div>

      {/* Per-metric entries */}
      <div className="grid grid-cols-1 gap-2">
        {METRIC_DOCS.map((m) => (
          <div key={m.label} className="flex gap-2.5">
            <span className={`shrink-0 w-2 h-2 rounded-sm mt-1 ${m.dot}`} />
            <div>
              <span className={`text-[11px] font-semibold font-mono ${m.color || (isDark ? 'text-slate-300' : 'text-gray-700')}`}>
                {m.label}
              </span>
              <span className={`text-[11px] ml-1.5 ${isDark ? 'text-slate-400' : 'text-gray-600'}`}>
                — {m.title}
              </span>
              <div className={`text-[10px] leading-relaxed mt-0.5 ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>
                {m.desc}
              </div>
            </div>
          </div>
        ))}
      </div>

      {/* Footer relationships */}
      <div className={`mt-3 pt-2 border-t text-[10px] leading-relaxed ${isDark ? 'border-slate-700 text-slate-500' : 'border-gray-200 text-gray-400'}`}>
        <strong>Perceived latency</strong> = STT + TTFB (server) = E2E (client). Both measure the same gap — choose whichever is easier to reason about.
        <br />
        <strong>TTFB relationship:</strong> TTFB ≈ TTFT + TTS. To cut TTFB, reduce LLM TTFT (faster model / smaller prompt) or TTS startup (smaller first chunk).
        <br />
        <strong>Total vs TTFB:</strong> the difference is how long it takes to stream the rest of the response after the first audio byte. Long replies will have Total ≫ TTFB.
      </div>
    </>
  );
}
