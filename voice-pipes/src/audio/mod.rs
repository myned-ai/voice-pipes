//! Audio primitives -- frames, I/O, resampling, and WAV codecs.

pub mod frame;
pub mod input_meter;
#[cfg(feature = "audio-device")]
pub mod io;
pub mod wav_reader;
pub mod wav_writer;

pub use frame::AudioFrame;
pub use input_meter::{InputAudioMeter, InputAudioStats};
pub use voice_pipes_sdk::audio::resample;

/// Root-mean-square level of a PCM16 buffer (0.0 for empty input).
/// Canonical implementation shared by audio IO and the VAD stack.
pub fn compute_rms(samples: &[i16]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f64 = samples.iter().map(|&s| (s as f64) * (s as f64)).sum();
    (sum / samples.len() as f64).sqrt()
}

#[cfg(feature = "audio-device")]
pub mod playback;

use std::sync::Arc;

use crate::component::{Component, ComponentCore, ComponentKind, CoreComponent};

/// Build a minimal "always-healthy" `Component` for audio I/O so that the
/// worker registry can expose audio plumbing alongside STT/TTS/Text/VAD.
///
/// The returned component carries only the standard counters / latency
/// window; instrumentation can be added later by recording into the same
/// `ComponentCore` from the I/O hot paths.
pub fn build_io_component(id: impl Into<String>) -> Arc<dyn Component> {
    let core = Arc::new(ComponentCore::new(id, ComponentKind::Audio));
    Arc::new(CoreComponent::new(core))
}
