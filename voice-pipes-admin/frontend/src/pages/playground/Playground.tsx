import { useState, useCallback, useEffect, useRef } from "react";
import axios from "axios";
import { type PlaygroundSession, type PlaybackOptions, type TurnMetricsEntry } from "./usePlaygroundSession";
import { DEFAULT_PREBUFFER_MS } from "./session/types";
import { DEFAULT_MIC_SAMPLE_RATE } from "../../lib/sdk/VoicePipesSessionCore";
import type { Settings } from "./SettingsPanel";
import ConnectionBar from "./ConnectionBar";
import SettingsPanel from "./SettingsPanel";
import MicCheckDialog from "./micCheck/MicCheckDialog";
import ComponentCheckDialog from "./componentCheck/ComponentCheckDialog";
import ChatTranscript from "./ChatTranscript";
import AudioVisualiser from "./AudioVisualiser";
import InputBar from "./InputBar";
import EventLog from "./EventLog";
import StateTimeline from "./StateTimeline";
import ExtensionStage from "./ExtensionStage";
import {
  EXTENSION_READY_TIMEOUT_MS,
  loadPlaygroundExtensionsOnce,
  pendingExtensions,
  waitForExtensionsReady,
} from "./extensionRegistry";
import { useTheme } from "../../contexts/ThemeContext";
// TEMP: Hide metrics bar and legend UI
const HIDE_METRICS_BAR = true;
import { API_BASE, WS_URL } from "../../config/api";
import type { SessionMode } from "../../lib/transport/types";
import { ChevronDown, ChevronUp, HelpCircle, X } from "lucide-react";
import { MetricsGuide } from "../../components/MetricsGuide";
import { chainOverheadMs, emitRtf, firstTextMs, pacerWaitMs, llmGenerationMs, providerEndpointingMs, sttLatencyMs, ttfbAnchorSource, ttfbMs, ttftMs, ttsTtfbMs } from "../../lib/metrics/lookup";
import { describeAnchorSource, describeClientE2eSource } from "../../lib/metrics/anchorSource";
import { TurnComponentMetricsTable } from "../../components/metrics/TurnComponentMetricsTable";
import { SessionDescriptorsContext } from "./SessionDescriptorsContext";
import { requestDispatch } from "./dispatch";
import { parseClientTools, type ClientToolInfo } from "./clientTools";
import {
  effectiveSource, pipelineUnavailableReason, readPreferredSource, storePreferredSource,
  type ConnectSource, type ConnectedTarget, type PipelineListStatus,
} from "./connectTarget";

const DEFAULT_URL = WS_URL;
// One empty list, so a session without client tools passes the same array
// every render.
const NO_CLIENT_TOOLS: ClientToolInfo[] = [];

/** Shape of one `GET /api/v1/pipelines` group as far as the Playground reads it. */
interface PipelineGroup {
  pipeline_name?: string | null;
  pipeline_config_hash: string;
  workers?: unknown[];
}

