// The admin UI's credential handling for the BFF.
//
// Contract: a stored token is attached to every axios request as a bearer
// header, `login` validates against the BFF before persisting, and a 401 from
// any BFF route clears the token and raises the `vp-admin-unauthorized`
// event that the AuthGate listens for.

import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import axios from "axios";

vi.mock("axios", () => {
  const defaults = { headers: { common: {} as Record<string, string> }, withCredentials: false };
  const handlers: { onRejected?: (e: unknown) => unknown } = {};
  return {
    default: {
      defaults,
      get: vi.fn(),
      post: vi.fn(),
      interceptors: {
        response: {
          use: vi.fn((_ok: unknown, onRejected: (e: unknown) => unknown) => {
            handlers.onRejected = onRejected;
          }),
        },
      },
      __handlers: handlers,
    },
  };
});

type MockedAxios = typeof axios & {
  __handlers: { onRejected?: (e: unknown) => Promise<unknown> };
};
const mocked = axios as unknown as MockedAxios;

async function loadFresh() {
  vi.resetModules();
  return import("../adminAuth");
}

describe("adminAuth", () => {
  beforeEach(() => {
    sessionStorage.clear();
    mocked.defaults.headers.common = {};
    mocked.defaults.withCredentials = false;
    vi.mocked(mocked.get).mockReset();
    vi.mocked(mocked.post).mockReset();
  });
  afterEach(() => {
    sessionStorage.clear();
  });

  it("restores a stored token into the axios default header and sends cookies", async () => {
    sessionStorage.setItem("vp_admin_ui_token", "abc");
    const { installAdminAuth } = await loadFresh();
    installAdminAuth();
    expect(mocked.defaults.headers.common["Authorization"]).toBe("Bearer abc");
    expect(mocked.defaults.withCredentials).toBe(true);
  });

  it("login persists the token only after the BFF accepts it", async () => {
    const { login, getToken } = await loadFresh();
    vi.mocked(mocked.post).mockRejectedValueOnce({ response: { status: 401 } });
    expect(await login("wrong")).toBe(false);
    expect(getToken()).toBe("");
    expect(mocked.defaults.headers.common["Authorization"]).toBeUndefined();

    vi.mocked(mocked.post).mockResolvedValueOnce({ data: { data: { ok: true } } });
    vi.mocked(mocked.get).mockResolvedValueOnce({ data: { data: { auth_required: true, authenticated: true } } });
    expect(await login("  right  ")).toBe(true);
    expect(getToken()).toBe("right");
    expect(mocked.defaults.headers.common["Authorization"]).toBe("Bearer right");
    expect(vi.mocked(mocked.post).mock.calls[1][0]).toMatch(/\/api\/v1\/auth\/login$/);
  });

  it("a 401 from a BFF route clears the token and raises the unauthorized event", async () => {
    sessionStorage.setItem("vp_admin_ui_token", "abc");
    const { installAdminAuth, getToken, UNAUTHORIZED_EVENT } = await loadFresh();
    installAdminAuth();
    const seen = vi.fn();
    window.addEventListener(UNAUTHORIZED_EVENT, seen);

    const err = { response: { status: 401 }, config: { url: "http://localhost:8000/api/v1/sessions" } };
    await expect(mocked.__handlers.onRejected!(err)).rejects.toBe(err);

    expect(seen).toHaveBeenCalledTimes(1);
    expect(getToken()).toBe("");
    expect(mocked.defaults.headers.common["Authorization"]).toBeUndefined();
    window.removeEventListener(UNAUTHORIZED_EVENT, seen);
  });

  it("a 401 from the login endpoint itself, or from another host, does not trip the gate", async () => {
    sessionStorage.setItem("vp_admin_ui_token", "abc");
    const { installAdminAuth, getToken, UNAUTHORIZED_EVENT } = await loadFresh();
    installAdminAuth();
    const seen = vi.fn();
    window.addEventListener(UNAUTHORIZED_EVENT, seen);

    await expect(
      mocked.__handlers.onRejected!({
        response: { status: 401 },
        config: { url: "http://localhost:8000/api/v1/auth/login" },
      }),
    ).rejects.toBeTruthy();
    await expect(
      mocked.__handlers.onRejected!({
        response: { status: 401 },
        config: { url: "http://elsewhere:9999/x" },
      }),
    ).rejects.toBeTruthy();

    expect(seen).not.toHaveBeenCalled();
    expect(getToken()).toBe("abc");
    window.removeEventListener(UNAUTHORIZED_EVENT, seen);
  });

  it("never stores or passes on a worker client token, even if a backend sends one", async () => {
    localStorage.removeItem("vp_client_token");
    const { checkAuth, login } = await loadFresh();
    vi.mocked(mocked.get).mockResolvedValue({
      data: { data: { auth_required: true, authenticated: true, client_token: "worker-secret" } },
    });
    const check = await checkAuth();
    expect(check).toEqual({ auth_required: true, authenticated: true });
    vi.mocked(mocked.post).mockResolvedValueOnce({ data: { data: { ok: true } } });
    expect(await login("abc")).toBe(true);
    expect(localStorage.getItem("vp_client_token")).toBeNull();
    expect(JSON.stringify({ ...localStorage })).not.toContain("worker-secret");
    expect(JSON.stringify({ ...sessionStorage })).not.toContain("worker-secret");
    vi.mocked(mocked.get).mockReset();
  });

  it("removes a worker client token an older build left in localStorage", async () => {
    localStorage.setItem("vp_client_token", "left-over");
    const { installAdminAuth } = await loadFresh();
    installAdminAuth();
    expect(localStorage.getItem("vp_client_token")).toBeNull();
  });

  it("checkAuth reports null when the BFF is unreachable instead of throwing", async () => {
    const { checkAuth } = await loadFresh();
    vi.mocked(mocked.get).mockRejectedValueOnce(new Error("ECONNREFUSED"));
    expect(await checkAuth()).toBeNull();
    vi.mocked(mocked.get).mockResolvedValueOnce({ data: { data: { auth_required: true, authenticated: false } } });
    expect(await checkAuth()).toEqual({ auth_required: true, authenticated: false });
  });
});
