import { useCallback, useEffect, useState, type FormEvent, type ReactNode } from "react";
import { KeyRound } from "lucide-react";
import { useTheme } from "../contexts/ThemeContext";
import { checkAuth, installAdminAuth, login, UNAUTHORIZED_EVENT } from "../lib/adminAuth";

/**
 * Blocks the whole app until the BFF accepts a token.
 *
 * On mount it asks `/api/v1/auth/check`.  If the BFF runs open
 * (`ADMIN_UI_AUTH_REQUIRED=false`) or the stored token is valid, children
 * render immediately.  Any later 401 (token rotated, cookie expired) flips the
 * gate back to the form via the `vp-admin-unauthorized` event.
 */
type GateState = "checking" | "locked" | "open" | "unreachable";

export function AuthGate({ children }: { children: ReactNode }) {
  const { isDark } = useTheme();
  const [state, setState] = useState<GateState>("checking");
  const [token, setToken] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const probe = useCallback(async () => {
    const res = await checkAuth();
    if (res === null) setState("unreachable");
    else setState(res.authenticated ? "open" : "locked");
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

  if (state === "open") return <>{children}</>;

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
        {state === "locked" && (
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
              Set <code>ADMIN_UI_TOKEN</code> on the admin backend and paste it here. The token is kept for this tab
              only.
            </p>
          </form>
        )}
      </div>
    </div>
  );
}
