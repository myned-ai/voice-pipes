//! WAV file reader -- loads audio into an [`AudioFrame`].

use std::path::Path;

use hound::WavReader;

use super::AudioFrame;

/// Read a WAV file into an `AudioFrame`.
pub fn read_wav(path: &Path) -> Result<AudioFrame, anyhow::Error> {
    let mut reader = WavReader::open(path)?;
    let spec = reader.spec();

    let samples: Vec<i16> = match spec.bits_per_sample {
        16 => reader.samples::<i16>().collect::<Result<_, _>>()?,
        32 if spec.sample_format == hound::SampleFormat::Float => {
            // Convert f32 -> i16
            reader
                .samples::<f32>()
                .map(|s| s.map(|v| (v.clamp(-1.0, 1.0) * i16::MAX as f32) as i16))
                .collect::<Result<_, _>>()?
        }
        other => {
            anyhow::bail!("unsupported WAV bits_per_sample: {other}");
        }
    };

    Ok(AudioFrame {
        samples,
        sample_rate: spec.sample_rate,
        channels: spec.channels,
    })
}

#[cfg(test)]
#[path = "tests/wav_reader.rs"]
mod tests;
