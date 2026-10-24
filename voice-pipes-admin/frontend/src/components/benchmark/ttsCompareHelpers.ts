/**
 * Helper for `TtsCompareTable`: extracts the per-variant entry slice
 * for a given scenario, dropping variants that recorded no entries.
 *
 * Lives in its own file so the table component file only exports the
 * default React component (required by the project's react-refresh
 * lint rule).
 */

import type { TtsEntryResult, VariantResults } from './types';

export function buildCompareVariants(
  variantResults: VariantResults[],
  scenarioId: string,
): { variantId: string; variantName: string; entries: TtsEntryResult[] }[] {
  return variantResults
    .map((vr) => {
      const sc = vr.scenarios.find((s) => s.scenario_id === scenarioId);
      return {
        variantId: vr.variant_id,
        variantName: vr.variant_name,
        entries: sc?.tts_entries ?? [],
      };
    })
    .filter((v) => v.entries.length > 0);
}
