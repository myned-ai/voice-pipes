import { useState } from "react";
import { X, ChevronRight, Mic, ListChecks } from "lucide-react";
import { useTheme } from "../../contexts/ThemeContext";

interface Settings {
  sampleRate: number;
  fixedOutputChunking: boolean;
  fixedChunkFrameMs: number;
  limitScheduledAudio: boolean;
  /** Let the browser apply echo cancellation, noise suppression and
   *  automatic gain to the microphone before the page sees it.
   *
   *  On by default, and usually right for an agent on a laptop. Turn it off
   *  when the audio itself matters -- recording a corpus, or working out why
   *  speech is not being transcribed -- because the suppression is
   *  aggressive enough to gate quiet or distant speech down to silence, which
   *  reaches the recogniser as a pause rather than as words. Takes effect the
   *  next time the microphone starts. */
  browserAudioProcessing: boolean;
  maxScheduledChunks: number;
  /** Playback jitter buffer (ms): hold each utterance's (re)start until
   *  this much audio is queued so realtime-paced providers (ElevenLabs,
   *  slow local TTS) don't underrun audibly on delivery hiccups.
   *  0 = play the first chunk immediately. */
  prebufferMs: number;
  /** Auto-answer `trigger_action` client-tool commands with a Completed
   *  result after a delay, so hands-free runs can test the speak/silent
   *  policy without clicking. Off = step through the card controls
   *  manually. */
  autoCompleteClientTools: boolean;
  autoCompleteClientToolsDelayMs: number;
}

interface Props {
  settings: Settings;
  onChange: (s: Settings) => void;
  onClose: () => void;
  /** Opens the microphone check, which is independent of the capture path
   *  the session uses and so can say whether that path is at fault. */
  onOpenMicCheck: () => void;
  /** Opens the component check, which runs one pipeline stage at a time
   *  against a known input. */
  onOpenComponentCheck: () => void;
  disabled: boolean;
}

export type { Settings };

