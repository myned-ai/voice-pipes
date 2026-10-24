# VoicePipes React Client SDK - LLM Integration Guide

**Status:** active
**Last verified:** 2026-09-06 (moved into the public tree; content unchanged)
**Owner:** Docs

> [!NOTE]
> This document provides mechanical instructions designed for LLMs (and humans) to easily extract and integrate the VoicePipes client into a custom React application.

The VoicePipes React SDK provides a robust, headless client for connecting to VoicePipes backend servers, complete with streaming STT/TTS support, karaoke-style progressive reveal, microphone capture, and automatic interruption/barge-in handling.

## 1. Extraction: Required Files
To use the client in another repository, copy the following core directories into your target React codebase (e.g., into `src/lib/voice-pipes/`):

1. `frontend/src/lib/transport/` (WebSockets, binary encoding, clock syncing)
2. `frontend/src/lib/audio/` (Capture, precise Playback, timing tables)
3. `frontend/src/lib/sdk/` (Headless `VoicePipesSessionCore` and the `useVoicePipesSession` React hook)
4. `frontend/src/pages/playground/session/` (Types and `interruptReducer`)
5. `frontend/src/pages/playground/transcriptReducer.ts` (Pure reducer for chat logs)

*You do not need UI components like `Playground.tsx` or `SettingsPanel.tsx`; you will build your own UI around the headless hook.*

## 2. Core Architecture
- **Headless Core (`VoicePipesSessionCore`)**: Manages the underlying WebSocket connection, precise NTP-lite clock sync, and audio graph. It is framework agnostic.
- **Pure Reducers (`transcriptReducer`, `interruptReducer`)**: Deterministic state management for the conversation history. Separating this from the transport allows for bug-free concurrent rendering.
- **React Hook (`useVoicePipesSession`)**: The primary integration point. It bridges the headless core's events into React state.

## 3. Implementation Steps

### Step 1: Scaffold the State Hook
In your main component, initialize the session using the hook:

```tsx
import { useVoicePipesSession } from "./lib/sdk/react/useVoicePipesSession";

export function CustomVoiceApp() {
  const session = useVoicePipesSession();

  // Connect to the server
  const handleConnect = async () => {
    await session.connect("ws://localhost:8000/api/v1/ws", "websocket", {
      sample_rate: 48000,
      channels: 1,
    });
  };

  return (
    <div>
      <button onClick={handleConnect}>Connect</button>
      <button onClick={() => session.startMic()}>Start Mic</button>
      <button onClick={() => session.stopMic()}>Stop Mic</button>
      <button onClick={() => session.sendText("Hello AI!")}>Send Text</button>
    </div>
  );
}
```

### Step 2: Render the Transcript
The `session.transcript` array contains `TranscriptEntry` objects.
> [!IMPORTANT]
> A critical feature of VoicePipes is the "Progressive Reveal" (Karaoke) effect for the currently speaking agent. To implement this, you must split rendering between standard entries and the **Active Streaming Entry**.

Check if an agent entry is the active speaker using `session.speakingEntryIndex`.

```tsx
{session.transcript.map((entry, index) => {
  const isActiveAgentTurn = entry.role === "agent" && session.speakingEntryIndex === index;

  if (isActiveAgentTurn) {
    return (
      <StreamingAgentBubble 
        key={index} 
        entry={entry} 
        highlightInfo={session.highlightInfo} 
        entryIndex={index} 
      />
    );
  }

  // Normal rendering for User or Completed Agent turns
  return (
    <div key={index}>
      <strong>{entry.role}: </strong>
      <span>{entry.heardText ?? entry.text}</span>
    </div>
  );
})}
```

### Step 3: Implement the Karaoke Effect (`StreamingAgentBubble`)
The `StreamingAgentBubble` must use `highlightInfo.wordIndex` to selectively highlight words as the audio plays.

> [!TIP]
> **Best Practice**: Render future (unrevealed) words with reduced opacity (`opacity-40`) instead of completely hiding them. This prevents the UI from abruptly "shrinking" or "clearing" when streaming text (like OpenAI Realtime) arrives before the audio starts.

```tsx
function StreamingAgentBubble({ entry, highlightInfo }) {
  if (!highlightInfo) {
    // Audio hasn't started playing yet, show loading dots or dim full text
    return <div className="opacity-40">{entry.text}</div>;
  }

  const tokens = entry.text.split(/(\s+)/);
  let wordCounter = 0;
  const nodes = [];

  for (let i = 0; i < tokens.length; i++) {
    const token = tokens[i];
    const isWord = /\S/.test(token);
    let isHeard = false;

    if (isWord) {
      if (wordCounter <= highlightInfo.wordIndex) isHeard = true;
      wordCounter++;
    } else {
      if (wordCounter <= highlightInfo.wordIndex + 1) isHeard = true;
    }

    nodes.push(
      <span key={i} className={isHeard ? "" : "opacity-40 transition-opacity duration-150"}>
        {token}
      </span>
    );
  }

  return <div>{nodes}</div>;
}
```

### Step 4: Handle Visualisers and Controls
You can use `session.micLevel` and `session.playbackLevel` (which output 0.0 to 1.0) to drive CSS-based audio visualisers. 

> [!WARNING]
> Ensure you call `session.flushLocalAudio()` in a cleanup `useEffect` if your user navigates away from the voice interface to stop playback immediately, even if the WebSocket stays open.
