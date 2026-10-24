# Why Your Voice Agent's "Interruption" Feature Is Probably Broken

**Status:** active
**Last verified:** 2026-09-06 (moved into the public tree; content unchanged)
**Owner:** Docs

*A state-machine deep dive into barge-in correctness*

---

## The deceptively simple feature

Every demo of a voice agent shows the same trick: the assistant starts answering, the user cuts in, the assistant stops mid-word and listens. It looks like one feature. It is, in fact, a distributed consensus problem about *what just happened*, played out across a microphone, a VAD, an STT provider, an LLM, a TTS engine, an audio pacer, a WebSocket, a browser scheduler, and the Web Audio API's sample-accurate playback graph.

Get any one of those wrong and you get the symptom every voice-agent user hates: you say "wait — " and the bot keeps talking for another 800 ms before it notices.

This article walks through the failure modes we hit building a real-time voice pipeline in Rust, why the "obvious" implementations are wrong, and the state machine you actually need.

---

## 1. The naïve model (and why it fails)

The textbook design has two states:

```
Listening  ⇄  Speaking
```

When the user talks, you're `Listening`. When the bot replies, you're `Speaking`. Barge-in means: while in `Speaking`, if you detect user speech, transition back to `Listening` and stop the audio.

This is wrong for three independent reasons.

### 1.1 "Speaking" on the server is not "audible" on the client

The server thinks it's in `Speaking` the moment the first TTS chunk leaves the encoder. The client is *audibly* speaking some indeterminate time later — after the chunk has crossed the WebSocket, been decoded, scheduled into an `AudioBufferSourceNode`, and reached the speakers.

Worse, when the server *stops* generating, the client is still draining a buffer. In our pipeline the audio pacer drip-feeds frames at 1.5× real-time precisely so the client always has a cushion. That cushion is also a 200–500 ms window in which the server believes it's done speaking but the user is still hearing the bot.

If the user starts talking during that window, what state are you in? The server says `Listening`. The client says "I can't be listening, I'm playing audio into the user's ears, which my mic will pick up as bot voice and feed back to STT." Both are correct. Neither is sufficient.

This is why we added a fourth state:

```rust
pub enum TurnState {
    Listening,
    Processing,   // STT final → LLM running
    Speaking,     // TTS frames being emitted
    Draining,     // server done emitting, client still playing
}
```

`Draining` is the state most pipelines are missing. It exists to answer one question: *should a barge-in trigger right now?* The answer is yes — and our VAD gating reflects that:

```rust
// voice-pipes/src/vad/engine.rs
match turn_state {
    TurnState::Speaking | TurnState::Draining => true,
    TurnState::Processing => settings.barge_in_during_processing(),
    TurnState::Listening => false,
}
```

Note the `match`, not `if let`. With four states and a fifth on the horizon, an `if state == Speaking` check is a silent bug waiting for a refactor.

### 1.2 The cancel signal arrives in three places at three times

When the user interrupts, *something* has to:

1. Stop the LLM token stream.
2. Stop the TTS synthesizer.
3. Drop the audio frames already queued in the pacer.
4. Send a `clear_audio` control message to the client.
5. Make the client flush its scheduled `AudioBufferSourceNode`s.
6. Tell the LLM history that the assistant only said *part* of its planned reply.
7. Reset the turn state.

Steps 1–4 happen on the server. Step 5 happens in the browser. Step 6 happens *after* step 5, because only the client knows how much audio it actually played before flushing.

A correct implementation is a small protocol:

```
server: detect speech → CancellationToken.cancel()
        ↓
        suppress_audio = true   (drops frames already in the channel)
        ClearAudio{utterance_id} → client
        state → Listening
        ↓
client: receive ClearAudio
        playback.flush() → stops every active AudioBufferSourceNode
        flushedUtteranceRef = utteranceIdRef   (blocks late arrivals)
        sendPlaybackReport(utterance_id, playedMs)
        ↓
server: receive PlaybackReport
        truncate assistant history to playedMs worth of words
```

Skip the `PlaybackReport` round-trip and your conversation history will say the bot finished a sentence it never spoke. The next LLM turn will reference content the user never heard. The user will think the bot is hallucinating. The bot is not hallucinating; you lied to it about its own past.

### 1.3 The WebSocket is not a synchronous channel

