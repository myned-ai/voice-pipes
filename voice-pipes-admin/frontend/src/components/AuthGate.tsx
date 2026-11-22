import { useCallback, useEffect, useState, type FormEvent, type ReactNode } from "react";
import { KeyRound, LogIn, UserCheck } from "lucide-react";
import { API_BASE } from "../config/api";
import { useTheme } from "../contexts/ThemeContext";
import {
  checkAuth,
  installAdminAuth,
  login,
  loginGuest,
  UNAUTHORIZED_EVENT,
  type AuthCheck,
} from "../lib/adminAuth";

/**
 * Blocks the whole app until the BFF accepts a credential (token, HF OAuth
 * session cookie, or isolated guest session cookie).
 *
 * On mount it asks `/api/v1/auth/check`.  If the BFF runs open
 * (`ADMIN_UI_AUTH_REQUIRED=false`) or the caller is authenticated, children
 * render immediately.  Any later 401 flips the gate back to the login view via
 * the `vp-admin-unauthorized` event.
 */
type GateState = "checking" | "locked" | "open" | "unreachable";

export function AuthGate({ children }: { children: ReactNode }) {
  const { isDark } = useTheme();
  const [state, setState] = useState<GateState>("checking");
  const [authInfo, setAuthInfo] = useState<AuthCheck | null>(null);
  const [token, setToken] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [awaitingOAuth, setAwaitingOAuth] = useState(false);

  const probe = useCallback(async () => {
    const res = await checkAuth();
    if (res === null) {
      setState("unreachable");
    } else {
      setAuthInfo(res);
      setState(res.authenticated ? "open" : "locked");
    }
  }, []);

  useEffect(() => {
    installAdminAuth();
    // The probe resolves asynchronously (network round-trip), so the state
    // update never lands in the same render pass as this effect; scheduling
    // it as a task also makes that explicit to React's lint rules.
    const initial = setTimeout(() => void probe(), 0);
    const onUnauthorized = () => setState("locked");
    window.addEventListener(UNAUTHORIZED_EVENT, onUnauthorized);
    return () => {
      clearTimeout(initial);
      window.removeEventListener(UNAUTHORIZED_EVENT, onUnauthorized);
    };
  }, [probe]);

  useEffect(() => {
    if (state !== "locked") return;
    const onFocus = () => void probe();
    window.addEventListener("focus", onFocus);
    let ch: BroadcastChannel | null = null;
    if (typeof BroadcastChannel !== "undefined") {
      ch = new BroadcastChannel("vp-admin-auth");
      ch.onmessage = () => void probe();
    }
    return () => {
      window.removeEventListener("focus", onFocus);
      ch?.close();
    };
  }, [probe, state]);

  useEffect(() => {
    if (state !== "locked" || !awaitingOAuth) return;
    // Storage Partitioning in modern browsers isolates BroadcastChannel between
    // top-level *.hf.space popups and huggingface.co iframes, and closing the
    // popup may return focus to the parent page without firing iframe focus.
    // Poll briefly while OAuth is in flight so the iframe unlocks immediately.
    const interval = setInterval(() => void probe(), 1000);
    const timeout = setTimeout(() => setAwaitingOAuth(false), 120_000);
    return () => {
      clearInterval(interval);
      clearTimeout(timeout);
    };
  }, [awaitingOAuth, probe, state]);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    const ok = await login(token);
    setBusy(false);
    if (ok) {
      setToken("");
      setState("open");
    } else {
      setError("Token rejected. Check ADMIN_UI_TOKEN on the admin backend.");
    }
  };

  const handleGuestLogin = async () => {
    setBusy(true);
    setError(null);
    const ok = await loginGuest();
    setBusy(false);
    if (ok) {
      setState("open");
    } else {
      setError("Could not start an isolated guest session.");
    }
  };

  const handleHfLogin = () => {
    const loginPath = authInfo?.hf_oauth_url ?? "/api/v1/auth/hf/login";
    const targetUrl = `${API_BASE}${loginPath}`;
    // When embedded inside the Hugging Face Space iframe, OAuth must open in a
    // top-level window so Hugging Face's login page is not blocked by X-Frame-Options.
    if (window.top && window.top !== window.self) {
      const sep = targetUrl.includes("?") ? "&" : "?";
      const popupUrl = `${targetUrl}${sep}popup=1`;
      setAwaitingOAuth(true);
      window.open(popupUrl, "vp_hf_oauth", "width=520,height=680,popup=yes,noopener");
    } else {
      window.location.assign(targetUrl);
    }
  };

  if (state === "open") return <>{children}</>;

  const mode = authInfo?.auth_mode ?? "token";
  const allowGuest = Boolean(authInfo?.allow_guest);

  const panel = isDark ? "bg-slate-900 border-slate-800 text-white" : "bg-white border-gray-200 text-gray-900";
  const input = isDark
    ? "bg-slate-950 border-slate-700 text-white placeholder-slate-500"
    : "bg-gray-50 border-gray-300 text-gray-900 placeholder-gray-400";

  return (
    <div className={`flex h-screen w-full items-center justify-center ${isDark ? "bg-slate-950" : "bg-gray-100"}`}>
      <div className={`w-full max-w-sm rounded-xl border p-6 shadow-lg ${panel}`}>
        <div className="mb-4 flex items-center gap-2">
          <KeyRound size={20} className="text-indigo-400" />
          <h1 className="text-lg font-semibold">Voice-Pipes Admin</h1>
        </div>
        {state === "checking" && <p className="text-sm opacity-70">Checking access…</p>}
        {state === "unreachable" && (
          <div className="space-y-3">
            <p className="text-sm opacity-80">
              The admin backend is not reachable. Start it and retry.
            </p>
            <button
              type="button"
              onClick={() => {
                setState("checking");
                void probe();
              }}
              className="rounded-md bg-indigo-600 px-3 py-2 text-sm font-medium text-white hover:bg-indigo-500"
            >
              Retry
            </button>
          </div>
        )}
        {state === "locked" && mode === "token" && (
          <form onSubmit={submit} className="space-y-3">
            <label className="block text-sm opacity-80" htmlFor="admin-ui-token">
              Admin token
            </label>
            <input
              id="admin-ui-token"
              type="password"
              autoComplete="current-password"
              autoFocus
              value={token}
              onChange={(e) => setToken(e.target.value)}
              placeholder="ADMIN_UI_TOKEN"
              className={`w-full rounded-md border px-3 py-2 text-sm outline-none focus:ring-2 focus:ring-indigo-500 ${input}`}
            />
            {error && (
              <p role="alert" className="text-sm text-red-400">
                {error}
              </p>
            )}
            <button
              type="submit"
              disabled={busy || !token.trim()}
              className="w-full rounded-md bg-indigo-600 px-3 py-2 text-sm font-medium text-white hover:bg-indigo-500 disabled:opacity-50"
            >
              {busy ? "Signing in…" : "Sign in"}
            </button>
            <p className="text-xs opacity-60">
              Paste <code>ADMIN_UI_TOKEN</code> from <code>voice-pipes-admin/.env</code> (or backend environment). The token is kept for this tab
              only.
            </p>
          </form>
        )}
        {state === "locked" && mode !== "token" && (
          <div className="space-y-3">
            <p className="text-sm opacity-80">
              Sign in to access your isolated Voice-Pipes workspace. Your sessions, audio recordings, and metrics are
              strictly private to your account.
            </p>
            {mode === "hf_oauth" && (
              <p className="text-xs opacity-70">
                Only an anonymous Hugging Face account ID is shared—used solely to separate your recordings and
                per-user session slot from other visitors (no email, repos, or API tokens).
              </p>
            )}
            {error && (
              <p role="alert" className="text-sm text-red-400">
                {error}
              </p>
            )}
            {mode === "hf_oauth" && (
              <button
                type="button"
                onClick={handleHfLogin}
                disabled={busy}
                className="flex w-full items-center justify-center gap-2 rounded-md bg-indigo-600 px-3 py-2.5 text-sm font-medium text-white hover:bg-indigo-500 disabled:opacity-50"
              >
                <LogIn size={16} />
                Sign in with Hugging Face
              </button>
            )}
            {allowGuest && (
              <button
                type="button"
                onClick={() => void handleGuestLogin()}
                disabled={busy}
                className="flex w-full items-center justify-center gap-2 rounded-md border border-indigo-500/40 bg-indigo-500/10 px-3 py-2.5 text-sm font-medium text-indigo-300 hover:bg-indigo-500/20 disabled:opacity-50"
              >
                <UserCheck size={16} />
                {busy ? "Starting workspace…" : "Continue with Isolated Guest Session"}
              </button>
            )}
            <p className="text-xs opacity-60">
              Saved recordings and logs are automatically deleted after inactivity to keep storage clean.
            </p>
          </div>
        )}
      </div>
    </div>
  );
}
