import { useCallback, useEffect, useRef, useState } from "react";
import { AudioCaptureService } from "../../../lib/audio/capture";
import type { AudioProcessingOptions } from "../../../lib/audio/capture";
import { AudioPlaybackService, type PlaybackSchedulingOptions } from "../../../lib/audio/playback";
import { CaptionSync } from "../../../lib/audio/caption-sync";
import { type PlaybackOptions, DEFAULT_PREBUFFER_MS } from "../session/types";

interface UseAudioPipelineProps {
  onAudioData?: (samples: Int16Array, seq: number) => void;
  onPlaybackStart?: (uttId: number) => void;
  onPlaybackEnd?: (uttId: number) => void;
  onSentenceStart?: (text: string, idx: number) => void;
  onWordHighlight?: (sentenceIdx: number, wordIdx: number, word: string) => void;
  onSentenceComplete?: (sentenceIdx: number) => void;
}

export function useAudioPipeline({
  onAudioData,
  onPlaybackStart,
  onPlaybackEnd,
  onSentenceStart,
  onWordHighlight,
  onSentenceComplete,
}: UseAudioPipelineProps = {}) {
  const captureRef = useRef<AudioCaptureService | null>(null);
  const playbackRef = useRef<AudioPlaybackService | null>(null);
  const captionSyncRef = useRef<CaptionSync | null>(null);
  const seqRef = useRef(0);
  const levelTimer = useRef<number>(0);

  const [micActive, setMicActive] = useState(false);
  const [micLevel, setMicLevel] = useState(0);
  const [playbackLevel, setPlaybackLevel] = useState(0);

  const playbackOptionsRef = useRef<PlaybackOptions>({
    limitScheduledAudio: false,
    maxScheduledChunks: 2,
    prebufferMs: DEFAULT_PREBUFFER_MS,
  });

  // Level polling for visualiser
  useEffect(() => {
    levelTimer.current = window.setInterval(() => {
      setMicLevel(captureRef.current?.level ?? 0);
      setPlaybackLevel(playbackRef.current?.level ?? 0);
    }, 50);
    return () => window.clearInterval(levelTimer.current);
  }, []);

  const createPlaybackService = useCallback((sampleRate: number) => {
    const scheduling: PlaybackSchedulingOptions = playbackOptionsRef.current;
    const pb = new AudioPlaybackService(sampleRate, scheduling);

    pb.onPlaybackStart = (uttId: number) => {
      onPlaybackStart?.(uttId);
    };

    pb.onPlaybackEnd = (uttId: number) => {
      onPlaybackEnd?.(uttId);
    };

    if (!captionSyncRef.current) {
      captionSyncRef.current = new CaptionSync();
    }
    const cs = captionSyncRef.current;
    cs.onSentenceStart = (text: string, idx: number) => {
      onSentenceStart?.(text, idx);
    };
    cs.onWordHighlight = (sentenceIdx: number, wordIdx: number, word: string) => {
      onWordHighlight?.(sentenceIdx, wordIdx, word);
    };
    cs.onSentenceComplete = (sentenceIdx: number) => {
      onSentenceComplete?.(sentenceIdx);
    };

    pb.onChunk = (chunk) => {
      if (cs && !chunk.meta.caption) {
        // Additional audio for an existing sentence lengthens it.
        const durationSec = chunk.endTime - chunk.startTime;
        cs.extendLastSentence(durationSec);
      }
    };

    playbackRef.current = pb;
    return pb;
  }, [onPlaybackStart, onPlaybackEnd, onSentenceStart, onWordHighlight, onSentenceComplete]);

  const startMic = useCallback(async (
    sampleRate = 48000,
    processing?: AudioProcessingOptions,
  ) => {
    if (captureRef.current?.active) return;

    const capture = new AudioCaptureService();
    captureRef.current = capture;

    capture.onSamples = (samples: Int16Array) => {
      const seq = seqRef.current++;
      onAudioData?.(samples, seq);
    };

    // Capture resamples when the browser ignores the requested rate, so this
    // always equals `sampleRate`. No mismatch path: the server fixes the input
    // rate at the handshake and ignores a later `configure`.
    await capture.start(sampleRate, processing);
    const actual = capture.actualSampleRate;

    setMicActive(true);
    return actual || sampleRate;
  }, [onAudioData]);

  const stopMic = useCallback(() => {
    captureRef.current?.stop();
    captureRef.current = null;
    setMicActive(false);
  }, []);

  const stopAll = useCallback(() => {
    captureRef.current?.stop();
    captureRef.current = null;
    playbackRef.current?.stop();
    captionSyncRef.current?.flush();
    setMicActive(false);
  }, []);

  const resetSeq = useCallback(() => {
    seqRef.current = 0;
  }, []);

  return {
    captureRef,
    playbackRef,
    captionSyncRef,
    micActive,
    micLevel,
    playbackLevel,
    playbackOptionsRef,
    createPlaybackService,
    startMic,
    stopMic,
    stopAll,
    resetSeq,
  };
}