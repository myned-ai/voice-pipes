/**
 * Single source of truth for the API base URL.
 *
 * Set VITE_API_URL in a .env file (or .env.local) to point at a different
 * environment — e.g. VITE_API_URL=https://staging.voice-pipes.example.com
 * Falls back to the local dev server when the variable is absent.
 */
export const API_BASE = import.meta.env.VITE_API_URL ?? 'http://localhost:8000';

/**
 * WebSocket URL used by the playground to connect to a worker.
 *
 * Set VITE_WS_URL to override.  When unset, defaults to the local dev worker
 * dispatcher (`ws://localhost:8800/ws`).  Production deployments MUST set
 * VITE_WS_URL to the wss:// endpoint exposed by the worker fleet.
 */
export const WS_URL = import.meta.env.VITE_WS_URL ?? 'ws://localhost:8800/ws';
