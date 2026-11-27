//! Zero-copy prefix-shared KV cache and online split-attention for `FlowLM`.
//!
//! Instead of cloning the voice prompt's KV cache on every synthesis request,
//! the voice prefix is stored once as an immutable [`Arc<VoiceKvPrefix>`] and
//! shared read-only across all concurrent sessions. Each request allocates only
//! a compact [`DeltaKvCache`] for its own text + generated audio tokens, and
//! [`split_attention`] fuses prefix + delta attention via online log-sum-exp
//! softmax without ever copying the prefix tensors.

use candle_core::{D, Device, Result, Tensor};

/// One layer's immutable precomputed K/V tensors for a voice prompt.
/// Shapes: `[1, num_heads, prefix_len, head_dim]`.
#[derive(Debug, Clone)]
pub struct LayerKvPrefix {
    pub k: Tensor,
    pub v: Tensor,
}

/// Immutable voice-conditioning KV prefix shared across all concurrent requests
/// using the same voice (`Arc<VoiceKvPrefix>`).
#[derive(Debug, Clone)]
pub struct VoiceKvPrefix {
    pub layers: Vec<LayerKvPrefix>,
    pub prefix_len: usize,
}

impl VoiceKvPrefix {
    /// Empty prefix (used when no voice conditioning frames precede text).
    #[cfg(test)]
    pub fn empty(num_layers: usize) -> Self {
        Self {
            layers: Vec::with_capacity(num_layers),
            prefix_len: 0,
        }
    }

    pub fn layer(&self, idx: usize) -> Option<&LayerKvPrefix> {
        if self.prefix_len == 0 {
            None
        } else {
            self.layers.get(idx)
        }
    }
}

/// Pre-allocated per-request KV buffer for one transformer layer.
///
/// Allocates `[1, num_heads, max_len, head_dim]` once per request and updates
/// the active slice in place via `slice_assign`, avoiding O(N^2) tensor
/// concatenation reallocations during autoregressive generation.
#[derive(Debug)]
pub struct LayerDeltaKv {
    k: Tensor,
    v: Tensor,
    len: usize,
    max_len: usize,
}

impl LayerDeltaKv {
    pub fn new(num_heads: usize, max_len: usize, head_dim: usize, device: &Device) -> Result<Self> {
        let k = Tensor::zeros(
            (1, num_heads, max_len, head_dim),
            candle_core::DType::F32,
            device,
        )?;
        let v = Tensor::zeros(
            (1, num_heads, max_len, head_dim),
            candle_core::DType::F32,
            device,
        )?;
        Ok(Self {
            k,
            v,
            len: 0,
            max_len,
        })
    }

    /// Append `new_k` / `new_v` of shape `[1, num_heads, step_len, head_dim]`
    /// and return views `(k_active, v_active)` over `0..self.len`.
    pub fn append(&mut self, new_k: &Tensor, new_v: &Tensor) -> Result<(Tensor, Tensor)> {
        let step_len = new_k.dim(2)?;
        let end = self.len.saturating_add(step_len);
        if end > self.max_len {
            candle_core::bail!(
                "pocket-candle delta KV cache overflow: len {} + step {} > max {}",
                self.len,
                step_len,
                self.max_len
            );
        }
        let k_contig = new_k.contiguous()?;
        let v_contig = new_v.contiguous()?;
        self.k = self.k.slice_assign(
            &[
                0..1,
                0..k_contig.dim(1)?,
                self.len..end,
                0..k_contig.dim(3)?,
            ],
            &k_contig,
        )?;
        self.v = self.v.slice_assign(
            &[
                0..1,
                0..v_contig.dim(1)?,
                self.len..end,
                0..v_contig.dim(3)?,
            ],
            &v_contig,
        )?;
        self.len = end;
        Ok((
            self.k.narrow(2, 0, self.len)?,
            self.v.narrow(2, 0, self.len)?,
        ))
    }

    /// Reset cursor to zero for an in-place silent-start retry without reallocating.
    pub fn reset(&mut self) {
        self.len = 0;
    }

    pub fn len(&self) -> usize {
        self.len
    }

    /// Return read-only views `(k_active, v_active)` over `0..self.len`.
    pub fn active_kv(&self) -> Result<(Tensor, Tensor)> {
        Ok((
            self.k.narrow(2, 0, self.len)?,
            self.v.narrow(2, 0, self.len)?,
        ))
    }
}