export default function SettingsPanel({ settings, onChange, onClose, onOpenMicCheck, onOpenComponentCheck, disabled }: Props) {
  const { isDark } = useTheme();
  // Open already when a limit is in force, so an active setting is never hidden.
  const [showAdvanced, setShowAdvanced] = useState(settings.limitScheduledAudio);
  const update = (patch: Partial<Settings>) => onChange({ ...settings, ...patch });

  const selectClass = `rounded-lg px-3 py-1.5 text-sm focus:outline-none focus:border-indigo-500 disabled:opacity-50 ${isDark ? 'bg-slate-900 border border-slate-600 text-slate-200' : 'bg-gray-50 border border-gray-300 text-gray-800'}`;
  const inputClass = `rounded-lg px-3 py-1.5 text-sm focus:outline-none focus:border-indigo-500 disabled:opacity-50 ${isDark ? 'bg-slate-900 border border-slate-600 text-slate-200 placeholder-slate-500' : 'bg-gray-50 border border-gray-300 text-gray-800 placeholder-gray-400'}`;
  const labelClass = `text-xs font-medium uppercase tracking-wide ${isDark ? 'text-slate-400' : 'text-gray-500'}`;
  const checkboxClass = `h-4 w-4 rounded border ${isDark ? 'border-slate-500 bg-slate-900 text-indigo-400' : 'border-gray-300 bg-white text-indigo-600'}`;

  return (
    <div className={`w-72 border-l flex flex-col shrink-0 ${isDark ? 'bg-slate-800 border-slate-700' : 'bg-white border-gray-200'}`}>
      <div className={`flex items-center justify-between px-4 py-3 border-b ${isDark ? 'border-slate-700' : 'border-gray-200'}`}>
        <h3 className={`text-sm font-semibold ${isDark ? 'text-slate-200' : 'text-gray-800'}`}>Session Settings</h3>
        <button onClick={onClose} className={`transition-colors ${isDark ? 'text-slate-400 hover:text-white' : 'text-gray-400 hover:text-gray-900'}`}>
          <X size={16} />
        </button>
      </div>

      <div className="flex flex-col gap-4 p-4 overflow-y-auto">
        {/* Sample Rate */}
        {/* Playback (output) rate. The MIC rate is not set here: the server
            picks it from what the STT needs and the client honours it. */}
        <label className="flex flex-col gap-1.5">
          <span className={labelClass} title="Playback rate. The microphone rate is chosen by the server to match the speech recogniser.">Playback Rate</span>
          <select
            value={settings.sampleRate}
            onChange={(e) => update({ sampleRate: Number(e.target.value) })}
            disabled={disabled}
            className={selectClass}
          >
            <option value={16000}>16,000 Hz</option>
            <option value={24000}>24,000 Hz</option>
            <option value={48000}>48,000 Hz</option>
          </select>
        </label>

        <label className="flex items-center justify-between gap-3">
          <span className={labelClass}>Fixed Output Chunking</span>
          <input
            type="checkbox"
            checked={settings.fixedOutputChunking}
            onChange={(e) => update({ fixedOutputChunking: e.target.checked })}
            disabled={disabled}
            className={checkboxClass}
          />
        </label>

        <label className="flex flex-col gap-1.5">
          <span className={labelClass}>Fixed Chunk Frame (ms)</span>
          <input
            type="number"
            min={10}
            max={250}
            step={5}
            value={settings.fixedChunkFrameMs}
            onChange={(e) => update({ fixedChunkFrameMs: Number(e.target.value) })}
            disabled={disabled || !settings.fixedOutputChunking}
            className={inputClass}
          />
        </label>

        <label className="flex flex-col gap-1.5" title="Jitter buffer: playback of each response starts only once this much audio is queued (or the response is complete), and rebuilds after an underrun. Absorbs delivery hiccups from realtime-paced providers like ElevenLabs at the cost of a slightly later playback start. 0 disables.">
          <span className={labelClass}>Playback Prebuffer (ms)</span>
          <input
            type="number"
            min={0}
            max={1000}
            step={50}
            value={settings.prebufferMs}
            onChange={(e) => update({ prebufferMs: Math.max(0, Number(e.target.value)) })}
            disabled={disabled}
            className={inputClass}
          />
        </label>

        <label
          className="flex items-center justify-between gap-3"
          title="Browser echo cancellation, noise suppression and auto gain. On is right for everyday use; turn it off if speech is not being transcribed, or when recording audio you intend to analyse. Applies next time the mic starts."
        >
          <span className={labelClass}>Browser Mic Processing</span>
          <input
            type="checkbox"
            checked={settings.browserAudioProcessing}
            onChange={(e) => update({ browserAudioProcessing: e.target.checked })}
            disabled={disabled}
            className={checkboxClass}
          />
        </label>

        {/* Client-side tools: auto-complete harness. Client-local
            behavior, so it stays editable while connected. */}
        <label className="flex items-center justify-between gap-3" title="Automatically answer trigger_action client-tool commands with a Completed result after the delay below — for hands-free testing of the speak/silent policy. Leave off to step through the card controls manually.">
          <span className={labelClass}>Auto-Complete Client Tools</span>
          <input
            type="checkbox"
            checked={settings.autoCompleteClientTools}
            onChange={(e) => update({ autoCompleteClientTools: e.target.checked })}
            className={checkboxClass}
          />
        </label>

        <label className="flex flex-col gap-1.5">
          <span className={labelClass}>Auto-Complete Delay (ms)</span>
          <input
            type="number"
            min={0}
            max={30000}
            step={100}
            value={settings.autoCompleteClientToolsDelayMs}
            onChange={(e) =>
              update({ autoCompleteClientToolsDelayMs: Number(e.target.value) })
            }
            disabled={!settings.autoCompleteClientTools}
            className={inputClass}
          />
        </label>

        {/* Playback-scheduler limits work, but they are for chasing a
            scheduling fault, not for everyday use, so they stay out of the
            way until asked for. */}
        <button
          type="button"
          onClick={() => setShowAdvanced((v) => !v)}
          aria-expanded={showAdvanced}
          className={`flex items-center gap-1 self-start text-xs font-medium uppercase tracking-wide ${
            isDark ? "text-slate-400 hover:text-slate-200" : "text-gray-500 hover:text-gray-800"
          }`}
        >
          <ChevronRight size={13} className={`transition-transform ${showAdvanced ? "rotate-90" : ""}`} />
          Advanced
        </button>
        {showAdvanced && (
          <div className="flex flex-col gap-4">
            <label className="flex items-center justify-between gap-3">
              <span className={labelClass}>Limit Scheduled Audio</span>
              <input
                type="checkbox"
                checked={settings.limitScheduledAudio}
                onChange={(e) => update({ limitScheduledAudio: e.target.checked })}
                disabled={disabled}
                className={checkboxClass}
              />
            </label>

            <label className="flex flex-col gap-1.5">
              <span className={labelClass}>Scheduled Chunks Ahead</span>
              <input
                type="number"
                min={1}
                max={12}
                step={1}
                value={settings.maxScheduledChunks}
                onChange={(e) => update({ maxScheduledChunks: Number(e.target.value) })}
                disabled={disabled || !settings.limitScheduledAudio}
                className={inputClass}
              />
            </label>
          </div>
        )}

        {/* The two checks, as things to press rather than links to decode:
            each says what it tests, because the difference between them --
            is the browser hearing anything, or is a stage misbehaving -- is
            the first thing someone with a silent session needs to know.
            Never disabled: a session that is connected and transcribing
            nothing is exactly when they are wanted. */}
        <div className={`flex flex-col gap-2 pt-4 border-t ${isDark ? "border-slate-700" : "border-gray-200"}`}>
          <span className={labelClass}>Troubleshooting</span>
          {[
            {
              icon: <Mic size={16} />,
              title: "Mic check",
              text: "Is the browser getting sound? Shows the live level and what the browser did to it. No session needed.",
              onClick: onOpenMicCheck,
            },
            {
              icon: <ListChecks size={16} />,
              title: "Component check",
              text: "Runs VAD, STT, LLM and TTS one at a time on a known input, to find the stage at fault.",
              onClick: onOpenComponentCheck,
            },
          ].map((item) => (
            <button
              key={item.title}
              type="button"
              onClick={item.onClick}
              className={`flex items-start gap-3 rounded-lg border px-3 py-2.5 text-left transition-colors ${
                isDark
                  ? "border-slate-600 bg-slate-900/40 hover:border-indigo-500 hover:bg-slate-900"
                  : "border-gray-300 bg-gray-50 hover:border-indigo-400 hover:bg-white"
              }`}
            >
              <span className={`mt-0.5 shrink-0 ${isDark ? "text-indigo-300" : "text-indigo-600"}`}>{item.icon}</span>
              <span className="flex flex-col gap-0.5 min-w-0">
                <span className={`text-sm font-medium ${isDark ? "text-slate-100" : "text-gray-900"}`}>{item.title}</span>
                <span className={`text-xs leading-snug ${isDark ? "text-slate-400" : "text-gray-500"}`}>{item.text}</span>
              </span>
              <ChevronRight size={14} className={`ml-auto mt-1 shrink-0 ${isDark ? "text-slate-500" : "text-gray-400"}`} />
            </button>
          ))}
        </div>
      </div>
    </div>
  );
}
