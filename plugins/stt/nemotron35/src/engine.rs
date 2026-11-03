//! The streaming engine: encoder cache, chunk schedule, RNNT greedy decode.
//!
//! One `Engine` is one live utterance stream on one set of ONNX sessions.
//! It follows the reference engine's schedule exactly, because the encoder's
//! cache-aware attention was trained on it: a first chunk of `1 + 8·L` mel
//! frames through the first-chunk graph, then chunks of `8·(L+1)` frames
//! through the steady graph, where `L` is the lookahead the chunk size was
//! exported with (3 at 320 ms). Each chunk yields `L + 1` encoder frames,
//! which the greedy loop consumes one at a time.
//!
//! The 75 cache tensors are carried between chunks by name. `cache_mask` is
//! an additive attention mask over the 56 left-context slots plus this
//! chunk's frames: slots the cache has not yet filled hold `-1e9`, so the
//! first chunks do not attend to zeroed history. The mask is built from a
//! count of valid frames the engine keeps, and nothing else reads that count.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use ort::session::Session;
use ort::value::{DynValue, Tensor};

use crate::features::{HOP_LENGTH, LogMelExtractor, N_FFT, N_MELS, WIN_LENGTH};
use crate::model::ModelConfig;

/// How an engine reaches its four sessions: a borrowed set in tests, an
/// owned pool slot in the provider, where the engine has to move into a
/// blocking task and back and so cannot hold a borrow.
pub trait SessionsAccess {
    fn sessions(&mut self) -> &mut Sessions;
}

impl SessionsAccess for &mut Sessions {
    fn sessions(&mut self) -> &mut Sessions {
        self
    }
}

impl SessionsAccess for Sessions {
    fn sessions(&mut self) -> &mut Sessions {
        self
    }
}

const SUBSAMPLING: usize = 8;
const LEFT_CONTEXT: usize = 56;
const MASK_INVALID: f32 = -1e9;

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("{stage}: {message}")]
    Ort {
        stage: &'static str,
        message: String,
    },
    #[error("the graph has no output named {0}")]
    MissingOutput(String),
    #[error("{0} holds fewer values than its shape says")]
    ShortOutput(String),
}

fn ort_err<E: std::fmt::Display>(stage: &'static str) -> impl Fn(E) -> EngineError {
    move |e| EngineError::Ort {
        stage,
        message: e.to_string(),
    }
}

/// The four sessions of one model directory, opened once and lent to
/// streams by the provider's pool.
pub struct Sessions {
    pub encoder_first: Session,
    pub encoder_steady: Session,
    pub decoder: Session,
    pub joiner: Session,
}

impl std::fmt::Debug for Sessions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Sessions { encoder_first, encoder_steady, decoder, joiner }")
    }
}

/// Build one session on `threads` threads, where `VP_ORT_EP` says (CUDA
/// when the build has `ort-cuda`).
pub fn open_session(path: &Path, threads: usize) -> Result<Session, EngineError> {
    let options = voice_pipes_onnx::SessionOptions::new(threads)
        .with_target(voice_pipes_onnx::ExecutionTarget::from_env());
    voice_pipes_onnx::open_session(path, &options).map_err(ort_err("opening the session"))
}

impl Sessions {
    pub fn open(model_dir: &Path, cfg: &ModelConfig, threads: usize) -> Result<Self, EngineError> {
        let (first, steady) = cfg.encoder_files();
        Ok(Self {
            encoder_first: open_session(&model_dir.join(first), threads)?,
            encoder_steady: open_session(&model_dir.join(steady), threads)?,
            decoder: open_session(&model_dir.join("decoder.onnx"), threads)?,
            joiner: open_session(&model_dir.join("joiner.onnx"), threads)?,
        })
    }
}

