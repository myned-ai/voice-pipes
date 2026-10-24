import { createContext, useContext } from "react";
import type { ComponentMetricDescriptor } from "../../lib/transport/types";

/**
 * Cache of `ComponentMetricDescriptor`s for the current session.
 *
 * Descriptors arrive once on `session_ready.negotiated.metric_descriptors`
 * and are stable for the life of the session — populated by
 * `usePlaygroundSession`'s `handleSessionReadyControl`. Consumers that
 * render snapshot tables look them up by `component_id`; absent
 * descriptors fall back to displaying the raw component id with no
 * typed fields (the snapshot's `extras` keys aren't enumerated without
 * a descriptor).
 */
export const SessionDescriptorsContext = createContext<ComponentMetricDescriptor[]>([]);

/** Hook accessor. Always returns the current cache (defaults to `[]`). */
export function useSessionDescriptors(): ComponentMetricDescriptor[] {
  return useContext(SessionDescriptorsContext);
}

/** Lookup helper: find a descriptor by `component_id`. */
export function findDescriptor(
  descriptors: ComponentMetricDescriptor[],
  componentId: string,
): ComponentMetricDescriptor | undefined {
  return descriptors.find((d) => d.component_id === componentId);
}
