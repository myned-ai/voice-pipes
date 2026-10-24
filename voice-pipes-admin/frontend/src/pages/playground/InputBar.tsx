import { useState } from "react";
import { Mic, MicOff, Send, Square, PowerOff } from "lucide-react";
import { useTheme } from "../../contexts/ThemeContext";

interface Props {
  connected: boolean;
  micActive: boolean;
  /** True when the agent is currently producing speech or has buffered audio
   *  still scheduled — controls whether the Stop button is visible/enabled. */
  agentSpeaking: boolean;
  onStartMic: () => void;
  onStopMic: () => void;
  onSendText: (text: string) => void;
  onCancelAgent: () => void;
  onDisconnect: () => void;
}

export default function InputBar({
  connected,
  micActive,
  agentSpeaking,
  onStartMic,
  onStopMic,
  onSendText,
  onCancelAgent,
  onDisconnect,
}: Props) {
  const { isDark } = useTheme();
  const [text, setText] = useState("");

  const handleSend = () => {
    const trimmed = text.trim();
    if (!trimmed) return;
    onSendText(trimmed);
    setText("");
  };

  return (
    <div className={`flex flex-col border-t shrink-0 ${isDark ? 'bg-slate-800 border-slate-700' : 'bg-white border-gray-200'}`}>
    <div className="flex items-center gap-2 px-4 py-3">
      {/* Mic toggle */}
      <button
        onClick={micActive ? onStopMic : onStartMic}
        disabled={!connected}
        className={`p-2.5 rounded-xl transition-colors ${
          micActive
            ? "bg-red-600 text-white hover:bg-red-700 animate-pulse"
            : isDark ? "bg-slate-700 text-slate-300 hover:bg-slate-600 disabled:opacity-40" : "bg-gray-200 text-gray-600 hover:bg-gray-300 disabled:opacity-40"
        }`}
        title={micActive ? "Stop microphone" : "Start microphone"}
      >
        {micActive ? <MicOff size={18} /> : <Mic size={18} />}
      </button>

      {/* Text input */}
      <input
        type="text"
        value={text}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && text.trim()) handleSend();
        }}
        disabled={!connected}
        placeholder={micActive ? "Mic active — or type here..." : "Type a message..."}
        className={`flex-1 rounded-xl px-4 py-2 text-sm focus:outline-none focus:border-indigo-500 disabled:opacity-40 min-w-0 ${isDark ? 'bg-slate-900 border border-slate-600 text-slate-200 placeholder-slate-500' : 'bg-gray-50 border border-gray-300 text-gray-800 placeholder-gray-400'} ${
          micActive ? "opacity-70" : ""
        }`}
      />

      {/* Send button */}
      <button
        onClick={handleSend}
        disabled={!connected || !text.trim()}
        className="p-2.5 rounded-xl bg-indigo-600 text-white hover:bg-indigo-700 transition-colors disabled:opacity-40"
        title="Send text message"
      >
        <Send size={18} />
      </button>

      {/* Stop agent (Path C: explicit cancel) */}
      {connected && agentSpeaking && (
        <button
          onClick={onCancelAgent}
          className={`p-2.5 rounded-xl transition-colors ${isDark ? 'bg-orange-900/40 text-orange-300 hover:bg-orange-900/60 border border-orange-700/50' : 'bg-orange-100 text-orange-700 hover:bg-orange-200 border border-orange-300'}`}
          title="Stop agent (cancel current response)"
        >
          <Square size={18} />
        </button>
      )}

      {/* Disconnect */}
      {connected && (
        <button
          onClick={onDisconnect}
          className={`p-2.5 rounded-xl transition-colors ${isDark ? 'bg-slate-700 text-red-400 hover:bg-slate-600' : 'bg-gray-200 text-red-500 hover:bg-gray-300'}`}
          title="Disconnect"
        >
          <PowerOff size={18} />
        </button>
      )}
    </div>
    </div>
  );
}
