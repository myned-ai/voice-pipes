# Dual-Track Text Sanitizer, TTS Instruction Exception List, and Session Details Inspector

**Status:** Planned
**Last verified:** 2026-11-05
**Owner:** Engine + Admin

---

## Quick-Start Prompt for Implementation Session

Copy and paste the prompt below into a new coding session:

```text
Implement `docs/internal/plans/PLAN-DUAL-TRACK-SANITIZER.md` in full (Phases A through E, plus all verification tests and doc updates). Follow every rule in `AGENTS.md`: `#![forbid(unsafe_code)]`, zero warnings under `cargo clippy --workspace -- -D warnings`, no `unwrap_used` / `expect_used` / `indexing_slicing` in non-test Rust code, and pass `cargo test -p voice-pipes-cli --test workspace_rules`, backend unittest, and frontend vitest suites.
```

---

## 1. Context & Verified Codebase Findings

When an LLM streams a response into a voice pipeline, the text that looks best on screen (`display_text`) is frequently different from the text that should be handed to a TTS synthesizer (`spoken_text`):

1. **Markdown & Formatting Leak:** LLMs emit `**bold**`, `*italic*`, `__underline__`, `` `code` ``, `# Headings`, `- list items`, and `[Link Text](https://example.com)`. Today, TTS providers either read formatting punctuation aloud (*"asterisk asterisk"*) or spell out raw URLs.
2. **Reasoning Tag Leak:** Thinking models (Qwen3, DeepSeek-R1, etc.) emit `<think>...</think>` or `<|think|>...</|think|>` blocks across streaming token boundaries that must never be spoken aloud.
3. **Inline TTS Instructions & Phoneme Markup:**
   - **Kokoro / Misaki IPA syntax:** `[Kokoro](/kˈoʊkəɹoʊ/)` uses Markdown-like link syntax where the target is `/ipa/`. A naive Markdown link stripper would destroy the phoneme override, while keeping `(/kˈoʊkəɹoʊ/)` in `display_text` pollutes user captions.
   - **SSML & Paralinguistic Directives:** Providers like ElevenLabs, Cartesia, and Orpheus support inline directives such as `<break time="500ms"/>`, `<phoneme alphabet="ipa" ph="...">word</phoneme>`, `<emotion value="positivity:high"/>`, `<laugh>`, `<sigh>`, `[laughs]`, and `[whispers]`.
   - **Provider Mismatch Hazard:** If an LLM emits `[laughs]` or `<break time="500ms"/>` while the pipeline is configured with a basic TTS like `piper`, Piper literally speaks the raw tag syntax aloud unless stripped. Conversely, even when supported by the TTS engine, silent control tags like `<break time="500ms"/>` must not appear in user-facing captions (`display_text`).
4. **Domain & Brand Pronunciation (`[tts.lexicon]`):** Words like `"SQL"`, `"Myned"`, `"API"`, or `"kubectl"` should remain `"SQL"` in the UI transcript and LLM conversation history (`display_text`), but be synthesized as `"sequel"` in `spoken_text`.
5. **Verified Existing Seams in the Repo:**
   - **`voice-pipes/src/pipeline/tts.rs` (lines 593 & 786):** Currently runs `raw.text = strip_unspeakable(&raw.text)` in-place before synthesis, which only strips emojis and mutates `fragment.text` directly.
   - **`voice-pipes-sdk/src/sentence.rs` (`split_first_sentence`, line 145):** Currently splits streaming text on `.`, `!`, `?`, `,`, `;`, `:` with guards for abbreviations, decimals (`3.14`), initials, and URLs, but has no guard preventing splits inside `<break time="1.5s"/>` or `[word](/kˈoʊ.kə.ɹoʊ/)`.
   - **`voice-pipes/src/pipeline/types.rs` (`AudioLedger::text_at_offset`, lines 272–325):** On barge-in, `PlaybackReport` looks up `WordBoundary.word` (the **spoken** word reported by TTS) inside `entry.text` via `entry.text[search_from..].find(&w.word)`. If `entry.text` is `display_text` (`"Run **SQL** now"`) while TTS synthesized `spoken_text` (`"Run sequel now"`), `.find("sequel")` will fail unless `AudioLedgerEntry` tracks both `display_text` and `spoken_text` and maps spoken word indices back to display word boundaries.
   - **`voice-pipes/src/audit/streaming.rs` (lines 368–395) & `TurnCard.tsx` (lines 182–201):** `{utt:03}_llm_output.json` and `{utt:03}_tts_frag_{idx:03}.txt` already persist per-turn and per-fragment text, and `TurnCard.tsx` already has a collapsible disclosure pattern (`LLM prompt · N chars`) right below the AI response.

