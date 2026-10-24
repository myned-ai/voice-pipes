// Audio capture service — microphone PCM i16 capture via Web Audio API.

import { debug, debugWarn } from "../debug";
import { StreamingResampler } from "./resampler";

export type AudioCaptureCallback = (samples: Int16Array) => void;

/**
 * The browser's own microphone processing.
 *
 * Chrome and Edge apply echo cancellation, noise suppression and automatic
 * gain before any audio reaches us, and they are aggressive: quiet or distant
 * speech can be gated to near-silence, which reaches the recogniser as a
 * pause rather than as words.
 *
 * On by default, because for an ordinary voice agent on a laptop they do more
 * good than harm. Turn them off when the audio itself is the subject --
 * recording a corpus to score turn detection, or diagnosing why speech is not
 * being transcribed -- since the point there is to capture what the microphone
 * actually heard rather than what the browser thought was worth keeping.
 */
export interface AudioProcessingOptions {
  echoCancellation?: boolean;
  noiseSuppression?: boolean;
  autoGainControl?: boolean;
}

/** How long to wait before concluding the mic graph never started. Long
 *  enough to clear worklet module loading and an autoplay-policy resume. */
const SILENT_MIC_CHECK_MS = 1500;

export const DEFAULT_AUDIO_PROCESSING: Required<AudioProcessingOptions> = {
  echoCancellation: true,
  noiseSuppression: true,
  autoGainControl: true,
};

const WORKLET_CODE = `
class PcmCaptureProcessor extends AudioWorkletProcessor {
  process(inputs) {
    const ch = inputs[0]?.[0];
    if (ch && ch.length > 0) {
      // Convert float32 (-1..1) to int16
      const i16 = new Int16Array(ch.length);
      for (let i = 0; i < ch.length; i++) {
        const s = Math.max(-1, Math.min(1, ch[i]));
        i16[i] = s < 0 ? s * 0x8000 : s * 0x7fff;
      }
      this.port.postMessage(i16.buffer, [i16.buffer]);
    }
    return true;
  }
}
registerProcessor('pcm-capture', PcmCaptureProcessor);
`;

export class AudioCaptureService {
  private ctx: AudioContext | null = null;
  private stream: MediaStream | null = null;
  private source: MediaStreamAudioSourceNode | null = null;
  private worklet: AudioWorkletNode | null = null;
  private _active = false;
  private _muted = false;
  /** Set when the `AudioContext` did not honour the requested rate; converts
   *  every block so the wire rate is the one we promised. */
  private resampler: StreamingResampler | null = null;
  /** The rate `onSamples` delivers — what actually goes on the wire. */
  private emitRate = 0;
  /** Worklet blocks received since {@link start}. Zero after the check below
   *  means the graph never ran, which is not the same as a quiet room. */
  private framesSeen = 0;
  /** Largest absolute sample seen since {@link start}. Frames arriving with
   *  this still at zero means the track is delivering digital silence, which
   *  is a different fault from the graph never running. */
  private peakSeen = 0;
  private silentSince: number | null = null;

  onSamples: AudioCaptureCallback | null = null;
  /** Called once if no audio arrives shortly after {@link start}. */
  onSilentMic: (() => void) | null = null;
  /** Rolling RMS level 0..1 for visualisation. */
  level = 0;

  get active(): boolean {
    return this._active;
  }
  get muted(): boolean {
    return this._muted;
  }
  /** The rate of the samples handed to `onSamples` — i.e. what the server
   *  must be told. Equals the rate passed to {@link start}, because a
   *  mismatched `AudioContext` is resampled rather than reported. */
  get actualSampleRate(): number {
    return this.emitRate;
  }

  /** Rate the browser actually gave the `AudioContext`. Diagnostics only —
   *  it differs from {@link actualSampleRate} whenever resampling is active. */
  get contextSampleRate(): number {
    return this.ctx?.sampleRate ?? 0;
  }

  /** Whether a rate conversion sits between the mic and `onSamples`. */
  get isResampling(): boolean {
    return this.resampler !== null;
  }

