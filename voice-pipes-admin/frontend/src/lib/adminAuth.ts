/**
 * The admin UI's own credential for the BFF.
 *
 * The BFF (`voice-pipes-admin/backend`) requires `ADMIN_UI_TOKEN` on every
 * request.  This module is the single place the browser keeps that token and
 * the single place it is attached:
 *
 * - `axios` gets an `Authorization: Bearer` default header plus a response
 *   interceptor that turns any 401 into a `vp-admin-unauthorized` window event
 *   (the `AuthGate` listens and shows the login form).
 * - `EventSource` and `<audio src>` cannot set headers, so `login()` also
 *   exchanges the token for an HttpOnly cookie via `POST /api/v1/auth/login`;
 *   those requests must be sent with credentials (`withCredentials: true` on
 *   `EventSource`; `<audio>` sends cookies by default).  Frontend and BFF must
 *   therefore be same-site (same eTLD+1) or served from one origin.
 *
 * The token lives in `sessionStorage` (per tab, cleared on close), never in a
 * URL.  The worker credential (`VP_CLIENT_TOKEN`) never reaches the browser:
 * the Playground opens `/api/v1/playground/ws` on the BFF, authenticated by the
 * cookie, and the BFF presents the worker credential upstream.
 */
import axios from "axios";
import { API_BASE } from "../config/api";

const STORAGE_KEY = "vp_admin_ui_token";
export const UNAUTHORIZED_EVENT = "vp-admin-unauthorized";

export function getToken(): string {
  try {
    return sessionStorage.getItem(STORAGE_KEY) ?? "";
  } catch {
    return "";
  }
}

function storeToken(token: string): void {
  try {
    if (token) sessionStorage.setItem(STORAGE_KEY, token);
    else sessionStorage.removeItem(STORAGE_KEY);
  } catch {
    /* sessionStorage unavailable (tests, sandbox) — header still set below */
  }
}

function applyAxiosHeader(token: string): void {
  if (token) axios.defaults.headers.common["Authorization"] = `Bearer ${token}`;
  else delete axios.defaults.headers.common["Authorization"];
}

export type AuthCheck = { auth_required: boolean; authenticated: boolean };

/** Where earlier builds of the console kept the worker credential
 *  (`VP_CLIENT_TOKEN`). The browser no longer receives it: the Playground
 *  goes through the backend's proxy, which presents it. A copy left behind
 *  by an older build is removed. */
const LEGACY_CLIENT_TOKEN_KEY = "vp_client_token";

function forgetLegacyClientToken(): void {
  try {
    localStorage.removeItem(LEGACY_CLIENT_TOKEN_KEY);
  } catch {
    /* localStorage unavailable — nothing was stored there either */
  }
}

/** Ask the BFF whether a token is needed and whether ours works. Never throws:
 *  a BFF that is down is reported as `null`. */
export async function checkAuth(): Promise<AuthCheck | null> {
  try {
    const res = await axios.get<{ data: AuthCheck }>(`${API_BASE}/api/v1/auth/check`, {
      withCredentials: true,
    });
    const { auth_required, authenticated } = res.data.data;
    return { auth_required, authenticated };
  } catch {
    return null;
  }
}

/** Validate `token` against the BFF; on success persist it, set the axios
 *  header and obtain the cookie.  Returns false on a wrong token. */
export async function login(token: string): Promise<boolean> {
  const trimmed = token.trim();
  if (!trimmed) return false;
  try {
    await axios.post(
      `${API_BASE}/api/v1/auth/login`,
      { token: trimmed },
      { withCredentials: true, headers: { Authorization: `Bearer ${trimmed}` } },
    );
  } catch {
    return false;
  }
  storeToken(trimmed);
  applyAxiosHeader(trimmed);
  return true;
}

export async function logout(): Promise<void> {
  try {
    await axios.post(`${API_BASE}/api/v1/auth/logout`, {}, { withCredentials: true });
  } catch {
    /* best effort */
  }
  storeToken("");
  applyAxiosHeader("");
}

let installed = false;

/** Idempotent. Call once at app start (before any request): restores a stored
 *  token into axios, sends cookies on every BFF request, and broadcasts 401s. */
export function installAdminAuth(): void {
  if (installed) return;
  installed = true;
  forgetLegacyClientToken();
  axios.defaults.withCredentials = true;
  applyAxiosHeader(getToken());
  axios.interceptors.response.use(
    (r) => r,
    (err) => {
      const status = err?.response?.status;
      const url: string = err?.config?.url ?? "";
      if (status === 401 && url.startsWith(API_BASE) && !url.includes("/api/v1/auth/")) {
        storeToken("");
        applyAxiosHeader("");
        window.dispatchEvent(new CustomEvent(UNAUTHORIZED_EVENT));
      }
      return Promise.reject(err);
    },
  );
}