---

## 2. Architecture & Data Contracts

```
LLM Token Stream
       │
       ▼
┌──────────────────────────────────────────────────────────┐
│ 1. ThinkTagFilter (stateful across token chunks)         │
│    Strips <think>...</think> and <|think|>...</|think|>  │
└───────────────────────────┬──────────────────────────────┘
                            │
                            ▼
┌──────────────────────────────────────────────────────────┐
│ 2. split_first_sentence (voice-pipes-sdk/src/sentence.rs)│
│    Guards against splitting inside <...> or [...](/.../) │
└───────────────────────────┬──────────────────────────────┘
                            │
                            ▼
┌──────────────────────────────────────────────────────────┐
│ 3. sanitize_dual_track (voice-pipes-sdk/src/sanitize.rs) │
│    Inputs: raw sentence, provider allowlist,             │
│            config passthrough_tags, [tts.lexicon]        │
└───────────┬──────────────────────────────┬───────────────┘
            │                              │
            ▼                              ▼
   [ display_text ]               [ spoken_text + telemetry ]
  • Keeps Markdown (**bold**)    • Strips Markdown wrappers & emojis
  • Unwraps [Kokoro](/ipa/)      • Preserves allowed TTS tags & (/ipa/)
    -> "Kokoro"                  • Strips unsupported TTS tags (recorded
  • Strips <break.../> &           in `stripped_directives`)
     TTS control tags            • Applies [tts.lexicon] ("SQL" -> "sequel")
            │                              │
            ▼                              ▼
  Captions, Transcript,          TtsProvider::synthesize_stream,
  LLM History, AudioLedger       {utt}_tts_frag_{n}.txt,
                                 Session Details "TTS input" Inspector
```

### Core Types in `voice-pipes-sdk::sanitize`

```rust
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TtsDirectiveRecord {
    /// An inline tag or bracket directive preserved in `spoken_text` (e.g. `<break time="300ms"/>`, `[laughs]`).
    TagPassthrough { raw: String },
    /// An inline IPA phoneme link preserved for TTS and unwrapped in display (e.g. `[Kokoro](/kˈoʊkəɹoʊ/)`).
    IpaPhoneme { word: String, ipa: String },
    /// A whole-word lexicon substitution applied to `spoken_text` (e.g. `"SQL"` -> `"sequel"`).
    Lexicon { from: String, to: String },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StrippedDirectiveRecord {
    /// The raw tag/directive that was removed from both `display_text` and `spoken_text`.
    pub raw: String,
    /// Why it was stripped (e.g. `"not in passthrough_tags for provider 'piper'"`).
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DualTrackText {
    /// Clean text for UI captions, transcripts, and LLM history.
    pub display: String,
    /// Text formatted specifically for the active TTS engine.
    pub spoken: String,
    /// Directives and lexicon replacements active in `spoken`.
    pub directives: Vec<TtsDirectiveRecord>,
    /// TTS-like tags that were stripped because the active TTS provider does not support them.
    pub stripped: Vec<StrippedDirectiveRecord>,
}
```

---

## 3. Step-by-Step Implementation Plan

### Phase A: SDK Sanitizer Engine, Sentence Guard & Trait Extension (`voice-pipes-sdk`)

1. **`TtsProvider::supported_inline_tags` in `voice-pipes-sdk/src/tts.rs`:**
   Add a default method on `TtsProvider`:
   ```rust
   /// Inline control tag prefixes or tokens (e.g. `"<break"`, `"[laughs]"`, `"/ipa/"`)
   /// that this TTS engine natively understands and that must not be stripped
   /// from `spoken_text`. Default: `&[]`.
   fn supported_inline_tags(&self) -> &'static [&'static str] {
       &[]
   }
   ```
2. **Tag & Phoneme Link Guard in `voice-pipes-sdk/src/sentence.rs`:**
   - Update `split_first_sentence()` so punctuation (`.`, `!`, `?`, `,`, `;`, `:`) inside an unclosed angle-bracket tag `<...>` (such as `<break time="1.5s"/>` or `<emotion value="positivity:high"/>`) or inside a `[...](...)` link/phoneme construct is never treated as a sentence split point.
