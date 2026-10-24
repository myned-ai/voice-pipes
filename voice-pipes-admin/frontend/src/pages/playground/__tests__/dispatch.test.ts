// The Playground's Dispatch contract: the selected pipeline reaches the
// orchestrator as `pipeline_name`, and the session connects to the BFF's
// Playground proxy for the worker that comes back. Regression for the
// two-worker stack where every dispatch silently opened the default worker,
// and for the browser connecting to the worker itself (which needs the
// worker credential the browser must not hold).

import { describe, it, expect, vi, beforeEach } from "vitest";
import axios from "axios";
import { bffWebSocketUrl, requestDispatch } from "../dispatch";

vi.mock("axios");
const mockedGet = vi.mocked(axios.get);

describe("requestDispatch", () => {
  beforeEach(() => {
    mockedGet.mockReset();
  });

  it("sends the selected pipeline as pipeline_name and connects through the BFF proxy for that worker", async () => {
    mockedGet.mockResolvedValueOnce({
      data: { data: { worker_id: "tesla_hr", ws_path: "/api/v1/playground/ws?worker_id=tesla_hr", pipeline_name: "tesla_hr" } },
    });
    const target = await requestDispatch("http://bff", "tesla_hr");
    expect(mockedGet).toHaveBeenCalledWith("http://bff/api/v1/dispatch", { params: { pipeline_name: "tesla_hr" } });
    expect(target).toEqual({
      wsUrl: "ws://bff/api/v1/playground/ws?worker_id=tesla_hr",
      workerId: "tesla_hr",
      pipelineName: "tesla_hr",
    });
  });

  it("omits pipeline_name for 'Any pipeline' so the orchestrator applies its policy", async () => {
    mockedGet.mockResolvedValueOnce({
      data: { data: { worker_id: "nikola_tesla", ws_path: "/api/v1/playground/ws?worker_id=nikola_tesla", pipeline_name: "nikola_tesla" } },
    });
    const target = await requestDispatch("http://bff", "");
    expect(mockedGet).toHaveBeenCalledWith("http://bff/api/v1/dispatch", { params: {} });
    expect(target.wsUrl).toBe("ws://bff/api/v1/playground/ws?worker_id=nikola_tesla");
  });

  it("never connects to the worker's own address, even if a backend still sends one", async () => {
    mockedGet.mockResolvedValueOnce({
      data: { data: { worker_id: "w1", ws_url: "ws://127.0.0.1:8800/ws" } },
    });
    await expect(requestDispatch("http://bff", "")).rejects.toThrow(/no worker/);
  });

  it("refuses a dispatch without a worker instead of falling back to the default", async () => {
    mockedGet.mockResolvedValueOnce({ data: { data: null } });
    await expect(requestDispatch("http://bff", "tesla_hr")).rejects.toThrow(/no worker/);
  });
});

describe("bffWebSocketUrl", () => {
  it("maps the BFF's scheme to the WebSocket one and keeps a path prefix", () => {
    expect(bffWebSocketUrl("https://console.example/admin", "/api/v1/playground/ws?worker_id=a%3Fb"))
      .toBe("wss://console.example/admin/api/v1/playground/ws?worker_id=a%3Fb");
    expect(bffWebSocketUrl("http://localhost:8000/", "/api/v1/playground/ws?worker_id=w"))
      .toBe("ws://localhost:8000/api/v1/playground/ws?worker_id=w");
  });

  it("puts no credential in the URL", () => {
    expect(bffWebSocketUrl("http://bff", "/api/v1/playground/ws?worker_id=w")).not.toMatch(/token/);
  });
});