/// Sizes derived from the chunk's lookahead, in the reference's terms.
#[derive(Debug, Clone, Copy)]
pub struct Schedule {
    pub lookahead: usize,
    /// Encoder frames per chunk.
    pub enc_frames: usize,
    pub mel_first: usize,
    pub mel_steady: usize,
    /// Samples the first chunk's features are cut from (centred extraction).
    pub samples_first: usize,
    /// Samples a steady window holds: its frames plus the window's reach.
    pub samples_steady: usize,
}

impl Schedule {
    pub fn for_lookahead(lookahead: usize) -> Self {
        let mel_first = 1 + SUBSAMPLING * lookahead;
        let mel_steady = SUBSAMPLING * (lookahead + 1);
        Self {
            lookahead,
            enc_frames: lookahead + 1,
            mel_first,
            mel_steady,
            samples_first: (mel_first - 1) * HOP_LENGTH + WIN_LENGTH / 2,
            samples_steady: mel_steady * HOP_LENGTH + WIN_LENGTH,
        }
    }
}

// Sessions and cache tensors carry nothing a log line could use; the
// decoding settings do.
impl<S: SessionsAccess> std::fmt::Debug for Engine<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("blank_id", &self.blank_id)
            .field("max_symbols", &self.max_symbols)
            .field("prompt_id", &self.prompt_id)
            .field("cached_tensors", &self.caches.len())
            .finish_non_exhaustive()
    }
}

pub struct Engine<S: SessionsAccess> {
    sessions: S,
    schedule: Schedule,
    extractor: LogMelExtractor,
    blank_id: i64,
    max_symbols: usize,
    prompt_id: i64,
    /// Cache tensors by input name, shape and data, carried between chunks.
    caches: HashMap<String, (Vec<i64>, Vec<f32>)>,
    /// Left-context slots the cache has filled so far, at most `LEFT_CONTEXT`.
    cache_valid: usize,
    dec_h: Vec<f32>,
    dec_c: Vec<f32>,
    dec_out: Vec<f32>,
    /// Audio not yet consumed, and the absolute index of its first sample.
    pcm: Vec<f32>,
    pcm_start: usize,
    /// Next mel frame index to produce, in whole-utterance terms.
    mel_idx: usize,
    started: bool,
    /// Speech tokens, in order. Locale tags are not among them.
    tokens: Vec<u32>,
    /// Token ids that are locale tags such as `<hr-HR>`, which the model
    /// emits as its belief about the language and which are not speech.
    locale_tags: HashSet<u32>,
    /// Encoder chunks run so far, for the inference count.
    chunks_run: u32,
    /// Locale tags emitted, in order. Under `auto` the first is the model's
    /// detection; a later one is it changing its mind mid-utterance, which
    /// is what precedes a run of the wrong script.
    tags_seen: Vec<u32>,
}

impl<S: SessionsAccess> Engine<S> {
    pub fn new(
        sessions: S,
        cfg: &ModelConfig,
        prompt_id: i64,
        locale_tags: HashSet<u32>,
    ) -> Result<Self, EngineError> {
        let hidden = cfg.decoder_hidden_size;
        let layers = cfg.num_decoder_layers;
        let mut e = Self {
            sessions,
            schedule: Schedule::for_lookahead(cfg.lookahead()),
            extractor: LogMelExtractor::new(),
            blank_id: cfg.blank_token_id,
            max_symbols: cfg.max_symbols_per_step,
            prompt_id,
            caches: cfg
                .cache_shapes
                .iter()
                .map(|(n, s)| {
                    // A shape that does not fit a length leaves the cache
                    // empty, and the run refuses it as a shape mismatch.
                    let len = s
                        .iter()
                        .try_fold(1usize, |acc, d| {
                            usize::try_from(*d).ok().and_then(|d| acc.checked_mul(d))
                        })
                        .unwrap_or(0);
                    (n.clone(), (s.clone(), vec![0f32; len]))
                })
                .collect(),
            cache_valid: 0,
            dec_h: vec![0f32; layers * hidden],
            dec_c: vec![0f32; layers * hidden],
            dec_out: Vec::new(),
            pcm: Vec::new(),
            pcm_start: 0,
            mel_idx: 0,
            started: false,
            tokens: Vec::new(),
            locale_tags,
            chunks_run: 0,
            tags_seen: Vec::new(),
        };
        // The prediction network is primed with the blank, as at training.
        e.dec_out = e.run_decoder(e.blank_id)?;
        Ok(e)
    }

