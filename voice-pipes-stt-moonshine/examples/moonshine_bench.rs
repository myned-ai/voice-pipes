//! Throwaway benchmark: how long does moonshine take to transcribe a real
//! captured utterance on THIS host's execution provider?
//!
//! Used to confirm whether the ~1700ms STT `processing_ms` seen in session
//! ba0865ca is GPU-class or CPU-class. Compare:
//!
//! ```text
//! CPU:  cargo run --release -p voice-pipes-stt-moonshine --example moonshine_bench -- <wav>...
//! CUDA: VP_ORT_EP=cuda cargo run --release -p voice-pipes-stt-moonshine \
//!           --example moonshine_bench --features ort-cuda -- <wav>...
//! ```
//!
//! Run from the `voice-pipes/` dir so `models/moonshine` resolves.

use std::time::Instant;

use voice_pipes_sdk::{AudioFrame, BuildContext, SttConfig};

fn read_wav(path: &str) -> AudioFrame {
    let mut r = hound::WavReader::open(path).expect("open wav");
    let spec = r.spec();
    let samples: Vec<i16> = match spec.sample_format {
        hound::SampleFormat::Int => r.samples::<i16>().map(|s| s.unwrap()).collect(),
        hound::SampleFormat::Float => r
            .samples::<f32>()
            .map(|s| (s.unwrap() * 32767.0) as i16)
            .collect(),
    };
    AudioFrame {
        samples,
        sample_rate: spec.sample_rate,
        channels: spec.channels,
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("usage: moonshine_bench <wav>...");
        std::process::exit(2);
    }

    let cfg = SttConfig::new("moonshine").with_model_dir(Some("models/moonshine".into()));
    let provider = voice_pipes_stt_moonshine::factory()
        .build(&cfg, &BuildContext::new())
        .expect("build moonshine");
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    println!(
        "VP_ORT_EP={:?}  ort-cuda-compiled={}",
        std::env::var("VP_ORT_EP").ok(),
        cfg!(feature = "ort-cuda"),
    );

    for path in &args {
        let frame = read_wav(path);
        let audio_s =
            frame.samples.len() as f64 / frame.sample_rate as f64 / frame.channels.max(1) as f64;
        // Warm once (session/model init), then take the best of 3.
        let _ = rt.block_on(provider.transcribe(&frame));
        let mut best_ms = f64::MAX;
        let mut text = String::new();
        for _ in 0..3 {
            let t = Instant::now();
            let res = rt
                .block_on(provider.transcribe(&frame))
                .expect("transcribe");
            best_ms = best_ms.min(t.elapsed().as_secs_f64() * 1000.0);
            text = res.text;
        }
        println!(
            "{path}  audio={audio_s:.2}s  best_processing={best_ms:.0}ms  rtf={:.2}  text={text:?}",
            (best_ms / 1000.0) / audio_s.max(0.001)
        );
    }
}