/// Per-request mutable delta KV cache across all `FlowLM` layers.
#[derive(Debug)]
pub struct DeltaKvCache {
    pub layers: Vec<LayerDeltaKv>,
}

impl DeltaKvCache {
    pub fn new(
        num_layers: usize,
        num_heads: usize,
        max_len: usize,
        head_dim: usize,
        device: &Device,
    ) -> Result<Self> {
        let mut layers = Vec::with_capacity(num_layers);
        for _ in 0..num_layers {
            layers.push(LayerDeltaKv::new(num_heads, max_len, head_dim, device)?);
        }
        Ok(Self { layers })
    }

    pub fn reset(&mut self) {
        for layer in &mut self.layers {
            layer.reset();
        }
    }

    pub fn current_len(&self) -> usize {
        self.layers.first().map_or(0, LayerDeltaKv::len)
    }
}

/// Exact causal attention over an optional immutable [`LayerKvPrefix`] followed by
/// the request's active `(delta_k, delta_v)` using online log-sum-exp fusion.
///
/// Shapes:
/// - `q`: `[1, H, Q, D]`
/// - `prefix`: optional `(K_p, V_p)` of `[1, H, P, D]` (always fully visible to all Q)
/// - `delta_k`, `delta_v`: `[1, H, T, D]` where `Q` attends causally to the last `Q`
///   positions of `T` (so `past_delta = T - Q` are visible to all queries, and the
///   trailing `Q x Q` block is lower-triangular).
pub fn split_causal_attention(
    q: &Tensor,
    prefix: Option<&LayerKvPrefix>,
    delta_k: &Tensor,
    delta_v: &Tensor,
    scale: f64,
) -> Result<Tensor> {
    let q_len = q.dim(2)?;
    let delta_len = delta_k.dim(2)?;
    let device = q.device();

    let mut delta_scores = (q.matmul(&delta_k.transpose(2, 3)?)? * scale)?;
    if q_len > 1 {
        let mask = causal_mask(q_len, delta_len, device)?;
        delta_scores = delta_scores.broadcast_add(&mask)?;
    }

    let Some(pref) = prefix else {
        let attn = candle_nn::ops::softmax_last_dim(&delta_scores)?;
        return attn.matmul(&delta_v.contiguous()?);
    };

    // Two-part online softmax fusion (zero-copy over shared voice prefix):
    //   m_total = max(m_p, m_d)
    //   out = (exp(m_p - m_total) * sum_p * out_p + exp(m_d - m_total) * sum_d * out_d)
    //         / (exp(m_p - m_total) * sum_p + exp(m_d - m_total) * sum_d)
    let pref_scores = (q.matmul(&pref.k.transpose(2, 3)?)? * scale)?;

    let m_p = pref_scores.max_keepdim(D::Minus1)?;
    let m_d = delta_scores.max_keepdim(D::Minus1)?;
    let m_tot = m_p.maximum(&m_d)?;

    let exp_p = pref_scores.broadcast_sub(&m_tot)?.exp()?;
    let exp_d = delta_scores.broadcast_sub(&m_tot)?.exp()?;

    let sum_p = exp_p.sum_keepdim(D::Minus1)?;
    let sum_d = exp_d.sum_keepdim(D::Minus1)?;
    let denom = (&sum_p + &sum_d)?;

    let num_p = exp_p.matmul(&pref.v.contiguous()?)?;
    let num_d = exp_d.matmul(&delta_v.contiguous()?)?;
    (&num_p + &num_d)?.broadcast_div(&denom)
}

/// Build an additive causal mask `[1, 1, q_len, delta_len]` with `0.0` on allowed
/// positions (`key_pos <= past_delta + query_pos`) and `-1e9` on future positions.
fn causal_mask(q_len: usize, delta_len: usize, device: &Device) -> Result<Tensor> {
    let past = delta_len.saturating_sub(q_len);
    let mut data = vec![0.0_f32; q_len * delta_len];
    for qi in 0..q_len {
        let max_key = past + qi;
        for ki in (max_key + 1)..delta_len {
            if let Some(slot) = data.get_mut(qi * delta_len + ki) {
                *slot = -1.0e9_f32;
            }
        }
    }
    Tensor::from_vec(data, (1, 1, q_len, delta_len), device)
}