    pub fn schedule(&self) -> Schedule {
        self.schedule
    }

    /// Speech tokens emitted so far, in order.
    pub fn tokens(&self) -> &[u32] {
        &self.tokens
    }

    /// Locale-tag tokens emitted so far, in order.
    pub fn locale_tags_seen(&self) -> &[u32] {
        &self.tags_seen
    }

    /// Encoder chunks run so far.
    pub fn chunks_run(&self) -> u32 {
        self.chunks_run
    }

    /// Give the sessions back, ending this stream.
    pub fn into_sessions(self) -> S {
        self.sessions
    }

    // ── decoder / joiner ────────────────────────────────────

    fn run_decoder(&mut self, token: i64) -> Result<Vec<f32>, EngineError> {
        let layers_hidden = self.dec_h.len();
        let hidden = layers_hidden / 2;
        let tok =
            Tensor::from_array(([1usize, 1], vec![token])).map_err(ort_err("token tensor"))?;
        let h = Tensor::from_array(([2usize, 1, hidden], std::mem::take(&mut self.dec_h)))
            .map_err(ort_err("h tensor"))?;
        let c = Tensor::from_array(([2usize, 1, hidden], std::mem::take(&mut self.dec_c)))
            .map_err(ort_err("c tensor"))?;
        let outputs = self
            .sessions
            .sessions()
            .decoder
            .run(ort::inputs!["token" => tok, "h_in" => h, "c_in" => c])
            .map_err(ort_err("decoder run"))?;
        let take = |name: &str| -> Result<Vec<f32>, EngineError> {
            let (_, data) = outputs
                .get(name)
                .ok_or_else(|| EngineError::MissingOutput(name.into()))?
                .try_extract_tensor::<f32>()
                .map_err(ort_err("extract decoder output"))?;
            Ok(data.to_vec())
        };
        self.dec_h = take("h_out")?;
        self.dec_c = take("c_out")?;
        take("decoder_out")
    }

    /// Greedy RNNT over `frames` encoder frames: advance on blank; on a
    /// non-blank emit it and step the prediction network; force the advance
    /// after `max_symbols` emissions on one frame.
    fn greedy_decode(
        &mut self,
        enc_out: &[f32],
        hidden: usize,
        frames: usize,
    ) -> Result<(), EngineError> {
        for t in 0..frames {
            let frame = enc_out
                .get(t * hidden..(t + 1) * hidden)
                .ok_or_else(|| EngineError::ShortOutput("encoder_out".into()))?;
            let mut symbols = 0;
            loop {
                let enc = Tensor::from_array(([1usize, hidden], frame.to_vec()))
                    .map_err(ort_err("encoder frame tensor"))?;
                let dec = Tensor::from_array(([1usize, hidden], self.dec_out.clone()))
                    .map_err(ort_err("decoder out tensor"))?;
                let outputs = self
                    .sessions
                    .sessions()
                    .joiner
                    .run(ort::inputs!["encoder_frame" => enc, "decoder_out" => dec])
                    .map_err(ort_err("joiner run"))?;
                let (_, logits) = outputs
                    .get("logits")
                    .ok_or_else(|| EngineError::MissingOutput("logits".into()))?
                    .try_extract_tensor::<f32>()
                    .map_err(ort_err("extract logits"))?;
                // Empty (or all-NaN) logits are a broken joiner output, not
                // a blank: decoding them as one would hide the fault.
                let token = voice_pipes_onnx::argmax(logits)
                    .and_then(|i| i64::try_from(i).ok())
                    .ok_or_else(|| EngineError::ShortOutput("logits".into()))?;
                drop(outputs);
                if token == self.blank_id {
                    break;
                }
                // A tag still steps the prediction network -- the model
                // conditions what follows on it -- but is not speech.
                let id = u32::try_from(token)
                    .map_err(|_out_of_range| EngineError::ShortOutput("logits".into()))?;
                if self.locale_tags.contains(&id) {
                    self.tags_seen.push(id);
                } else {
                    self.tokens.push(id);
                }
                self.dec_out = self.run_decoder(token)?;
                symbols += 1;
                if symbols >= self.max_symbols {
                    break;
                }
            }
        }
        Ok(())
    }

