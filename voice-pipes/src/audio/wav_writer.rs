//! WAV file writer -- persists an [`AudioFrame`] to disk or encodes to bytes.

use std::io::Cursor;
use std::path::Path;

use hound::{SampleFormat, WavSpec, WavWriter};

use super::AudioFrame;

fn wav_spec(frame: &AudioFrame) -> WavSpec {
    WavSpec {
        channels: frame.channels,
        sample_rate: frame.sample_rate,
        bits_per_sample: 16,
        sample_format: SampleFormat::Int,
    }
}

/// Write an `AudioFrame` to a WAV file.
pub fn write_wav(frame: &AudioFrame, path: &Path) -> Result<(), anyhow::Error> {
    let mut writer = WavWriter::create(path, wav_spec(frame))?;
    for &sample in &frame.samples {
        writer.write_sample(sample)?;
    }
    writer.finalize()?;

    Ok(())
}

/// Encode an `AudioFrame` as WAV bytes in memory.
pub fn encode_wav(frame: &AudioFrame) -> Result<Vec<u8>, anyhow::Error> {
    let mut buf = Cursor::new(Vec::new());
    {
        let mut writer = WavWriter::new(&mut buf, wav_spec(frame))?;
        for &sample in &frame.samples {
            writer.write_sample(sample)?;
        }
        writer.finalize()?;
    };
    Ok(buf.into_inner())
}

#[cfg(test)]
#[path = "tests/wav_writer.rs"]
mod tests;
