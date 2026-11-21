/**
 * Runtime feature flags fetched from the Admin BFF (`GET /api/v1/features`).
 *
 * All features default to enabled (`true`) so local and self-hosted setups
 * work without extra configuration.  Hosted deployments can toggle flags via
 * `ADMIN_FEATURE_BENCHMARKS=false` / `ADMIN_FEATURE_TEST_SUITES=false` on the
 * backend (or build-time `VITE_FEATURE_*=false` overrides).
 */
import axios from "axios";
import { API_BASE } from "../config/api";

export type AdminFeatures = {
  benchmarks: boolean;
  test_suites: boolean;
};

function parseBuildFlag(raw: string | undefined, fallback: boolean): boolean {
  if (!raw || !raw.trim()) return fallback;
  return !["0", "false", "no", "off"].includes(raw.trim().toLowerCase());
}

export const DEFAULT_ADMIN_FEATURES: AdminFeatures = {
  benchmarks: parseBuildFlag(import.meta.env.VITE_FEATURE_BENCHMARKS, true),
  test_suites: parseBuildFlag(import.meta.env.VITE_FEATURE_TEST_SUITES, true),
};

export async function fetchAdminFeatures(): Promise<AdminFeatures> {
  try {
    const res = await axios.get<{ data?: Partial<AdminFeatures> }>(`${API_BASE}/api/v1/features`, {
      withCredentials: true,
    });
    const d = res.data?.data;
    return {
      benchmarks: typeof d?.benchmarks === "boolean" ? d.benchmarks : DEFAULT_ADMIN_FEATURES.benchmarks,
      test_suites: typeof d?.test_suites === "boolean" ? d.test_suites : DEFAULT_ADMIN_FEATURES.test_suites,
    };
  } catch {
    return DEFAULT_ADMIN_FEATURES;
  }
}
