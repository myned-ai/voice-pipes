//! Voice conditioning state, compatibility fingerprinting, and per-request
//! deterministic PRNG (zero global lock contention across concurrent streams).

use std::sync::Arc;

use candle_core::{Device, Result, Tensor};

use crate::config::VoiceFingerprint;
use crate::model::kv_cache::VoiceKvPrefix;

/// Immutable cached voice conditioning state shared across concurrent sessions.
#[derive(Debug, Clone)]
pub struct VoiceState {
    pub name: String,
    pub fingerprint: VoiceFingerprint,
    pub kv_prefix: Arc<VoiceKvPrefix>,
}

/// Fast, deterministic, per-request PRNG (`xorshift64*` + Box-Muller Gaussian).
///
/// Avoids any global RNG lock contention across concurrent threads and guarantees
/// reproducible synthesis when a seed is configured.
#[derive(Debug, Clone)]
pub struct RequestRng {
    state: u64,
    spare: Option<f32>,
}

impl RequestRng {
    pub fn new(seed: u64) -> Self {
        let non_zero = if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        };
        Self {
            state: non_zero,
            spare: None,
        }
    }

    /// Derive a fresh per-request seed from a monotonic counter + optional base seed.
    pub fn for_request(base_seed: Option<u64>, request_seq: u64, attempt: u32) -> Self {
        let base = base_seed.unwrap_or(0xA076_1D64_78BD_642F);
        let mixed = base
            .wrapping_add(request_seq.wrapping_mul(0xE703_7ED1_A0B4_28DB))
            .wrapping_add(u64::from(attempt).wrapping_mul(0x8EBC_6AF0_9C88_C6E3));
        Self::new(mixed)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform float in `(0, 1)`.
    fn next_uniform_open(&mut self) -> f32 {
        // Use top 24 bits to stay exact in f32 mantissa, clamped away from 0.0.
        let bits = (self.next_u64() >> 40) as u32;
        ((bits as f32) + 0.5) / ((1u32 << 24) as f32)
    }

    /// Standard normal `N(0, 1)` sample via Box-Muller transform.
    pub fn next_normal(&mut self) -> f32 {
        if let Some(s) = self.spare.take() {
            return s;
        }
        let u1 = self.next_uniform_open();
        let u2 = self.next_uniform_open();
        let r = (-2.0_f32 * u1.ln()).sqrt();
        let theta = std::f32::consts::TAU * u2;
        self.spare = Some(r * theta.sin());
        r * theta.cos()
    }

    /// Sample a `[1, dim]` Gaussian noise tensor scaled by `std_dev`.
    pub fn normal_tensor(&mut self, dim: usize, std_dev: f32, device: &Device) -> Result<Tensor> {
        let mut data = Vec::with_capacity(dim);
        for _ in 0..dim {
            data.push(self.next_normal() * std_dev);
        }
        Tensor::from_vec(data, (1, dim), device)
    }
}