    // ── encoder ─────────────────────────────────────────────

    /// Run one chunk. `features` holds exactly the schedule's mel frames for
    /// the graph chosen by `first`. Returns the encoder output and its
    /// hidden size.
    fn run_encoder(
        &mut self,
        features: Vec<f32>,
        first: bool,
    ) -> Result<(Vec<f32>, usize), EngineError> {
        let mel_frames = if first {
            self.schedule.mel_first
        } else {
            self.schedule.mel_steady
        };
        debug_assert_eq!(features.len(), mel_frames * N_MELS);
        let mask_len = LEFT_CONTEXT + self.schedule.enc_frames;
        let mut mask = vec![0f32; mask_len];
        let invalid = LEFT_CONTEXT.saturating_sub(self.cache_valid);
        for m in mask.iter_mut().take(invalid) {
            *m = MASK_INVALID;
        }
        let mut named: Vec<(String, DynValue)> = Vec::with_capacity(3 + self.caches.len());
        named.push((
            "input_features".into(),
            Tensor::from_array(([1usize, mel_frames, N_MELS], features))
                .map_err(ort_err("features tensor"))?
                .into_dyn(),
        ));
        named.push((
            "prompt_ids".into(),
            Tensor::from_array(([1usize], vec![self.prompt_id]))
                .map_err(ort_err("prompt tensor"))?
                .into_dyn(),
        ));
        named.push((
            "cache_mask".into(),
            Tensor::from_array(([1usize, 1, 1, mask_len], mask))
                .map_err(ort_err("mask tensor"))?
                .into_dyn(),
        ));
        // Caches are moved into the run: the graph returns a replacement for
        // each, and a copy would only be dropped.
        let mut names: Vec<String> = self.caches.keys().cloned().collect();
        names.sort();
        for name in &names {
            let Some((shape, data)) = self.caches.get_mut(name) else {
                continue;
            };
            let t = Tensor::from_array((shape.as_slice(), std::mem::take(data)))
                .map_err(ort_err("cache tensor"))?;
            named.push((name.clone(), t.into_dyn()));
        }
        let sessions = self.sessions.sessions();
        let session = if first {
            &mut sessions.encoder_first
        } else {
            &mut sessions.encoder_steady
        };
        let outputs = session.run(named).map_err(ort_err("encoder run"))?;
        let (shape, data) = outputs
            .get("encoder_out")
            .ok_or_else(|| EngineError::MissingOutput("encoder_out".into()))?
            .try_extract_tensor::<f32>()
            .map_err(ort_err("extract encoder_out"))?;
        let hidden = usize::try_from(*shape.last().unwrap_or(&0)).unwrap_or(0);
        let enc_out = data.to_vec();
        for name in &names {
            let out_name = cache_output_name(name);
            let (shape, data) = outputs
                .get(out_name.as_str())
                .ok_or_else(|| EngineError::MissingOutput(out_name.clone()))?
                .try_extract_tensor::<f32>()
                .map_err(ort_err("extract cache"))?;
            self.caches
                .insert(name.clone(), (shape.to_vec(), data.to_vec()));
        }
        self.cache_valid = (self.cache_valid + self.schedule.enc_frames).min(LEFT_CONTEXT);
        self.chunks_run += 1;
        Ok((enc_out, hidden))
    }