3. **Create `voice-pipes-sdk/src/sanitize.rs` (and export in `voice-pipes-sdk/src/lib.rs`):**
   - Move `is_unspeakable_symbol` and `strip_unspeakable` from `voice-pipes/src/pipeline/tts.rs` into `voice-pipes-sdk/src/sanitize.rs` (re-exporting in `tts.rs` if needed).
   - Implement **`ThinkTagFilter`**:
     - Stateful struct (`pub struct ThinkTagFilter { in_think: bool, pending: String }`) with `pub fn push(&mut self, chunk: &str) -> String` and `pub fn flush(&mut self) -> String`.
     - Handles `<think>...</think>` and `<|think|>...</|think|>` even when tags are split across arbitrary 1-byte or multi-byte streaming chunks.
   - Implement **`sanitize_dual_track(raw: &str, cfg: &SanitizeConfig) -> DualTrackText`**:
     - **IPA Phoneme Links `[word](/ipa/)`:**
       - Detected when a link target starts and ends with `/` (and is not `//` or a URL path).
       - `display` receives `word`.
       - If `"/ipa/"` or `"*"` is in the effective allowlist (`provider.supported_inline_tags()` $\cup$ `cfg.passthrough_tags`), `spoken` receives `[word](/ipa/)` and records `TtsDirectiveRecord::IpaPhoneme { word, ipa }`. Otherwise `spoken` receives `word` and records `StrippedDirectiveRecord`.
     - **Standard Markdown Links `[label](url)` & Bare URLs:**
       - `display` keeps `[label](url)` (or `label`); `spoken` unwraps to `label` and strips bare `https://...` / `http://...` URLs.
     - **Inline XML/SSML Tags (`<...>`) & Bracket Stage Directives (`[laughs]`, `[whispers]`, `[sighs]`, `[laughter]`, `[pause]`):**
       - Note: Do not confuse normal prose brackets (like `[1]` or `[citation]`) with TTS directives unless they match known directive shapes or the allowlist.
       - For `<phoneme ...>word</phoneme>`: `display` unwraps to `word`.
       - For self-closing/standalone tags (`<break.../>`, `<emotion.../>`, `<speed.../>`, `<laugh>`, `<sigh>`, `[laughs]`, etc.):
         - Always strip from `display` (collapsing extra spaces).
         - Check against effective `passthrough_tags` (case-insensitive prefix/token match, or `"*"` wildcard):
           - If allowed: keep verbatim in `spoken` and push `TtsDirectiveRecord::TagPassthrough { raw }`.
           - If not allowed: strip from `spoken` and push `StrippedDirectiveRecord { raw, reason }`.
     - **Markdown Wrappers & Emojis (when `cfg.sanitize_markdown == true`):**
       - Keep Markdown (`**`, `*`, `__`, `` ` ``, `# `, list bullets) in `display`.
       - Strip Markdown wrappers and emojis (`strip_unspeakable`) from `spoken`.
     - **Pronunciation Lexicon (`cfg.lexicon`):**
       - Apply whole-word replacements from `cfg.lexicon` to `spoken` (skipping regions inside preserved `<...>` tags or `(/ipa/)` phoneme targets).
       - Every match records `TtsDirectiveRecord::Lexicon { from, to }`.
4. **Extend `TextFragment` in `voice-pipes-sdk/src/text.rs`:**
   - Add `pub spoken_text: Option<String>` and `with_spoken_text` setter to `TextFragment`.
   - Add `optional string spoken_text = 6;` to `TextFragment` in `voice-pipes/proto/text_processor.proto` and wire it in `voice-pipes/src/text/grpc.rs` and `voice-pipes/src/grpc/text_server.rs`.

---

### Phase B: Provider Declarations & TOML Configuration

1. **Provider Tag Declarations:**
   - In `voice-pipes-tts-kokoro`: implement `supported_inline_tags(&self)` returning `&["/ipa/"]`.
   - In `voice-pipes-tts-elevenlabs`: implement `supported_inline_tags(&self)` returning `&["<break", "<phoneme", "</phoneme>", "[laughs]", "[whispers]", "[sighs]", "[laughter]"]`.
2. **Pipeline Configuration (`voice-pipes/src/config/types.rs` & `validate.rs`):**
   - Add to `TtsConfig`:
     ```rust
     /// Strip Markdown formatting (**bold**, *italic*, `code`, URLs) from TTS
     /// spoken text while keeping it in display captions/transcripts. Default: true.
     #[serde(default = "default_true")]
     pub sanitize_markdown: bool,

     /// Additional inline TTS instructions/tags (e.g. `["<break", "<laugh>", "[whispers]"]`
     /// or `["*"]`) to preserve in `spoken_text` and strip from `display_text`.
     /// Merged with the active TTS provider's `supported_inline_tags()`.
     #[serde(default)]
     pub passthrough_tags: Vec<String>,

     /// Whole-word pronunciation replacements applied only to `spoken_text`
     /// (e.g. `{"SQL": "sequel", "Myned": "Mined"}`).
     #[serde(default)]
     pub lexicon: std::collections::BTreeMap<String, String>,
     ```
   - Validate in `voice-pipes/src/config/validate.rs` that no key in `tts.lexicon` is empty or whitespace-only, and no entry in `tts.passthrough_tags` is empty.

---

### Phase C: Pipeline Wiring & `AudioLedger` Barge-In Alignment (`voice-pipes`)

1. **Streaming `<think>` Filter in LLM Processors:**
   - Apply `ThinkTagFilter` in `voice-pipes-llm-openai-compat` and `voice-pipes-llm-gguf` token loops before `split_first_sentence` so `<think>...</think>` blocks never trigger premature sentence flushes or leak into fragments.
2. **Dual-Track Routing in `tts_task` (`voice-pipes/src/pipeline/tts.rs`):**
   - Pass `SanitizeConfig` (built from `TtsConfig` + `tts.supported_inline_tags()`) into `tts_task`.
   - For each incoming `TextFragment` (both main loop at line ~590 and prefetch arm at line ~785):
     - Run `let dual = sanitize_dual_track(&raw.text, &sanitize_cfg);` (or use `raw.spoken_text` if pre-populated).
     - If `dual.spoken.trim().is_empty()`, skip synthesis (preserving the `is_end` sentinel handling).
     - Pass `dual.spoken.clone()` to `spawn_synthesis_task(...)`.
     - Populate `CaptionMeta` with `text: dual.display.clone()`, `spoken_text: Some(dual.spoken.clone())`, `tts_directives: dual.directives`, and `stripped_directives: dual.stripped`.
     - In `AuditEvent::TtsFragment`, pass `text: dual.display` and `spoken_text: Some(dual.spoken)` so `{utt:03}_tts_frag_{idx:03}.txt` records the exact text synthesized (or sidecar metadata).
3. **Word-Accurate Barge-In Truncation in `AudioLedger` (`voice-pipes/src/pipeline/types.rs`):**
   - Add `pub spoken_text: String` to `AudioLedgerEntry`.
   - Update `AudioLedger::push(...)` to accept `display_text` and `spoken_text`.
   - Update `AudioLedger::text_at_offset(&self, played_ms: u64) -> Option<String>`:
     - If `played_ms >= entry.cumulative_ms`: append `&entry.text` (`display_text`).
     - If partially played with `entry.word_timings`:
       - Match `w.word` against `entry.spoken_text` (advancing `search_from` past any inline `<...>` tags).
       - Compute the spoken word ratio or word index reached among spoken words, and slice `entry.text` (`display_text`) to the corresponding display word boundary using UTF-8-safe word indexing.
     - If partially played without `word_timings`:
       - Use `proportional_prefix(&entry.text, frac)`.

---

### Phase D: Session Details Debugging UI (`voice-pipes` Audit + `voice-pipes-admin`)

1. **Rust Audit Capture (`voice-pipes/src/audit/streaming.rs` & `orchestrate.rs`):**
   - Aggregate `spoken_text`, `tts_directives`, and `stripped_directives` across the turn's fragments (or compute via `sanitize_dual_track` on the turn output) and write them into `{utt:03}_llm_output.json`:
     ```json
     {
       "utterance_id": 1,
       "text": "Hello! Check your **SQL** database.",
       "spoken_text": "<emotion value=\"positivity:high\"/> Hello! Check your sequel database.",
       "tts_directives": [
         { "kind": "tag_passthrough", "raw": "<emotion value=\"positivity:high\"/>" },
         { "kind": "lexicon", "from": "SQL", "to": "sequel" }
       ],
       "stripped_directives": [
         { "raw": "[whispers]", "reason": "not in passthrough_tags for provider 'kokoro'" }
       ],
       "latency_ms": 210,
       "is_partial": false
     }
     ```
2. **Admin Backend (`voice-pipes-admin/backend/app/captures/reader.py`):**
   - Ensure `_read_capture_artifacts` preserves `spoken_text`, `tts_directives`, and `stripped_directives` inside `utt["llm_output"]`.
3. **Admin Frontend (`voice-pipes-admin/frontend/src/pages/sessions/detail/`):**
   - Update `LlmOutput` interface in `types.ts` to include:
     ```ts
     spoken_text?: string;
     tts_directives?: Array<
       | { kind: 'tag_passthrough'; raw: string }
       | { kind: 'ipa_phoneme'; word: string; ipa: string }
       | { kind: 'lexicon'; from: string; to: string }
     >;
     stripped_directives?: Array<{ raw: string; reason: string }>;
     ```
   - In `TurnCard.tsx` (right next to the collapsible `LLM prompt` button around line 183):
     - When `llm_output.spoken_text` differs from `llm_output.text`, or `tts_directives` / `stripped_directives` is non-empty, render a collapsible violet toggle button:
       - Label: `TTS input · {N} directive(s) · {M} lexicon` (plus an amber `· {K} stripped ⚠` badge when `stripped_directives.length > 0`).
     - When expanded, render a violet-themed monospace inspector box showing:
       - The exact `spoken_text` sent to TTS, with inline tags/lexicon replacements highlighted as distinct chips.
       - A list of `tts_directives` badges (`<break time="300ms"/>`, `Kokoro → /kˈoʊkəɹoʊ/`, `SQL → sequel`).
       - If `stripped_directives` is non-empty, an amber warning row listing each stripped tag and its reason (`[whispers] — not in passthrough_tags for provider 'piper'`).

---

### Phase E: Documentation Updates & Feature Indexing

Update the documentation across the repository so the new capabilities, configurations, and debugging tools are properly documented:

1. **Configuration Reference (`docs/reference/config.md`):**
   - In section `## [tts]`, document the new fields:
     - `sanitize_markdown` (`bool`, default `true`): Strips Markdown formatting (`**bold**`, `*italic*`, `` `code` ``, `#`, `-`, URLs) from spoken audio while preserving them in user display transcripts.
     - `passthrough_tags` (`string[]`, default `[]`): Explicit allowlist of inline directives/tags (e.g. `["<break", "<emotion", "[whispers]"]` or `["*"]`) to preserve in `spoken_text` and strip from `display_text`.
     - `[tts.lexicon]` (table / key-value map, default `{}`): Whole-word pronunciation substitutions applied only to `spoken_text` (e.g. `"SQL" = "sequel"`, `"Myned" = "Mined"`).
2. **Configuration Guide (`docs/guides/configuration.md`):**
   - Add a dedicated subsection **"Pronunciation lexicon and Markdown sanitization"** explaining:
     - How `[tts.lexicon]` works and why it prevents mispronunciation without corrupting LLM history.
     - How `passthrough_tags` allows passing SSML or emotion tags to supported providers without breaking fallback TTS engines.
3. **Session Details Specification (`docs/explanation/session-history.md`):**
   - In **Section 4 ("Conversation View")** or **Section 5 ("Developer Artifacts")**, document the new **"TTS Input Inspector"** component:
     - When it appears (turns where `spoken_text` differs from `display_text` or directives exist).
     - How directive chips and warning badges for stripped directives are rendered.
4. **Provider-Specific TTS Guides (`docs/guides/providers/tts/`):**
   - `kokoro.md`: Document native support for `[word](/ipa/)` phoneme notation.
   - `elevenlabs.md`: Document supported inline tags (`<break time="..."/>`, `<phoneme>`, `[laughs]`, `[whispers]`, etc.) and how they flow through `passthrough_tags`.
   - `piper.md`: Document that unsupported tags and Markdown are automatically stripped cleanly so Piper never speaks raw code aloud.
5. **Feature Examples (`examples/features/` & `docs/guides/examples-and-features.md`):**
   - Add a ready-to-run example `examples/features/tts-lexicon-and-tags.toml` showing `[tts.lexicon]` and `passthrough_tags` in action.
   - Index it in `examples/features/README.md` and `docs/guides/examples-and-features.md`.
6. **Reference Regeneration:**
   - Run `python scripts/gen-reference.py` if metric or reference markers are updated.

---

## 4. Verification & Test Plan

### 4.1 Rust Unit Tests (`voice-pipes-sdk`)
- `voice-pipes-sdk/src/tests/sanitize.rs`:
  1. `think_tag_filter_strips_across_chunk_boundaries`: feed `"<th"`, `"ink>secret reasoning</thi"`, `"nk>Hello!"` $\rightarrow$ outputs `"Hello!"`.
  2. `markdown_stripped_from_spoken_preserved_in_display`: `"Check **SQL** and *logs* at [Docs](https://example.com) 🎉"` $\rightarrow$ `display = "Check **SQL** and *logs* at [Docs](https://example.com) 🎉"`, `spoken = "Check sequel and logs at Docs"`.
  3. `kokoro_ipa_link_preserved_for_kokoro_stripped_for_piper`: `"[Kokoro](/kˈoʊkəɹoʊ/) is fast."` $\rightarrow$ `display = "Kokoro is fast."`; with `"/ipa/"` allowed, `spoken = "[Kokoro](/kˈoʊkəɹoʊ/) is fast."` and `directives` contains `IpaPhoneme`; without `"/ipa/"`, `spoken = "Kokoro is fast."` and `stripped` records it.
  4. `passthrough_tags_preserved_in_spoken_stripped_in_display`: `"<emotion value=\"happy\"/> Hi! <break time=\"500ms\"/> [laughs]"` with `passthrough_tags = ["<emotion", "<break", "[laughs]"]` $\rightarrow$ `display = "Hi!"`, `spoken = "<emotion value=\"happy\"/> Hi! <break time=\"500ms\"/> [laughs]"`, 3 `TagPassthrough` records.
  5. `unsupported_tags_stripped_from_both_and_recorded`: `"[whispers] Secret <break time=\"1s\"/>"` with empty allowlist $\rightarrow$ `display = "Secret"`, `spoken = "Secret"`, 2 `StrippedDirectiveRecord` items.
  6. `lexicon_respects_word_boundaries_and_skips_tags`: `"SQL and MySQL in <break time=\"SQL\"/>"` with `"SQL" -> "sequel"` $\rightarrow$ replaces standalone `"SQL"`, leaves `"MySQL"` and tag attributes untouched.
- `voice-pipes-sdk/src/tests/sentence.rs`:
  7. `sentence_splitter_does_not_split_inside_tts_tags_or_ipa`: `"Wait <break time=\"1.5s\"/> here. Next."` splits only after `"here."`, never on `"1.5s"`.

### 4.2 Rust Pipeline & Barge-In Ledger Tests (`voice-pipes`)
- `voice-pipes/src/pipeline/tests/suite/tts.rs`:
  1. `dual_track_barge_in_with_lexicon_and_word_timings`: `display = "Run **SQL** queries now."`, `spoken = "Run sequel queries now."`, TTS reports `WordBoundary` for `"Run"` (0..200ms) and `"sequel"` (200..500ms). `ledger.text_at_offset(350)` returns `"Run **SQL**"`.
  2. `dual_track_barge_in_with_passthrough_tags_and_word_timings`: `display = "Hello world today."`, `spoken = "<break time=\"200ms\"/> Hello world today."`, TTS reports `WordBoundary` for `"Hello"` (200..400ms). `ledger.text_at_offset(300)` returns `"Hello"`.

### 4.3 Admin Backend & Frontend Tests (`voice-pipes-admin`)
- `voice-pipes-admin/backend/app/test_captures.py`:
  1. Verify `{utt:03}_llm_output.json` with `spoken_text`, `tts_directives`, and `stripped_directives` is exposed on the session detail API response.
- `voice-pipes-admin/frontend/src/pages/sessions/detail/TurnCard.test.tsx`:
  1. Renders `TTS input · 2 directives · 1 lexicon` button when `tts_directives` is present; expands on click to show `spoken_text` and directive chips.
  2. Renders amber warning badge when `stripped_directives` is non-empty and displays the stripped tag reason when expanded.
  3. Hides `TTS input` toggle when `spoken_text` is absent/identical and no directives exist.

### 4.4 Mandatory Workspace Gates
```sh
cargo check --workspace
cargo clippy --workspace -- -D warnings
cargo test --workspace
cargo test -p voice-pipes-cli --test workspace_rules
cd voice-pipes-admin/backend && $env:PYTHONPATH="."; py -3.12 -m unittest discover -s app -p "test_*.py" -t .
cd voice-pipes-admin/frontend && npm run lint && npx tsc -b && npm test
```
