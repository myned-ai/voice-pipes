# Regression: `beginUtterance`-after-`enqueue` silences the word-reveal pipeline

## Symptom

The last spoken word never appeared in the chat transcript for normal (non-interrupted)
turns. On very short responses (single fragment / few chunks) the bubble showed
`<TypingDots>` for the entire duration instead of progressive word reveal.

## Root cause

`handleAudioMessage` in `usePlaygroundSession.ts` called `beginUtterance` **after**
`enqueue` on the very first audio chunk of each new utterance:

```
// ❌ Old order – WRONG
playbackRef.current?.enqueue(samples, meta, seqNum, eou);   // (1) sets utteranceStartTime
...
if (!receivedFirstAudioRef.current) {
  playbackRef.current?.beginUtterance(id);                   // (2) RESETS utteranceStartTime to 0
}
```

`AudioPlaybackService.enqueue` calls `drainPendingQueue` internally, which
sets `utteranceStartTime = startAt` (the AudioContext timestamp of the scheduled
buffer) and fires `onPlaybackStart`.

Then `beginUtterance` immediately overwrites `utteranceStartTime = 0`.

`getPlayedDurationMs()` guards on `utteranceStartTime === 0` and returns `0`:

```ts
getPlayedDurationMs(): number {
  if (!this.ctx || this.utteranceStartTime === 0) return 0;   // ← always returns 0
  ...
}
```

`createPlaybackSource` feeds `getPlayedDurationMs()` into `getNowMs()`, so the
`PlaybackClock` always delivered `nowMs = 0`.

`WordTimingTable.wordsHeardAtMs(0)` returns `0` (no words heard).

`computeHighlight` with `wordsHeard = 0` returns `null`.

`StreamingAgentBubble` renders `<TypingDots>` when `highlightInfo === null`,
so the transcript bubble showed animated dots for the whole utterance and
**never revealed any words**.

## Secondary effect on `preEnqueueMs`

`preEnqueueMs` was captured before `enqueue` (correct) but also before
`beginUtterance`, which resets `totalEnqueuedDurationMs = 0`. For the **first**
chunk the captured value was 0 either way, so no visible damage — but the
comment was wrong ("captured before enqueue" implied it was capturing a
meaningful non-zero value when in fact `beginUtterance` would have zeroed it
regardless).

## Fix

Move `beginUtterance` (and the `receivedFirstAudioRef` guard) to **before**
`enqueue`. Then move `preEnqueueMs` capture to after `beginUtterance` (so its
comment is accurate: for the first chunk it reads 0 because `beginUtterance`
just zeroed `totalEnqueuedDurationMs`, and for subsequent chunks it reads the
accumulated total).

```
// ✅ New order – CORRECT
if (!receivedFirstAudioRef.current) {
  receivedFirstAudioRef.current = true;
  playbackRef.current?.beginUtterance(id);    // (1) reset tracking
  ...
}
const preEnqueueMs = playbackRef.current?.totalEnqueuedDurationMs ?? 0;  // (2) capture
playbackRef.current?.enqueue(samples, meta, seqNum, eou);                // (3) schedules audio,
                                                                          //     sets utteranceStartTime
```

## Regression tests added

- `playback.test.ts` → `"beginUtterance before enqueue preserves utteranceStartTime"`  
  Directly asserts that `getPlayedDurationMs()` returns a positive value after
  the correct ordering, and returns 0 with the old (wrong) ordering.

- `playback.test.ts` → `"beginUtterance after enqueue corrupts utteranceStartTime (documents the bug)"`  
  The negative-path test explicitly documents the broken ordering so any future
  refactor that moves the call order will immediately fail a clear test.

- `word-reveal-pipeline.test.ts` → end-to-end integration tests that drive the
  full `PlaybackSource → PlaybackClock → useSynchronizedSpeech → computeHighlight`
  chain for a single-fragment utterance and assert the last word is eventually
  revealed.