    /// Pad or cut a feature block to exactly `frames` rows.
    fn fit(mut feats: Vec<f32>, frames: usize) -> Vec<f32> {
        feats.resize(frames * N_MELS, 0.0);
        feats
    }

    fn produce_first(
        &mut self,
        decode_frames: Option<usize>,
        valid_mel: Option<usize>,
    ) -> Result<(), EngineError> {
        let take = self.pcm.len().min(self.schedule.samples_first);
        let (_, mut feats) = self.extractor.extract(self.pcm.split_at(take).0, true);
        if let Some(v) = valid_mel
            && v * N_MELS < feats.len()
        {
            feats.truncate(v * N_MELS);
        }
        let feats = Self::fit(feats, self.schedule.mel_first);
        let (enc_out, hidden) = self.run_encoder(feats, true)?;
        self.started = true;
        self.mel_idx = self.schedule.mel_first;
        let frames = decode_frames.unwrap_or(self.schedule.enc_frames);
        if frames > 0 {
            self.greedy_decode(&enc_out, hidden, frames)?;
        }
        self.discard_consumed();
        Ok(())
    }

    fn produce_steady(
        &mut self,
        window: &[f32],
        valid_mel: Option<usize>,
    ) -> Result<(), EngineError> {
        let (_, mut feats) = self.extractor.extract(window, false);
        if let Some(v) = valid_mel
            && v * N_MELS < feats.len()
        {
            feats.truncate(v * N_MELS);
        }
        let feats = Self::fit(feats, self.schedule.mel_steady);
        let (enc_out, hidden) = self.run_encoder(feats, false)?;
        let frames = match valid_mel {
            None => self.schedule.enc_frames,
            Some(v) => self.schedule.enc_frames.min(v.div_ceil(SUBSAMPLING).max(1)),
        };
        self.mel_idx += self.schedule.mel_steady;
        self.greedy_decode(&enc_out, hidden, frames)?;
        self.discard_consumed();
        Ok(())
    }

    fn discard_consumed(&mut self) {
        let next_start = (self.mel_idx * HOP_LENGTH).saturating_sub(N_FFT / 2);
        let drop = next_start.saturating_sub(self.pcm_start);
        if drop > 0 {
            let drop = drop.min(self.pcm.len());
            self.pcm.drain(..drop);
            self.pcm_start += drop;
        }
    }

    // ── public ──────────────────────────────────────────────

    /// Feed 16 kHz mono samples in `[-1, 1]`; every complete chunk they
    /// allow is run.
    pub fn accept(&mut self, samples: &[f32]) -> Result<(), EngineError> {
        self.pcm.extend_from_slice(samples);
        let pcm_end = self.pcm_start + self.pcm.len();
        if !self.started {
            if self.pcm.len() < self.schedule.samples_first {
                return Ok(());
            }
            self.produce_first(None, None)?;
        }
        loop {
            let start = (self.mel_idx * HOP_LENGTH).saturating_sub(N_FFT / 2);
            if start + self.schedule.samples_steady > pcm_end {
                break;
            }
            let offset = start - self.pcm_start;
            let window = self
                .pcm
                .get(offset..offset + self.schedule.samples_steady)
                .ok_or_else(|| EngineError::ShortOutput("buffered audio".into()))?
                .to_vec();
            self.produce_steady(&window, None)?;
        }
        Ok(())
    }

