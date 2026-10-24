import { type PipelineState } from "../transport/types";

export type ClientFsmEvent =
  | { type: "SESSION_READY"; outputRate: number }
  | { type: "SERVER_STATE_CHANGED"; serverState: PipelineState }
  | { type: "AUDIO_META_ARRIVED"; utteranceId: number }
  | { type: "AUDIO_CHUNK_ARRIVED"; utteranceId: number; isFirst: boolean }
  | { type: "PLAYBACK_END"; utteranceId: number }
  | { type: "CLEAR_AUDIO"; utteranceId: number }
  | { type: "CONNECTION_LOST" };

export type ClientFsmAction =
  | { type: "START_PLAYBACK_SYSTEM"; sampleRate: number }
  | { type: "STOP_PLAYBACK_SYSTEM" }
  | { type: "OPEN_BOT_TYPING_BUBBLE" }
  | { type: "ROTATE_BUBBLE_TO_KARAOKE"; utteranceId: number }
  | { type: "FREEZE_BUBBLE_AT_POSITION"; utteranceId: number }
  | { type: "SEND_PLAYBACK_COMPLETE"; utteranceId: number }
  | { type: "SEND_PLAYBACK_REPORT"; utteranceId: number };

/**
 * A finite state machine modeling the client-side pipeline playback state.
 * 
 * ⚠️ Note: A graphical representation and state transition guide for this FSM
 * is maintained in the docs folder.
 * Please keep the diagram updated if you add or change state transitions!
 */
export class PureVoicePipesClientFsm {
  private state: PipelineState | "idle" = "idle";
  private activeUtteranceId: number | null = null;
  private playbackSamplerate = 48000;