Even after `suppress_audio = true`, audio frames already in the OS send buffer will arrive at the client *after* `ClearAudio`. The client must be able to recognize and drop them. We do this with an `flushedUtteranceRef` guard: any audio chunk whose `utterance_id` is ≤ the last flushed ID is silently dropped on receipt.

This is the single most underrated invariant in barge-in design: **every audio chunk must carry an utterance ID, and the client must be willing to throw chunks away.** If your wire format only sends raw PCM frames you have already lost.

---

## 2. The VAD source problem

Naïve barge-in does this:

```rust
if rms(audio_chunk) > THRESHOLD && consecutive_chunks > N {
    cancel();
}
```

This is what we shipped first. It missed roughly one in three interruptions in real rooms. The reasons, in order of how much they hurt:

1. **Threshold/gain coupling.** A laptop mic at default gain in a quiet office produces RMS values that hover *just under* whatever threshold you pick. Lower the threshold and the bot's own audio bleeds through echo cancellation and self-interrupts. Raise it and quiet users get ignored.
2. **The state-gate race.** RMS-based detection only counts speech while `state == Speaking`. If the user starts speaking 50 ms before the state transitions out of `Processing`, those frames are silently discarded and the grace counter starts from zero.
3. **The provider already knows.** AssemblyAI, OpenAI Realtime, Deepgram — they all emit a `speech_started` or `partial_transcript` event the moment they detect user voice. Their VAD is better than your RMS check. *You are ignoring it.*

The fix is to stop hardcoding the detector and treat barge-in detection as a pluggable event source:

```rust
pub trait VadEngine {
    fn events(&self) -> mpsc::Receiver<VadEvent>;
}

pub enum VadEvent {
    SpeechStarted { at: Instant, source: VadSource },
    SpeechEnded   { at: Instant },
}
```

Now `RmsVadEngine`, `ProviderVadEngine` (adapts STT partials), `ClientVadEngine` (adapts a `{action: "cancel"}` from the UI), and a future `SileroVadEngine` are all implementations of the same trait. The orchestrator listens on a single `mpsc::Receiver<VadEvent>` and doesn't care where the signal came from.

The interesting part is what you do with multiple engines at once. We use a **first-to-fire composite**:

```
ClientVadEngine    ─┐
RmsVadEngine       ─┼─► CompositeVadEngine ─► CancellationToken
ProviderVadEngine  ─┘
```

Each engine runs concurrently. Whichever detects speech first wins. The client button is hyper-fast (zero round-trip — flushes locally before the server even knows). The local RMS catches loud interruptions in ~30 ms. The provider VAD is the safety net for the quiet user the local math missed. You stop choosing between latency and accuracy and instead get the minimum of both.

---

## 3. History truncation: word timings or it didn't happen

Suppose the LLM generated 80 words. TTS spoke 30. The user interrupted. What goes into the conversation history?

Three options, ranked from worst to least-bad:

1. **All 80 words.** The model thinks it told the user something it didn't. Catastrophic for any agent that references prior turns ("as I mentioned…").
2. **Sentence-level truncation.** Round to the last fully-spoken sentence. Acceptable, but you lose the partial sentence the user actually heard, and the LLM has no idea where it was cut off.
3. **Word-level truncation.** TTS engines that emit `WordBoundary` events (Azure, Piper with timestamps, OpenAI streaming) let you record `(word, start_ms, end_ms)` for every spoken word. On `PlaybackReport{played_ms}`, walk the ledger and keep only words whose `end_ms ≤ played_ms`.

We do (3) when available and fall back to (2). The data structure is small:

```rust
pub struct AudioLedgerEntry {
    pub utterance_id: u64,
    pub text: String,
    pub start_ms: u64,
    pub end_ms: u64,
    pub word_timings: Option<Vec<WordTiming>>,
}
```

The cost is one ledger entry per TTS chunk. The benefit is that interrupted turns appear in history exactly as the user experienced them.

---

## 4. The state machine, complete

Putting it together, the real diagram is:

```
                  ┌──────── VadEvent::SpeechStarted ────────┐
                  │                                         │
                  ▼                                         │
┌──────────┐  STT final  ┌────────────┐ first TTS  ┌────────────┐
│ Listening├────────────►│ Processing ├───────────►│  Speaking  │
└────▲─────┘             └─────┬──────┘            └──────┬─────┘
     │                         │                          │
     │                         │ (barge_in_during_         │ TTS done,
     │                         │  processing = true)       │ pacer not drained
     │                         ▼                          ▼
     │                   ┌──────────┐   PlaybackReport ┌──────────┐
     └───────────────────┤  Cancel  │◄─────────────────┤ Draining │
                         └──────────┘                  └──────────┘
```

The non-obvious edges:

- **`Processing → Cancel`**: the user interrupted *while the LLM was still thinking*. Whether you allow this is a config flag (`barge_in_during_processing`). Allowing it makes the agent feel snappier; disallowing it prevents the user from cancelling a reply they haven't heard yet because they coughed.
- **`Draining → Cancel`**: the user interrupted *after* the server finished generating but *before* the client finished playing. This is the state most implementations don't model and the one that produces the "I waited a beat after it stopped, then talked, and it stepped on me" UX bug.
- **`Cancel → Listening`** is gated on receiving `PlaybackReport` from the client. Without it, you don't know how much to truncate.
- **`Draining → Processing`**: a new utterance (typed text, or speech right after an interrupt) may commit while the previous turn's tail is still playing. The server is done with that turn, so the next one starts without waiting for `PlaybackComplete`; the client is told to drop the tail first. From `Speaking` the same commit is refused, because audio for the current turn is still being produced and a barge-in has to tear it down first.
- **Interrupts after the turn already ended** (the user talks over the tail in `Draining`) only cut the tail: one `ClearAudio`, no second `turn_metrics`, and the turn is counted once in the session summary.

The client is only ever told about a state the server actually entered. A trigger the machine rejects (say, an agent turn arriving while a user turn is active) changes nothing and sends no `state_changed`; it is logged on the server instead.

---

## 5. How to test this without losing your mind

Barge-in bugs are non-deterministic by nature: they depend on network jitter, audio scheduler quanta, and human reflexes. Three things made our test suite tractable:

1. **Exhaustive `match` on `TurnState`.** Every place the state is read is a `match` with all four arms. When we added `Draining`, the compiler walked us to every site that needed to think about it. An `if state == Speaking` check would have silently kept the old behavior.
2. **A mock `VadEngine` that emits scripted `VadEvent`s at deterministic times.** Decouples barge-in logic from real audio.
3. **Per-turn metrics with timestamps for every transition.** `state_changed_at`, `vad_fired_at`, `clear_audio_sent_at`, `playback_report_received_at`. The metric `vad_fired_at − speech_actually_started_at` is your barge-in latency. The metric `clear_audio_sent_at − vad_fired_at` is your server overhead. The metric `playback_report_received_at − clear_audio_sent_at` is your client RTT plus flush time. You cannot tune what you cannot see.

---

## 6. What "good" feels like

The numbers we target, end-to-end on a local LAN with AssemblyAI as the provider VAD:

| Stage | p50 | p99 |
|------|-----|-----|
| User starts speaking → VAD fires | 80 ms | 180 ms |
| VAD fires → `ClearAudio` sent | 5 ms | 20 ms |
| `ClearAudio` sent → client `flush()` | 25 ms | 60 ms |
| Total: speech → silence | **110 ms** | **260 ms** |

Below ~150 ms p50, interruption feels like talking to a human. Above ~300 ms p99, users start repeating themselves.

The single biggest lever is **which VAD fired**. Provider VAD wins on accuracy but adds 80–120 ms over local RMS. The composite engine lets you have local RMS as the fast path and provider VAD as the recall safety net, which is the only configuration we've found that hits both p50 and p99 targets simultaneously.

---

## Takeaways

1. **Two states is not enough.** You need at least `Listening / Processing / Speaking / Draining`, and you need exhaustive matching on them.
2. **The server's "done speaking" is not the client's "done speaking."** The `Draining` state and the `PlaybackReport` round-trip are how you reconcile them.
3. **Don't write your own VAD when the STT provider already has one.** Put barge-in detection behind a trait and race multiple sources.
4. **Truncate history at word granularity.** The LLM should believe exactly what the user heard, no more.
5. **Every audio frame needs an utterance ID, and the client must drop late arrivals.** TCP doesn't save you from in-flight bytes.

The features that look like one button — *interrupt me* — are the ones with the deepest state machines. Build the state machine first; the button is the easy part.
