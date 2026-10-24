//! Minimal streaming-WER harness for the Nemotron provider.
//!
//! This is the gate for the `MatMulNBits` self-quant work.
//! Point it at a model directory and a labeled corpus, get back WER / CER / `RTFx` for both the
//! batch and the true cache-aware streaming paths, plus the batch-to-stream
//! factor (BSF). Run it once on the FP32 encoder to establish a baseline,
//! then again on `encoder.int4.onnx` to prove the quant didn't regress.
//!
//! Build / usage:
//!
//! ```text
//! cargo build --release -p voice-pipes-stt-nemotron --example wer_eval
//! cargo run --release -p voice-pipes-stt-nemotron --example wer_eval -- \
//!     <model_dir> <manifest.tsv> [out.json]
//! ```
//!
//! Manifest format (one clip per line, TAB-separated):
//!
//! ```text
//! path/to/clip.wav<TAB>the reference transcript in plain words
//! ```
//!
//! Lines starting with `#` and blank lines are ignored. Relative WAV paths
//! are resolved against the manifest's own directory.
//!
//! WER is micro-averaged over the corpus (sum of word edits / sum of
//! reference words), which is the standard ASR aggregate — not the mean of
//! per-clip rates. Both sides are normalized (lowercased, punctuation
//! stripped, whitespace collapsed) before scoring so capitalization and
//! trailing periods don't count as errors.

use std::path::{Path, PathBuf};
use std::time::Instant;

use voice_pipes_sdk::AudioFrame;
use voice_pipes_sdk::audio::resample::resample_f32;
use voice_pipes_sdk::stt::{AudioChunk, RecognitionRequest, SttProvider};
use voice_pipes_stt_nemotron::NemotronStt;

const TARGET_SR: u32 = 16_000;

// ── WER / CER scoring ────────────────────────────────────────