  public transition(event: ClientFsmEvent): ClientFsmAction[] {
    const actions: ClientFsmAction[] = [];

    switch (this.state) {
      case "idle":
        if (event.type === "SESSION_READY") {
          this.state = "listening";
          this.playbackSamplerate = event.outputRate;
          actions.push({ type: "START_PLAYBACK_SYSTEM", sampleRate: this.playbackSamplerate });
        }
        break;

      case "listening":
        if (event.type === "SERVER_STATE_CHANGED" && event.serverState === "processing") {
          this.state = "processing";
          actions.push({ type: "OPEN_BOT_TYPING_BUBBLE" });
        } else if (event.type === "SERVER_STATE_CHANGED" && event.serverState === "speaking") {
          this.state = "speaking";
        } else if (event.type === "AUDIO_CHUNK_ARRIVED") {
          this.state = "speaking";
          this.activeUtteranceId = event.utteranceId;
          actions.push({ type: "ROTATE_BUBBLE_TO_KARAOKE", utteranceId: event.utteranceId });
        } else if (event.type === "AUDIO_META_ARRIVED") {
          this.state = "speaking";
          this.activeUtteranceId = event.utteranceId;
          actions.push({ type: "ROTATE_BUBBLE_TO_KARAOKE", utteranceId: event.utteranceId });
        } else if (event.type === "CLEAR_AUDIO") {
          // Very late clear (turn already wound down). Nothing is playing,
          // but the flush protocol must still run: it marks the utterance
          // flushed (so its late audio gets dropped) and completes the
          // server's draining handshake.
          actions.push({ type: "FREEZE_BUBBLE_AT_POSITION", utteranceId: event.utteranceId });
          actions.push({ type: "SEND_PLAYBACK_REPORT", utteranceId: event.utteranceId });
          actions.push({ type: "SEND_PLAYBACK_COMPLETE", utteranceId: event.utteranceId });
        }
        break;

      case "processing":
        if (event.type === "AUDIO_META_ARRIVED") {
          this.state = "speaking";
          this.activeUtteranceId = event.utteranceId;
          actions.push({ type: "ROTATE_BUBBLE_TO_KARAOKE", utteranceId: event.utteranceId });
        } else if (event.type === "AUDIO_CHUNK_ARRIVED") {
          this.state = "speaking";
          this.activeUtteranceId = event.utteranceId;
          actions.push({ type: "ROTATE_BUBBLE_TO_KARAOKE", utteranceId: event.utteranceId });
        } else if (event.type === "SERVER_STATE_CHANGED" && event.serverState === "speaking") {
          this.state = "speaking";
        } else if (event.type === "SERVER_STATE_CHANGED" && event.serverState === "listening") {
          this.state = "listening";
          this.activeUtteranceId = null;
        } else if (event.type === "CLEAR_AUDIO") {
          // Barge-in landed before this turn's first audio chunk reached us
          // (server-side barge during Processing does exactly this). Without
          // this transition the flush protocol never runs: the utterance is
          // never marked flushed (its late audio then plays), no
          // PlaybackReport/Complete is sent (playback tracking stays anchored
          // to the PREVIOUS utterance and over-reports), and the playback
          // pipeline desyncs for the following turns.
          actions.push({ type: "FREEZE_BUBBLE_AT_POSITION", utteranceId: event.utteranceId });
          actions.push({ type: "SEND_PLAYBACK_REPORT", utteranceId: event.utteranceId });
          actions.push({ type: "SEND_PLAYBACK_COMPLETE", utteranceId: event.utteranceId });
          this.state = "listening";
          this.activeUtteranceId = null;
        }
        break;

      case "speaking":
        if (event.type === "AUDIO_CHUNK_ARRIVED") {
          if (event.isFirst || this.activeUtteranceId !== event.utteranceId) {
            this.activeUtteranceId = event.utteranceId;
            actions.push({ type: "ROTATE_BUBBLE_TO_KARAOKE", utteranceId: event.utteranceId });
          }
        } else if (event.type === "AUDIO_META_ARRIVED") {
          if (this.activeUtteranceId !== event.utteranceId) {
            this.activeUtteranceId = event.utteranceId;
            actions.push({ type: "ROTATE_BUBBLE_TO_KARAOKE", utteranceId: event.utteranceId });
          }
        } else if (event.type === "PLAYBACK_END" && event.utteranceId === this.activeUtteranceId) {
          actions.push({ type: "SEND_PLAYBACK_COMPLETE", utteranceId: event.utteranceId });
          this.state = "listening";
          this.activeUtteranceId = null;
        } else if (event.type === "SERVER_STATE_CHANGED" && event.serverState === "draining") {
          this.state = "draining";
        } else if (event.type === "SERVER_STATE_CHANGED" && event.serverState === "listening") {
          this.state = "listening";
          this.activeUtteranceId = null;
        } else if (event.type === "SERVER_STATE_CHANGED" && event.serverState === "processing") {
          this.state = "processing";
          this.activeUtteranceId = null;
          actions.push({ type: "OPEN_BOT_TYPING_BUBBLE" });
        } else if (event.type === "CLEAR_AUDIO") {
          actions.push({ type: "FREEZE_BUBBLE_AT_POSITION", utteranceId: event.utteranceId });
          actions.push({ type: "SEND_PLAYBACK_REPORT", utteranceId: event.utteranceId });
          actions.push({ type: "SEND_PLAYBACK_COMPLETE", utteranceId: event.utteranceId });
          this.state = "listening";
          this.activeUtteranceId = null;
        }
        break;

      case "draining":
        if (event.type === "AUDIO_CHUNK_ARRIVED") {
          if (event.isFirst || this.activeUtteranceId !== event.utteranceId) {
            this.activeUtteranceId = event.utteranceId;
            actions.push({ type: "ROTATE_BUBBLE_TO_KARAOKE", utteranceId: event.utteranceId });
          }
        } else if (event.type === "AUDIO_META_ARRIVED") {
          if (this.activeUtteranceId !== event.utteranceId) {
            this.activeUtteranceId = event.utteranceId;
            actions.push({ type: "ROTATE_BUBBLE_TO_KARAOKE", utteranceId: event.utteranceId });
          }
        } else if (event.type === "PLAYBACK_END" && event.utteranceId === this.activeUtteranceId) {
          actions.push({ type: "SEND_PLAYBACK_COMPLETE", utteranceId: event.utteranceId });
          this.state = "listening";
          this.activeUtteranceId = null;
        } else if (event.type === "CLEAR_AUDIO") {
          actions.push({ type: "FREEZE_BUBBLE_AT_POSITION", utteranceId: event.utteranceId });
          actions.push({ type: "SEND_PLAYBACK_REPORT", utteranceId: event.utteranceId });
          actions.push({ type: "SEND_PLAYBACK_COMPLETE", utteranceId: event.utteranceId });
          this.state = "listening";
          this.activeUtteranceId = null;
        } else if (event.type === "SERVER_STATE_CHANGED" && event.serverState === "listening") {
          this.state = "listening";
          this.activeUtteranceId = null;
        } else if (event.type === "SERVER_STATE_CHANGED" && event.serverState === "processing") {
          this.state = "processing";
          this.activeUtteranceId = null;
          actions.push({ type: "OPEN_BOT_TYPING_BUBBLE" });
        } else if (event.type === "SERVER_STATE_CHANGED" && event.serverState === "speaking") {
          this.state = "speaking";
        }
        break;
    }

    if (event.type === "CONNECTION_LOST") {
      this.state = "idle";
      this.activeUtteranceId = null;
      actions.push({ type: "STOP_PLAYBACK_SYSTEM" });
    }

    return actions;
  }

  public getCurrentState(): PipelineState | "idle" {
    return this.state;
  }

  public getActiveUtteranceId(): number | null {
    return this.activeUtteranceId;
  }
}