/** Compact metrics bar for the latest turn + RTT. */
function MetricsBar({ latest, rttMs, showLegend, onToggleLegend, showDetails, onToggleDetails, hasSnapshot }: { latest?: TurnMetricsEntry; rttMs: number | null; showLegend: boolean; onToggleLegend: () => void; showDetails: boolean; onToggleDetails: () => void; hasSnapshot: boolean }) {
  const { isDark } = useTheme();
  if (!latest && rttMs == null) return null;

  const PILL_META: Record<string, { title: string; color?: string }> = {
    STT:   { title: "Speech-to-Text latency: time from flushing your audio to receiving the final transcript from the STT provider." },
    TTFT:  { title: "Time To First Token: time from utterance commit (STT done) to the first text token produced by the LLM.", color: "text-amber-400" },
    LLM:   { title: "LLM generation duration: time from the first LLM token to the last (streaming generation span)." },
    TTS:   { title: "TTS time-to-first-byte: time from the first LLM text fragment arriving at TTS to the first audio sample produced." },
    TTFB:  { title: "Time To First Byte (server): time from the end-of-speech anchor to the first audio byte delivered to your browser (never the first transcript). This is the core 'response latency' the server measures.", color: "text-emerald-400" },
    // The provider's endpointing lag is
    // user-felt wait that used to be invisible (null) for every S2S turn.
    Endpointing: { title: "Provider endpointing (S2S): how long the speech-to-speech provider's VAD / transcription took to notice you stopped talking — its end-of-speech anchor minus the worker's own VAD stamp. Budget 500 ms; the user waits through this before TTFB even starts.", color: "text-rose-400" },
    Total: { title: "Wall-clock time from utterance commit to the last audio byte for this turn. Includes full LLM generation + TTS synthesis." },
    E2E:   { title: "Client end-to-end: time from your browser's end-of-speech anchor to when the first audio arrived. Includes network round-trip. Closest to what you perceive as 'delay'.", color: "text-sky-400" },
    TTFA:     { title: "Client time-to-first-audio: end-of-speech (client VAD) → first sample actually audible. E2E + Buffer. This is the delay you hear.", color: "text-sky-400" },
    Buffer:   { title: "Jitter buffer + scheduling: first audio byte received → first sample audible. ≈ the prebuffer setting when delivery is steady; larger when the prebuffer timed out on bursty delivery." },
    Delivery: { title: "Server → ear: the pacer emitted the first chunk → first sample audible, corrected with the Ping/Pong clock offset. Network one-way + Buffer. Needs one Pong; hidden until then." },
    RTT:   { title: "Network round-trip time measured via WebSocket Ping/Pong. Subtract from E2E to estimate pure server processing time." },
    // Throughput, not latency: a stream
    // that starts on time and then trickles at 0.6× is invisible to every
    // pill above.
    "Emit RTF":    { title: "Output realtime factor (server, streaming phase): wall time first → last audio chunk emitted ÷ audio duration. > 1.0 = the server emits audio slower than it plays — the client starves once the prebuffer drains. Target ≤ 0.9.", color: "text-violet-400" },
    Chain:         { title: "Post-processor chain overhead: wall time (ms) the inline audio post-processors added between receiving audio from the provider and forwarding it. 0 when no processor ran. Budget ≈ 10% of the audio duration.", color: "text-violet-400" },
    Pacer:         { title: "Realtime pacer wait: the rest of the time between receiving audio and forwarding it, mostly the pacer holding a faster-than-realtime provider to playback speed. Large with Emit RTF near 1.0 is by design.", color: "text-violet-400" },
    "Arrival RTF": { title: "Client arrival realtime factor: wall time first → last audio frame received ÷ audio duration received. > 1.0 with underruns = starvation (server/network too slow); ≤ 1.0 with underruns = jitter. Known once the turn ends.", color: "text-sky-400" },
  };

  const pill = (label: string, value: number | undefined, unit = "ms", colorOverride?: string, titleOverride?: string) => {
    if (value == null) return null;
    const meta = PILL_META[label];
    return (
      <span
        key={label}
        title={titleOverride ?? meta?.title}
        className={`inline-flex items-center gap-1 rounded px-2 py-0.5 text-xs font-mono cursor-help ${isDark ? 'bg-slate-700 text-slate-200' : 'bg-gray-200 text-gray-800'}`}
      >
        <span className={colorOverride ?? meta?.color ?? (isDark ? 'text-slate-400' : 'text-gray-500')}>{label}</span>
        {value}{unit}
      </span>
    );
  };

  /** Realtime-factor pill: red label when the value is over 1.0 (slower than realtime). */
  const rtfPill = (label: string, value: number | undefined) =>
    pill(label, value, "×", value != null && value > 1.0 ? "text-rose-400" : undefined);

  // Anchor provenance: the TTFB tooltip
  // names the server's anchor, the E2E tooltip the client's.
  const anchor = ttfbAnchorSource(latest?.snapshot);
  const anchorInfo = anchor ? describeAnchorSource(anchor) : null;
  const firstText = firstTextMs(latest?.snapshot);
  const ttfbTitle = anchorInfo
    ? `server: ${anchorInfo.label} → first audio (${anchorInfo.detail})${firstText != null ? ` · first transcript at ${firstText}ms` : ""}. ${PILL_META.TTFB.title}`
    : undefined;
  const e2eTitle = latest?.clientE2eMs != null
    ? `client: ${describeClientE2eSource(latest.clientE2eSource)} → first audio received. ${PILL_META.E2E.title}`
    : undefined;
  const endpointing = providerEndpointingMs(latest?.snapshot);

  return (
    <div className={`border-t ${isDark ? 'bg-slate-800 border-slate-700' : 'bg-gray-100 border-gray-200'}`}>
      <div className="flex flex-wrap items-center justify-end gap-2 px-4 py-1.5 text-xs">
        {latest && (
          <>
            {pill("STT", sttLatencyMs(latest.snapshot))}
            {pill("TTFT", ttftMs(latest.snapshot))}
            {pill("LLM", llmGenerationMs(latest.snapshot))}
            {pill("TTS", ttsTtfbMs(latest.snapshot))}
            {pill("Endpointing", endpointing, "ms", endpointing != null && endpointing > 500 ? "text-rose-400" : (isDark ? "text-slate-400" : "text-gray-500"))}
            {pill("TTFB", ttfbMs(latest.snapshot), "ms", anchorInfo?.tone === "none" ? "text-rose-400" : anchorInfo?.tone === "proxy" ? "text-amber-400" : undefined, ttfbTitle)}
            {pill("Total", latest.snapshot.total_ms)}
            {pill("E2E", latest.clientE2eMs, "ms", latest.clientE2eSource === "listening" ? "text-amber-400" : undefined, e2eTitle)}
            {pill("TTFA", latest.clientTtfaMs)}
            {pill("Buffer", latest.clientReceiveToPlayMs)}
            {pill("Delivery", latest.clientPlaybackLatencyMs)}
            {rtfPill("Emit RTF", emitRtf(latest.snapshot))}
            {pill("Chain", chainOverheadMs(latest.snapshot))}
            {pill("Pacer", pacerWaitMs(latest.snapshot))}
            {rtfPill("Arrival RTF", latest.clientArrivalRtf)}
            {latest.snapshot.interrupted && (
              <span className="rounded bg-amber-800/60 px-2 py-0.5 text-amber-300 text-xs">interrupted</span>
            )}
          </>
        )}
        {rttMs != null && pill("RTT", rttMs)}
        <button
          onClick={onToggleLegend}
          title="Explain metrics"
          className={`ml-1 rounded p-0.5 transition-colors ${showLegend ? (isDark ? 'text-sky-400' : 'text-sky-600') : (isDark ? 'text-slate-500 hover:text-slate-300' : 'text-gray-400 hover:text-gray-600')}`}
        >
          <HelpCircle size={14} />
        </button>
        {hasSnapshot && (
          <button
            onClick={onToggleDetails}
            title={showDetails ? "Hide per-component breakdown" : "Show per-component breakdown"}
            className={`rounded p-0.5 transition-colors ${showDetails ? (isDark ? 'text-sky-400' : 'text-sky-600') : (isDark ? 'text-slate-500 hover:text-slate-300' : 'text-gray-400 hover:text-gray-600')}`}
          >
            {showDetails ? <ChevronUp size={14} /> : <ChevronDown size={14} />}
          </button>
        )}
      </div>
    </div>
  );
}