/// Normalize text for WER comparison: lowercase, drop punctuation
/// (keeping intra-word apostrophes), collapse whitespace into single
/// tokens. e.g. `"Hello, world."` -> `["hello", "world"]`.
fn normalize_words(text: &str) -> Vec<String> {
    let cleaned: String = text
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '\'' {
                c.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect();
    cleaned
        .split_whitespace()
        .map(ToString::to_string)
        .collect()
}

fn normalize_chars(text: &str) -> Vec<char> {
    let cleaned: String = text
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '\'' || c.is_whitespace() {
                c.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect();
    // Collapse runs of whitespace so spacing differences don't dominate CER.
    cleaned
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .collect()
}

/// Levenshtein edit distance over a token slice (two-row DP, O(min) memory).
fn edit_distance<T: Eq>(reference: &[T], hypothesis: &[T]) -> usize {
    let n = reference.len();
    let m = hypothesis.len();
    if n == 0 {
        return m;
    }
    if m == 0 {
        return n;
    }
    let mut prev: Vec<usize> = (0..=m).collect();
    let mut curr = vec![0usize; m + 1];
    for i in 1..=n {
        curr[0] = i;
        for j in 1..=m {
            let cost = usize::from(reference[i - 1] != hypothesis[j - 1]);
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[m]
}

/// (`word_edits`, `reference_word_count`) — kept raw so the corpus aggregate
/// can be micro-averaged rather than averaging per-clip rates.
fn word_counts(reference: &str, hypothesis: &str) -> (usize, usize) {
    let r = normalize_words(reference);
    let h = normalize_words(hypothesis);
    (edit_distance(&r, &h), r.len())
}

fn char_counts(reference: &str, hypothesis: &str) -> (usize, usize) {
    let r = normalize_chars(reference);
    let h = normalize_chars(hypothesis);
    (edit_distance(&r, &h), r.len())
}

fn rate(edits: usize, refs: usize) -> f64 {
    if refs == 0 {
        if edits == 0 { 0.0 } else { 1.0 }
    } else {
        edits as f64 / refs as f64
    }
}

// ── Audio loading ────────────────────────────────────────────

/// Load a WAV as 16 kHz mono i16, resampling/downmixing as needed.
fn load_wav_16k_mono(path: &Path) -> Result<AudioFrame, Box<dyn std::error::Error>> {
    let mut reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    let raw: Vec<f32> = match (spec.sample_format, spec.bits_per_sample) {
        (hound::SampleFormat::Int, 16) => reader
            .samples::<i16>()
            .map(|s| s.map(|v| v as f32 / 32768.0))
            .collect::<Result<_, _>>()?,
        (hound::SampleFormat::Float, 32) => reader.samples::<f32>().collect::<Result<_, _>>()?,
        other => {
            return Err(format!("unsupported WAV format {other:?} in {}", path.display()).into());
        }
    };

    let ch = spec.channels.max(1) as usize;
    let mono: Vec<f32> = if ch == 1 {
        raw
    } else {
        raw.chunks(ch)
            .map(|f| f.iter().sum::<f32>() / ch as f32)
            .collect()
    };

    let resampled = if spec.sample_rate == TARGET_SR {
        mono
    } else {
        resample_f32(&mono, spec.sample_rate, TARGET_SR)?
    };

    let samples: Vec<i16> = resampled
        .into_iter()
        .map(|s| (s.clamp(-1.0, 1.0) * 32767.0) as i16)
        .collect();
    Ok(AudioFrame {
        samples,
        sample_rate: TARGET_SR,
        channels: 1,
    })
}

// ── Streaming driver ─────────────────────────────────────────

/// Drive `transcribe_stream` like the live pipeline: 100 ms chunks, with
/// 500 ms of trailing silence appended to the final chunk (the pipeline's
/// "flush trick"). Returns the final transcript.
async fn run_streaming(stt: &NemotronStt, frame: &AudioFrame) -> String {
    let sr = TARGET_SR as usize;
    let chunk_size = sr / 10; // 100 ms
    let samples = frame.samples.clone();

    let (chunk_tx, chunk_rx) = tokio::sync::mpsc::channel::<AudioChunk>(64);
    let (result_tx, mut result_rx) = tokio::sync::mpsc::channel(16);
    let request = RecognitionRequest::default().with_sample_rate(TARGET_SR);

    let producer = tokio::spawn(async move {
        let total = samples.len();
        let mut pos = 0;
        while pos < total {
            let end = (pos + chunk_size).min(total);
            let is_last = end == total;
            let mut chunk_samples = samples[pos..end].to_vec();
            if is_last {
                chunk_samples.extend(std::iter::repeat_n(0i16, sr / 2));
            }
            if chunk_tx
                .send(AudioChunk {
                    samples: chunk_samples,
                    is_last,
                })
                .await
                .is_err()
            {
                break;
            }
            pos = end;
        }
        drop(chunk_tx);
    });

    let stream = stt.transcribe_stream(
        request,
        chunk_rx,
        result_tx,
        tokio_util::sync::CancellationToken::new(),
    );

    let collector = async {
        let mut final_text = String::new();
        while let Some(r) = result_rx.recv().await {
            if r.is_final {
                final_text = r.text;
            }
        }
        final_text
    };

    let (stream_res, _p, final_text) = tokio::join!(stream, producer, collector);
    if let Err(e) = stream_res {
        eprintln!("  [warn] streaming failed: {e}");
    }
    final_text
}

// ── Per-clip result ──────────────────────────────────────────

struct ClipResult {
    name: String,
    audio_secs: f64,
    batch_text: String,
    stream_text: String,
    batch_word_edits: usize,
    batch_char_edits: usize,
    stream_word_edits: usize,
    stream_char_edits: usize,
    ref_words: usize,
    ref_chars: usize,
    batch_rtfx: f64,
    stream_rtfx: f64,
}

fn parse_manifest(path: &Path) -> Result<Vec<(PathBuf, String)>, Box<dyn std::error::Error>> {
    let base = path.parent().unwrap_or_else(|| Path::new("."));
    let text = std::fs::read_to_string(path)?;
    let mut out = Vec::new();
    for (lineno, line) in text.lines().enumerate() {
        let line = line.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (wav, reference) = line.split_once('\t').ok_or_else(|| {
            format!(
                "manifest line {} missing TAB separator: {line:?}",
                lineno + 1
            )
        })?;
        let wav = wav.trim();
        let reference = reference.trim().to_string();
        let wav_path = {
            let p = Path::new(wav);
            if p.is_absolute() {
                p.to_path_buf()
            } else {
                base.join(p)
            }
        };
        out.push((wav_path, reference));
    }
    Ok(out)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 || args.len() > 4 {
        eprintln!("usage: wer_eval <model_dir> <manifest.tsv> [out.json]");
        std::process::exit(2);
    }
    let model_dir = PathBuf::from(&args[1]);
    let manifest = PathBuf::from(&args[2]);
    let json_out = args.get(3).map(PathBuf::from);

    let clips = parse_manifest(&manifest)?;
    if clips.is_empty() {
        return Err("manifest contained no clips".into());
    }
    eprintln!("=== wer_eval ===");
    eprintln!("  model_dir = {}", model_dir.display());
    eprintln!(
        "  manifest  = {} ({} clips)",
        manifest.display(),
        clips.len()
    );

    let stt = NemotronStt::new(&model_dir)?;

    let mut results: Vec<ClipResult> = Vec::with_capacity(clips.len());
    for (wav_path, reference) in &clips {
        let name = wav_path.file_name().map_or_else(
            || wav_path.display().to_string(),
            |s| s.to_string_lossy().to_string(),
        );
        let frame = match load_wav_16k_mono(wav_path) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("  [skip] {name}: {e}");
                continue;
            }
        };
        let audio_secs = frame.samples.len() as f64 / TARGET_SR as f64;

        // Batch path.
        let t0 = Instant::now();
        let batch_text = stt.transcribe(&frame).await?.text;
        let batch_wall = t0.elapsed().as_secs_f64();

        // Streaming path.
        let t1 = Instant::now();
        let stream_text = run_streaming(&stt, &frame).await;
        let stream_wall = t1.elapsed().as_secs_f64();

        let (bw_edits, ref_words) = word_counts(reference, &batch_text);
        let (bc_edits, ref_chars) = char_counts(reference, &batch_text);
        let (sw_edits, _) = word_counts(reference, &stream_text);
        let (sc_edits, _) = char_counts(reference, &stream_text);

        eprintln!(
            "  {name:<28} {audio_secs:>5.1}s  batch WER {:>5.1}%  stream WER {:>5.1}%",
            rate(bw_edits, ref_words) * 100.0,
            rate(sw_edits, ref_words) * 100.0,
        );

        results.push(ClipResult {
            name,
            audio_secs,
            batch_text,
            stream_text,
            batch_word_edits: bw_edits,
            batch_char_edits: bc_edits,
            stream_word_edits: sw_edits,
            stream_char_edits: sc_edits,
            ref_words,
            ref_chars,
            batch_rtfx: if batch_wall > 0.0 {
                audio_secs / batch_wall
            } else {
                0.0
            },
            stream_rtfx: if stream_wall > 0.0 {
                audio_secs / stream_wall
            } else {
                0.0
            },
        });
    }

    if results.is_empty() {
        return Err("no clips decoded successfully".into());
    }

    // ── Corpus aggregates (micro-averaged) ──
    let sum_ref_words: usize = results.iter().map(|r| r.ref_words).sum();
    let sum_ref_chars: usize = results.iter().map(|r| r.ref_chars).sum();
    let batch_wer = rate(
        results.iter().map(|r| r.batch_word_edits).sum(),
        sum_ref_words,
    );
    let stream_wer = rate(
        results.iter().map(|r| r.stream_word_edits).sum(),
        sum_ref_words,
    );
    let batch_cer = rate(
        results.iter().map(|r| r.batch_char_edits).sum(),
        sum_ref_chars,
    );
    let stream_cer = rate(
        results.iter().map(|r| r.stream_char_edits).sum(),
        sum_ref_chars,
    );
    let mean_batch_rtfx = results.iter().map(|r| r.batch_rtfx).sum::<f64>() / results.len() as f64;
    let mean_stream_rtfx =
        results.iter().map(|r| r.stream_rtfx).sum::<f64>() / results.len() as f64;
    // Batch-to-stream factor: how much WER degrades going from full-context
    // batch decode to cache-aware streaming. >1 means streaming is worse.
    let bsf = if batch_wer > 0.0 {
        stream_wer / batch_wer
    } else {
        f64::NAN
    };

    println!("\n=== corpus summary ({} clips) ===", results.len());
    println!(
        "  batch   WER {:>6.2}%   CER {:>6.2}%   mean RTFx {:>5.2}x",
        batch_wer * 100.0,
        batch_cer * 100.0,
        mean_batch_rtfx
    );
    println!(
        "  stream  WER {:>6.2}%   CER {:>6.2}%   mean RTFx {:>5.2}x",
        stream_wer * 100.0,
        stream_cer * 100.0,
        mean_stream_rtfx
    );
    println!("  BSF (stream WER / batch WER): {bsf:.3}");

    if let Some(json_path) = json_out {
        let clips_json: Vec<String> = results
            .iter()
            .map(|r| {
                format!(
                    "    {{\"clip\": {:?}, \"audio_secs\": {:.3}, \"ref_words\": {}, \
                     \"batch_wer\": {:.4}, \"stream_wer\": {:.4}, \
                     \"batch_rtfx\": {:.3}, \"stream_rtfx\": {:.3}, \
                     \"batch_text\": {:?}, \"stream_text\": {:?}}}",
                    r.name,
                    r.audio_secs,
                    r.ref_words,
                    rate(r.batch_word_edits, r.ref_words),
                    rate(r.stream_word_edits, r.ref_words),
                    r.batch_rtfx,
                    r.stream_rtfx,
                    r.batch_text,
                    r.stream_text,
                )
            })
            .collect();
        let json = format!(
            "{{\n  \"model_dir\": {:?},\n  \"clips\": {},\n  \"batch_wer\": {:.4},\n  \"stream_wer\": {:.4},\n  \"batch_cer\": {:.4},\n  \"stream_cer\": {:.4},\n  \"mean_batch_rtfx\": {:.3},\n  \"mean_stream_rtfx\": {:.3},\n  \"bsf\": {:.4},\n  \"per_clip\": [\n{}\n  ]\n}}\n",
            model_dir.display().to_string(),
            results.len(),
            batch_wer,
            stream_wer,
            batch_cer,
            stream_cer,
            mean_batch_rtfx,
            mean_stream_rtfx,
            bsf,
            clips_json.join(",\n"),
        );
        std::fs::write(&json_path, json)?;
        println!("  wrote {}", json_path.display());
    }

    Ok(())
}