    /// Close an utterance without closing the stream: silence is appended
    /// up to the next chunk boundary and run, so every token the utterance
    /// can still produce is produced now, and the tokens and locale tags
    /// emitted so far are handed back and cleared. The encoder cache, the
    /// prediction network's state and the audio position all carry on,
    /// which is what a cache-aware streaming model expects: the next
    /// utterance is decoded with this one as its acoustic context, the
    /// same as when the whole conversation is decoded in one pass. A fresh
    /// engine per utterance decodes each one from nothing, and a segment
    /// cut by a VAD right at speech onset can then decode to nothing at
    /// all.
    pub fn end_utterance(&mut self) -> Result<(Vec<u32>, Vec<u32>, usize), EngineError> {
        let pcm_end = self.pcm_start + self.pcm.len();
        let target = if self.started {
            (self.mel_idx * HOP_LENGTH).saturating_sub(N_FFT / 2) + self.schedule.samples_steady
        } else {
            self.schedule.samples_first
        };
        let mut padded = 0;
        if target > pcm_end {
            padded = target - pcm_end;
            self.accept(&vec![0.0; padded])?;
        }
        Ok((
            std::mem::take(&mut self.tokens),
            std::mem::take(&mut self.tags_seen),
            padded,
        ))
    }

    /// Run the audio tail as a final, zero-padded chunk. Call once, at the
    /// end of the stream. Returns the number of padding samples added.
    pub fn finish(&mut self) -> Result<usize, EngineError> {
        let pcm_end = self.pcm_start + self.pcm.len();
        let mut padded = 0;
        if !self.started {
            if pcm_end == 0 {
                return Ok(0);
            }
            let real = self.pcm.len();
            if real < self.schedule.samples_first {
                padded = self.schedule.samples_first - real;
            }
            self.pcm.resize(self.schedule.samples_first, 0.0);
            let valid_mel = (real / HOP_LENGTH).min(self.schedule.mel_first);
            let frames = if valid_mel > 0 {
                Some(
                    self.schedule
                        .enc_frames
                        .min(valid_mel.div_ceil(SUBSAMPLING).max(1)),
                )
            } else {
                Some(0)
            };
            self.produce_first(frames, Some(valid_mel))?;
            return Ok(padded);
        }
        let start = (self.mel_idx * HOP_LENGTH).saturating_sub(N_FFT / 2);
        let real = pcm_end.saturating_sub(start.max(self.pcm_start));
        let valid_mel =
            (real.saturating_sub(N_FFT) / HOP_LENGTH).saturating_add(usize::from(real >= N_FFT));
        if valid_mel > 0 {
            if real < self.schedule.samples_steady {
                padded = self.schedule.samples_steady - real;
            }
            let offset = start.saturating_sub(self.pcm_start);
            let mut window = self
                .pcm
                .get(offset..)
                .map(<[f32]>::to_vec)
                .unwrap_or_default();
            window.resize(self.schedule.samples_steady, 0.0);
            self.produce_steady(&window, Some(valid_mel))?;
        }
        Ok(padded)
    }
}

/// `k_cache_3` → `k_cache_out_3`, `cache_mask` is not a carried cache.
fn cache_output_name(input: &str) -> String {
    match input.rfind('_') {
        Some(i) if input[i + 1..].chars().all(|c| c.is_ascii_digit()) => {
            format!("{}_out_{}", &input[..i], &input[i + 1..])
        }
        _ => format!("{input}_out"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_names_follow_the_export_convention() {
        assert_eq!(cache_output_name("k_cache_0"), "k_cache_out_0");
        assert_eq!(cache_output_name("conv1d_cache_23"), "conv1d_cache_out_23");
        assert_eq!(cache_output_name("conv2d_cache_2"), "conv2d_cache_out_2");
    }

    #[test]
    fn the_320ms_schedule_is_the_one_wp_a_measured() {
        let s = Schedule::for_lookahead(3);
        assert_eq!((s.enc_frames, s.mel_first, s.mel_steady), (4, 25, 32));
        assert_eq!(s.samples_first, 24 * 160 + 200);
        assert_eq!(s.samples_steady, 32 * 160 + 400);
    }
}