  async start(
    sampleRate = 48000,
    processing: AudioProcessingOptions = {},
  ): Promise<void> {
    if (this._active) return;

    const audioProcessing = { ...DEFAULT_AUDIO_PROCESSING, ...processing };
    // No `sampleRate` constraint: the device runs at whatever rate it likes
    // and the conversion happens below, in one place we control. Asking the
    // device for the wire rate as well as the context would leave the two
    // free to disagree -- a track at one rate feeding a graph at another,
    // which Chrome resolves by resampling and sometimes by delivering nothing
    // at all.
    this.stream = await navigator.mediaDevices.getUserMedia({
      audio: {
        channelCount: 1,
        ...audioProcessing,
      },
    });

    // Deliberately no `sampleRate` here, so the context runs at the device's
    // own rate and the microphone reaches the graph unconverted.
    //
    // Asking for a context at the wire rate looks tidier and is a trap: a
    // 48 kHz microphone feeding a 16 kHz context is still downsampled, only
    // by `createMediaStreamSource` rather than by us, and that conversion
    // aliases. Aliasing folds high frequencies back into the speech band, so
    // the result still sounds like speech and its gross band energy is
    // unchanged -- but the fine spectral structure a recogniser's mel
    // front-end reads is gone, and transcripts come back empty. It only bites
    // pipelines that negotiate a rate below the device's; a 48 kHz
    // speech-to-speech session matches the device and sounds fine, which
    // makes the fault look like a property of the pipeline rather than of
    // capture.
    this.ctx = new AudioContext();

    // Browsers may auto-suspend AudioContext due to autoplay policy; resume
    // explicitly so the worklet actually processes audio and emits samples.
    if (this.ctx.state === "suspended") {
      await this.ctx.resume();
    }

    // Convert here rather than reporting the device rate upstream and hoping
    // the server can adapt — it cannot, `client_sample_rate` is bound once at
    // the handshake — so the promised rate is the delivered rate.
    //
    // This is the ordinary path now, not an exception: most devices run at
    // 48 kHz and most cascade pipelines ask for 16 kHz. `StreamingResampler`
    // low-passes before it decimates, which is the part the browser's own
    // conversion skips.
    this.emitRate = sampleRate;
    if (this.ctx.sampleRate !== sampleRate) {
      this.resampler = new StreamingResampler(this.ctx.sampleRate, sampleRate);
      debug(
        `[AudioCapture] AudioContext at ${this.ctx.sampleRate} Hz — resampling to ${sampleRate} Hz`,
      );
    } else {
      this.resampler = null;
      debug(`[AudioCapture] AudioContext running at ${this.ctx.sampleRate} Hz`);
    }
    debug(`[AudioCapture] AudioContext state: ${this.ctx.state}`);
    this.source = this.ctx.createMediaStreamSource(this.stream);

    // Load worklet from inline blob
    const blob = new Blob([WORKLET_CODE], { type: "application/javascript" });
    const url = URL.createObjectURL(blob);
    try {
      await this.ctx.audioWorklet.addModule(url);
    } finally {
      URL.revokeObjectURL(url);
    }

    this.worklet = new AudioWorkletNode(this.ctx, "pcm-capture");
    this.worklet.port.onmessage = (e: MessageEvent) => {
      const buf = e.data as ArrayBuffer;
      const samples = new Int16Array(buf);
      if (samples.length === 0) return;
      this.framesSeen++;
      if (this.peakSeen === 0) {
        for (let i = 0; i < samples.length; i++) {
          const a = samples[i] < 0 ? -samples[i] : samples[i];
          if (a > this.peakSeen) this.peakSeen = a;
        }
      }
      // Compute RMS level for visualisation
      let sum = 0;
      for (let i = 0; i < samples.length; i++) {
        const n = samples[i] / 32768;
        sum += n * n;
      }
      this.level = Math.sqrt(sum / samples.length);

      if (!this._muted) {
        // Level stays on the pre-conversion block so the client VAD sees the
        // signal it has always seen; RMS is rate-invariant anyway.
        const out = this.resampler ? this.resampler.process(samples) : samples;
        if (out.length > 0) this.onSamples?.(out);
      }
    };

    this.worklet.port.onmessageerror = (e) => {
      debugWarn(`[AudioCapture] worklet port message error:`, e);
    };

    this.source.connect(this.worklet);
    this.worklet.connect(this.ctx.destination); // needed to keep worklet alive in graph
    this._active = true;
    debug(`[AudioCapture] capture started, worklet connected`);

    // A microphone that delivers nothing is indistinguishable from a silent
    // room: the level meter sits at zero and every log line still says the
    // session is listening. The graph is either running within a block or two
    // or it is not running at all, so one late check separates the two.
    this.silentSince = window.setTimeout(() => {
      if (!this._active || this.peakSeen > 0) return;
      this.onSilentMic?.();

      // `console.error`, not the gated debug helpers: a capture path that
      // produces nothing is a real failure, and gating it behind an opt-in
      // flag is what made it invisible in the first place.
      //
      // The two cases look identical from outside -- meter at zero, session
      // listening, no error -- but they have nothing in common underneath.
      // No frames means the graph never ran. Frames of pure silence mean the
      // graph is fine and the track itself is muted or has been handed to
      // another consumer.
      const track = this.stream?.getAudioTracks()[0];
      const where =
        `context ${this.ctx?.sampleRate} Hz, track ` +
        `${track?.getSettings().sampleRate ?? "?"} Hz, ` +
        `track state ${track?.readyState ?? "?"}` +
        `${track?.muted ? ", muted" : ""}`;
      console.error(
        this.framesSeen === 0
          ? `[AudioCapture] no audio ${SILENT_MIC_CHECK_MS} ms after start (${where}) — ` +
              `the microphone graph is not delivering frames`
          : `[AudioCapture] ${this.framesSeen} blocks in ${SILENT_MIC_CHECK_MS} ms but every ` +
              `sample is zero (${where}) — the graph is running and the track is silent`,
      );
    }, SILENT_MIC_CHECK_MS);
  }

  stop(): void {
    if (this.silentSince !== null) {
      clearTimeout(this.silentSince);
      this.silentSince = null;
    }
    this.framesSeen = 0;
    this.peakSeen = 0;
    this.worklet?.disconnect();
    this.source?.disconnect();
    this.stream?.getTracks().forEach((t) => t.stop());
    void this.ctx?.close();
    this.worklet = null;
    this.source = null;
    this.stream = null;
    this.ctx = null;
    this.resampler = null;
    this.emitRate = 0;
    this._active = false;
    this._muted = false;
    this.level = 0;
  }

  setMuted(muted: boolean): void {
    this._muted = muted;
  }
}
