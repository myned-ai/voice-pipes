import type {
  ComponentMetricDescriptor,
  TurnComponentMetrics,
  TurnMetricsSnapshot,
} from "../../lib/transport/types";
import { formatMetric } from "../../lib/metrics/format";
import { findDescriptor } from "../../pages/playground/SessionDescriptorsContext";

// ── Generic per-turn metrics table ─────────────────────────────
//
// Renders one section per `TurnComponentMetrics` in `snapshot.components`
// (insertion order = pipeline order). The set of rows under each section
// is driven entirely by the matching `ComponentMetricDescriptor.fields`
// — there is *no* hardcoded knowledge of stt/llm/tts here. Adding a new
// pipeline component is zero edits to this file: as soon as the server
// advertises a descriptor for it on `session_ready`, this table renders
// its rows.
//
// When a component has no descriptor (e.g. an older server that hasn't
// been rebuilt yet, or a brand-new component before the descriptor
// fan-out lands), the section still renders the header (provider /
// latency / cold-start / error / running indicator) but skips the
// per-field rows — extras keys can't be enumerated without the
// descriptor's typed key list.

export interface TurnComponentMetricsTableProps {
  snapshot: TurnMetricsSnapshot;
  /** From `SessionDescriptorsContext` (session_ready cache). */
  descriptors: ComponentMetricDescriptor[];
  /** Optional Tailwind classes appended to the wrapper. */
  className?: string;
}

export function TurnComponentMetricsTable({
  snapshot,
  descriptors,
  className,
}: TurnComponentMetricsTableProps) {
  if (snapshot.components.length === 0) {
    return (
      <div className={`text-xs text-slate-500 italic ${className ?? ""}`}>
        No per-component metrics for this turn.
      </div>
    );
  }

  return (
    <div className={`flex flex-col gap-3 ${className ?? ""}`}>
      {snapshot.components.map((component) => (
        <ComponentSection
          key={component.component_id}
          component={component}
          descriptor={findDescriptor(descriptors, component.component_id)}
        />
      ))}
    </div>
  );
}

interface ComponentSectionProps {
  component: TurnComponentMetrics;
  descriptor: ComponentMetricDescriptor | undefined;
}

function ComponentSection({ component, descriptor }: ComponentSectionProps) {
  const label = descriptor?.label ?? component.component_id;
  const isRunning = component.finished_at_ms === undefined;

  return (
    <div
      className="rounded border border-slate-700/60 bg-slate-800/40 px-3 py-2 text-xs"
      data-testid={`component-section-${component.component_id}`}
    >
      <div className="flex flex-wrap items-baseline justify-between gap-x-4 gap-y-1">
        <div className="flex items-baseline gap-2">
          <span className="font-semibold text-slate-200">{label}</span>
          <span className="font-mono text-[10px] uppercase text-slate-500">
            {component.kind}
          </span>
          {component.provider && (
            <span className="text-slate-400">{component.provider}</span>
          )}
          {component.cold_start && (
            <span className="rounded bg-amber-900/60 px-1.5 py-0.5 text-[10px] text-amber-300">
              cold start
            </span>
          )}
          {isRunning && (
            <span
              className="rounded bg-sky-900/60 px-1.5 py-0.5 text-[10px] text-sky-300"
              data-testid={`running-indicator-${component.component_id}`}
            >
              running
            </span>
          )}
        </div>
        <span className="font-mono text-slate-300">
          {component.latency_ms != null
            ? `${Math.round(component.latency_ms)} ms`
            : "-"}
        </span>
      </div>

      {component.error && (
        <div
          className="mt-2 rounded bg-red-950/60 px-2 py-1 text-red-300"
          data-testid={`error-banner-${component.component_id}`}
        >
          {component.error}
        </div>
      )}

      {descriptor && descriptor.fields.length > 0 && (
        <table className="mt-2 w-full table-auto text-left">
          <tbody>
            {descriptor.fields.map((field) => {
              const value = component.extras?.[field.key];
              return (
                <tr key={field.key} className="border-t border-slate-700/40">
                  <td className="py-0.5 pr-2 text-slate-400">{field.label}</td>
                  <td className="py-0.5 font-mono text-slate-200">
                    {formatMetric(value, field.unit, field.format)}
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      )}
    </div>
  );
}