function MetricsLegend({ onClose }: { onClose: () => void }) {
  const { isDark } = useTheme();
  return (
    <div className={`border-t px-4 py-3 ${isDark ? 'bg-slate-900/70 border-slate-700' : 'bg-gray-50 border-gray-200'}`}>
      <div className="flex items-center justify-between mb-2">
        <span className={`text-[10px] font-bold uppercase tracking-widest ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>
          Metrics Guide
        </span>
        <button onClick={onClose} className={`rounded p-0.5 ${isDark ? 'text-slate-500 hover:text-slate-300' : 'text-gray-400 hover:text-gray-600'}`}>
          <X size={13} />
        </button>
      </div>

      <MetricsGuide />
    </div>
  );
}

export default function Playground({ session }: { session: PlaygroundSession }) {
  const { isDark } = useTheme();
  const [url, setUrl] = useState(DEFAULT_URL);
  const [showSettings, setShowSettings] = useState(false);
  const [showMicCheck, setShowMicCheck] = useState(false);
  const [showComponentCheck, setShowComponentCheck] = useState(false);
  // Which worker the last dispatch actually chose. The component check probes
  // this one in preference to dispatching again: dispatch picks the
  // least-loaded worker, and a session in trouble makes its own worker the
  // least likely to be picked -- so re-dispatching would probe a different,
  // probably healthy, worker than the one misbehaving.
  const [dispatchedWorkerId, setDispatchedWorkerId] = useState<string | null>(null);
  const [showEvents, setShowEvents] = useState(false);
  const [showTimeline, setShowTimeline] = useState(false);
  // Playground Extensions — stage visibility persists; the toggle only renders
  // once discovery reports at least one configured (loaded OR failed) entry.
  const [showStage, setShowStage] = useState(() => localStorage.getItem("vp.playground.showStage") !== "0");
  const [extensionCount, setExtensionCount] = useState<number | null>(null);
  useEffect(() => {
    let alive = true;
    loadPlaygroundExtensionsOnce().then((r) => {
      if (alive) setExtensionCount(r.loaded.length + r.failed.length);
    });
    return () => { alive = false; };
  }, []);
  useEffect(() => {
    localStorage.setItem("vp.playground.showStage", showStage ? "1" : "0");
  }, [showStage]);
  const [showMetricsLegend, setShowMetricsLegend] = useState(false);
  const [showComponentDetails, setShowComponentDetails] = useState(false);
  const [settings, setSettings] = useState<Settings>({
    sampleRate: 48000,
    fixedOutputChunking: true,
    fixedChunkFrameMs: 50,
    limitScheduledAudio: false,
    browserAudioProcessing: true,
    maxScheduledChunks: 2,
    prebufferMs: DEFAULT_PREBUFFER_MS,
    autoCompleteClientTools: false,
    autoCompleteClientToolsDelayMs: 1000,
  });

  // Flush any locally buffered audio when navigating away so it doesn't
  // keep playing in the background. The WebSocket session stays alive.
  const { flushLocalAudio } = session;
  useEffect(() => {
    return () => { flushLocalAudio(); };
  }, [flushLocalAudio]);

  const [dispatching, setDispatching] = useState(false);
  const [dispatchError, setDispatchError] = useState<string | null>(null);
  const [pipelines, setPipelines] = useState<{ name: string; hash: string; workerCount: number }[]>([]);
  const [selectedPipeline, setSelectedPipeline] = useState("");
  const [pipelineStatus, setPipelineStatus] = useState<PipelineListStatus>("loading");
  const [preferredSource, setPreferredSource] = useState<ConnectSource>(readPreferredSource);
  const [target, setTarget] = useState<ConnectedTarget | null>(null);
  const [sessionMode, setSessionMode] = useState<SessionMode>("voice");

  // Fetch available pipelines
  useEffect(() => {
    axios.get(`${API_BASE}/api/v1/pipelines`)
      .then(res => {
        const groups: unknown = res.data?.data?.groups;
        if (Array.isArray(groups)) {
          setPipelines(
            (groups as PipelineGroup[]).flatMap((g) =>
              g.pipeline_name
                ? [{ name: g.pipeline_name, hash: g.pipeline_config_hash, workerCount: g.workers?.length ?? 0 }]
                : [],
            ),
          );
        }
        setPipelineStatus("ok");
      })
      .catch(() => setPipelineStatus("unreachable"));
  }, []);

  const pipelineUnavailable = pipelineUnavailableReason(pipelineStatus, pipelines.length);
  const source = effectiveSource(preferredSource, pipelineStatus, pipelines.length);
  const handleSourceChange = useCallback((next: ConnectSource) => {
    setPreferredSource(next);
    storePreferredSource(next);
  }, []);

  // Extension readiness gate: hold Start until every MOUNTED
  // extension that implements `ready()` has resolved (bounded), so an agent
  // that greets on connect is not speaking to a loading spinner. Nothing
  // mounted / nothing pending → returns immediately.
  const [waitingFor, setWaitingFor] = useState<string[] | null>(null);
  const gateOnExtensions = useCallback(async () => {
    const pending = pendingExtensions();
    if (pending.length === 0) return;
    setWaitingFor(pending.map((p) => p.name));
    try {
      const r = await waitForExtensionsReady();
      if (r.pending.length > 0) {
        console.warn(`[playground] extensions not ready after ${EXTENSION_READY_TIMEOUT_MS}ms, connecting anyway:`, r.pending);
      }
    } finally {
      setWaitingFor(null);
    }
  }, []);

  // One way in. The source decides only where the session goes; everything
  // the session is configured with is the same either way, so it is built once.
  const startSession = useCallback(async () => {
    setDispatchError(null);
    const playbackOptions: PlaybackOptions = {
      limitScheduledAudio: settings.limitScheduledAudio,
      maxScheduledChunks: settings.maxScheduledChunks,
      prebufferMs: settings.prebufferMs,
    };
    const options: Record<string, unknown> = {
      output_chunking_enabled: settings.fixedOutputChunking,
      output_chunk_frame_ms: settings.fixedChunkFrameMs,
    };

    let next: ConnectedTarget;
    if (source === "pipeline") {
      setDispatching(true);
      try {
        const found = await requestDispatch(API_BASE, selectedPipeline);
        next = {
          source: "pipeline",
          pipelineName: found.pipelineName ?? (selectedPipeline || null),
          workerId: found.workerId,
          url: found.wsUrl,
        };
      } catch (err) {
        const msg = axios.isAxiosError(err)
          ? err.response?.data?.detail ?? err.message
          : err instanceof Error ? err.message : "no worker found";
        setDispatchError(`Could not find a worker: ${msg}`);
        return;
      } finally {
        setDispatching(false);
      }
    } else {
      next = { source: "url", pipelineName: null, workerId: null, url };
    }
    // A direct connection names no worker, and one left over from an earlier
    // dispatch would send the component check to the wrong machine.
    setDispatchedWorkerId(next.workerId);
    setTarget(next);

    // After the dispatch HTTP call so the two waits overlap.
    await gateOnExtensions();
    await session.connect(next.url, "websocket", {
      // `sampleRate` is the PLAYBACK rate now. The mic rate is proposed at the
      // browser's native 48 kHz and the server lowers it to whatever the STT
      // needs — see `accepts_negotiated_input_rate`.
      sample_rate: DEFAULT_MIC_SAMPLE_RATE,
      output_sample_rate: settings.sampleRate,
      // Capture and playback are mono; language, voice and provider tuning
      // are the worker's configuration, not the Playground's.
      channels: 1,
      options,
      mode: sessionMode,
    }, playbackOptions);
  }, [source, url, settings, session, selectedPipeline, sessionMode, gateOnExtensions]);

  const connected = session.sessionState === "active";

  // Client tools of the worker the session landed on, read once per session
  // from the config it registered with the orchestrator (admin API; the voice
  // protocol is untouched). A direct-URL session names no worker, so shows
  // none.
  // Kept with the worker they were read for, so a session on another worker
  // (or none) shows no stale tools without an effect clearing them.
  const [toolsFor, setToolsFor] = useState<{ workerId: string; tools: ClientToolInfo[] } | null>(null);
  const connectedWorkerId = connected ? target?.workerId ?? null : null;
  const clientTools =
    connectedWorkerId && toolsFor?.workerId === connectedWorkerId ? toolsFor.tools : NO_CLIENT_TOOLS;
  useEffect(() => {
    if (!connectedWorkerId) return;
    let alive = true;
    axios.get(`${API_BASE}/api/v1/workers/${encodeURIComponent(connectedWorkerId)}`)
      .then((res) => {
        if (alive) {
          setToolsFor({
            workerId: connectedWorkerId,
            tools: parseClientTools(res.data?.data?.pipeline_config),
          });
        }
      })
      .catch(() => { /* no chip is the right fallback */ });
    return () => { alive = false; };
  }, [connectedWorkerId]);

  // ── Client-side tools: auto-complete harness ──
  // When enabled, every new `trigger_action` command is answered with a
  // Completed result after the configured delay — hands-free testing of
  // the speak/silent policy. Manual mode leaves the card controls in
  // charge (the card still works alongside auto mode; the server treats
  // duplicate results as a no-op).
  const autoCompletedCallsRef = useRef(new Set<string>());
  const { transcript, sendClientToolResult } = session;
  useEffect(() => {
    if (!settings.autoCompleteClientTools || !connected) return;
    for (const entry of transcript) {
      if (entry.type !== "message" || entry.pipelineMessage?.kind !== "trigger_action") continue;
      const payload = entry.pipelineMessage.payload as { call_id?: string; name?: string } | null;
      const callId = payload?.call_id;
      if (!callId || autoCompletedCallsRef.current.has(callId)) continue;
      autoCompletedCallsRef.current.add(callId);
      window.setTimeout(() => {
        sendClientToolResult(callId, "completed", {
          status: "ok",
          auto_completed: true,
          name: payload?.name,
        });
      }, settings.autoCompleteClientToolsDelayMs);
    }
  }, [transcript, connected, settings.autoCompleteClientTools, settings.autoCompleteClientToolsDelayMs, sendClientToolResult]);

  return (
    <div className="flex flex-col h-full w-full">
      {/* Top bar */}
      <ConnectionBar
        sessionState={session.sessionState}
        pipelineState={session.pipelineState}
        errorMessage={session.errorMessage || dispatchError}
        url={url}
        onUrlChange={setUrl}
        source={source}
        onSourceChange={handleSourceChange}
        pipelineUnavailable={pipelineUnavailable}
        onStart={startSession}
        onDisconnect={session.disconnect}
        findingWorker={dispatching}
        target={target}
        waitingFor={waitingFor}
        pipelines={pipelines}
        selectedPipeline={selectedPipeline}
        onPipelineChange={setSelectedPipeline}
        onToggleSettings={() => setShowSettings((s) => !s)}
        showSettings={showSettings}
        onToggleEvents={() => setShowEvents((s) => !s)}
        showEvents={showEvents}
        onToggleTimeline={() => setShowTimeline((s) => !s)}
        showTimeline={showTimeline}
        hasExtensions={(extensionCount ?? 0) > 0}
        showStage={showStage}
        onToggleStage={() => setShowStage((s) => !s)}
        qualityTier={session.qualityTier}
        clientTools={clientTools}
        sessionMode={sessionMode}
        onSessionModeChange={setSessionMode}
        onClearConversation={session.clearConversation}
        hasConversation={session.transcript.length > 0 || session.events.length > 0}
      />

      {/* Main content area */}
      <div className="flex flex-1 overflow-hidden">
        {/* Settings panel (collapsible left) */}
        {showSettings && (
          <SettingsPanel
            settings={settings}
            onChange={setSettings}
            onClose={() => setShowSettings(false)}
            onOpenMicCheck={() => setShowMicCheck(true)}
            onOpenComponentCheck={() => setShowComponentCheck(true)}
            disabled={connected}
          />
        )}

        {/* Center: conversation + audio viz + input */}
        <div className="flex-1 flex flex-col min-w-0 overflow-hidden">
          {/* Extension stage (external renderers: avatars, meters, …) */}
          {showStage && (extensionCount ?? 0) > 0 && (
            <ExtensionStage session={session.extensionSession} onClose={() => setShowStage(false)} />
          )}

          {/* Chat transcript */}
          <ChatTranscript
            transcript={session.transcript}
            pipelineState={session.pipelineState}
            connected={connected}
            highlightInfo={session.highlightInfo}
            speakingEntryIndex={session.speakingEntryIndex}
            wsUrl={url}
            onClientToolResult={session.sendClientToolResult}
          />

          {/* Audio visualisers */}
          {connected && (
            <div className={`flex gap-4 px-4 py-2 ${isDark ? 'bg-slate-800' : 'bg-gray-100'}`}>
              <div className="flex-1">
                <AudioVisualiser
                  level={session.micLevel}
                  label="Microphone"
                  colorVariant={session.micActive ? "red" : "emerald"}
                />
              </div>
              <div className="flex-1">
                <AudioVisualiser
                  level={session.playbackLevel}
                  label="Agent"
                  colorVariant="indigo"
                />
              </div>
            </div>
          )}

          {/* Agent response metrics (temporarily hidden) */}
          {connected && !HIDE_METRICS_BAR && (
            <>
              <MetricsBar
                latest={session.turnMetrics[session.turnMetrics.length - 1]}
                rttMs={session.rttMs}
                showLegend={showMetricsLegend}
                onToggleLegend={() => setShowMetricsLegend((s) => !s)}
                showDetails={showComponentDetails}
                onToggleDetails={() => setShowComponentDetails((s) => !s)}
                hasSnapshot={!!session.turnMetrics[session.turnMetrics.length - 1]}
              />
              {showMetricsLegend && (
                <MetricsLegend onClose={() => setShowMetricsLegend(false)} />
              )}
              {showComponentDetails && session.turnMetrics[session.turnMetrics.length - 1] && (
                <SessionDescriptorsContext.Provider value={session.metricDescriptors}>
                  <div className={`border-t px-4 py-3 ${isDark ? 'bg-slate-900/70 border-slate-700' : 'bg-gray-50 border-gray-200'}`}>
                    <TurnComponentMetricsTable
                      snapshot={session.turnMetrics[session.turnMetrics.length - 1]!.snapshot}
                      descriptors={session.metricDescriptors}
                    />
                  </div>
                </SessionDescriptorsContext.Provider>
              )}
            </>
          )}

          {/* Input bar */}
          <InputBar
            connected={connected}
            micActive={session.micActive}
            agentSpeaking={session.pipelineState === "speaking" || session.pipelineState === "processing"}
            onStartMic={() => session.startMic(undefined, {
              echoCancellation: settings.browserAudioProcessing,
              noiseSuppression: settings.browserAudioProcessing,
              autoGainControl: settings.browserAudioProcessing,
            })}
            onStopMic={session.stopMic}
            onSendText={session.sendText}
            onCancelAgent={session.cancelAgent}
            onDisconnect={session.disconnect}
          />
        </div>

        {/* Event log (collapsible right) */}
        {showEvents && (
          <EventLog
            events={session.events}
            onClose={() => setShowEvents(false)}
          />
        )}

        {/* State timeline (collapsible right, T6) */}
        {showTimeline && (
          <StateTimeline
            timeline={session.stateTimeline}
            onClose={() => setShowTimeline(false)}
          />
        )}
      </div>

      {/* Microphone check — deliberately shares no code with the capture path
          it is used to judge. */}
      {showMicCheck && (
        <MicCheckDialog
          onClose={() => setShowMicCheck(false)}
          sessionHoldsMic={session.micActive}
        />
      )}

      {/* Component check — asks what each stage does with a known input, where
          the mic check asks whether the device delivers usable audio. */}
      {showComponentCheck && (
        <ComponentCheckDialog
          apiBase={API_BASE}
          selectedPipeline={selectedPipeline}
          preferredWorkerId={dispatchedWorkerId}
          browserAudioProcessing={settings.browserAudioProcessing}
          onClose={() => setShowComponentCheck(false)}
        />
      )}
    </div>
  );
}
