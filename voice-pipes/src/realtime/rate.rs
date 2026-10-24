//! Sample-rate conversion of a session's audio, one stream per direction.
//!
//! Providers send and receive audio in pieces a few milliseconds long, at
//! the vendor's rate. Resampling each piece on its own rounds every length
//! (the stream drifts) and restarts the filter at every edge (a click per
//! piece), so each direction of a session keeps one
//! [`StreamingResampler`] for its whole length.

use std::ops::RangeInclusive;

use voice_pipes_sdk::audio::resample::StreamingResampler;

use super::event::RealtimeEvent;

/// Source rates converted. A zero would divide by zero, and a rate far
/// below speech rates would multiply one piece of audio into gigabytes;
/// every microphone and provider rate in use is inside.
const RATES: RangeInclusive<u32> = 8_000..=192_000;

/// One direction's audio, converted to `to` as a single stream.
#[derive(Debug)]
pub(crate) struct StreamRate {
    to: u32,
    /// The source rate and the resampler for it; `None` while the audio
    /// is already at `to`, or before any has arrived.
    stage: Option<(u32, StreamingResampler)>,
}

impl StreamRate {
    pub(crate) fn new(to: u32) -> Self {
        Self { to, stage: None }
    }

    /// The rate every converted sample is at.
    pub(crate) fn to(&self) -> u32 {
        self.to
    }

    /// Convert `samples` sent at `from`. A change of source rate ends the
    /// old stream: its held tail comes out first, then the new audio.
    ///
    /// # Errors
    /// A source rate outside [`RATES`], returned as the error.
    pub(crate) fn convert(&mut self, samples: &[i16], from: u32) -> Result<Vec<i16>, u32> {
        if !RATES.contains(&from) {
            return Err(from);
        }
        if from == self.to {
            let mut out = self.flush();
            out.extend_from_slice(samples);
            return Ok(out);
        }
        if let Some((rate, resampler)) = self.stage.as_mut()
            && *rate == from
        {
            return Ok(resampler.process_chunk(samples));
        }
        // `from` passed the range check above, which the resampler's own range
        // contains; the error is only the same refusal.
        let mut resampler = StreamingResampler::new(from, self.to).map_err(|_refused| from)?;
        let mut out = self.flush();
        out.extend(resampler.process_chunk(samples));
        self.stage = Some((from, resampler));
        Ok(out)
    }

    /// Convert to `to` from now on. A protocol may learn its rate after
    /// connecting (an agent's configured input format arrives with the
    /// first frame); what the filter holds was meant for the old rate and
    /// is dropped.
    pub(crate) fn retarget(&mut self, to: u32) {
        if to != self.to {
            self.to = to;
            self.stage = None;
        }
    }

    /// End the stream: the samples the filter still holds.
    pub(crate) fn flush(&mut self) -> Vec<i16> {
        self.stage
            .take()
            .map(|(_, mut resampler)| resampler.flush())
            .unwrap_or_default()
    }

    /// Drop what the filter holds: audio that will not be played.
    pub(crate) fn reset(&mut self) {
        self.stage = None;
    }
}

/// The downlink: every provider event passes through here once, in order,
/// and audio leaves at the session's output rate.
#[derive(Debug)]
pub(crate) struct Downlink {
    rate: StreamRate,
}

impl Downlink {
    pub(crate) fn new(output_rate: u32) -> Self {
        Self {
            rate: StreamRate::new(output_rate),
        }
    }

    /// The events to forward for `ev`. Audio is converted; the end of a
    /// response first releases the audio the filter still holds, so no
    /// reply loses its last milliseconds; speech from the user, a
    /// cancelled or new response, and a closed connection drop it, since
    /// that audio belongs to a reply that will not be played.
    pub(crate) fn on_event(&mut self, ev: RealtimeEvent) -> Vec<RealtimeEvent> {
        match ev {
            RealtimeEvent::ResponseAudioDelta {
                samples,
                sample_rate,
                alignment,
            } => match self.rate.convert(&samples, sample_rate) {
                Ok(samples) if samples.is_empty() && alignment.is_none() => Vec::new(),
                Ok(samples) => vec![RealtimeEvent::ResponseAudioDelta {
                    samples,
                    sample_rate: self.rate.to(),
                    alignment,
                }],
                Err(rate) => vec![RealtimeEvent::Error {
                    code: Some("audio_rate".to_string()),
                    message: format!("provider audio at an unsupported rate ({rate} Hz) dropped"),
                    recoverable: true,
                }],
            },
            RealtimeEvent::ResponseCompleted { .. } => {
                let tail = self.rate.flush();
                let mut out = Vec::with_capacity(2);
                if !tail.is_empty() {
                    out.push(RealtimeEvent::ResponseAudioDelta {
                        samples: tail,
                        sample_rate: self.rate.to(),
                        alignment: None,
                    });
                }
                out.push(ev);
                out
            }
            RealtimeEvent::InputSpeechStarted
            | RealtimeEvent::ResponseCancelled { .. }
            | RealtimeEvent::ResponseStarted { .. }
            | RealtimeEvent::Closed { .. } => {
                self.rate.reset();
                vec![ev]
            }
            other => vec![other],
        }
    }
}

#[cfg(test)]
#[path = "tests/rate.rs"]
mod tests;
