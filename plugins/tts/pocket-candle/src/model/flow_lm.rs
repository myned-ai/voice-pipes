//! `FlowLM` causal audio language model and LSD `SimpleMLPAdaLN` flow-matching head.
//!
//! Aligned directly to Kyutai's `kyutai/pocket-tts` safetensors layout:
//! - `conditioner.embed.weight` (`[4001, 1024]`)
//! - `speaker_proj_weight` (`[1024, 32]`), optional `bos_before_voice` (`[1, 1, 1024]`)
//! - `transformer.layers.{0..5}` (`norm1`, `self_attn.{in_proj,out_proj}`, `norm2`, `linear1`, `linear2`)
//! - `flow_net.time_embed.{0,1}` (`freqs`, `mlp.0`, `mlp.2`, `mlp.3.alpha` `RMSNorm`)
//! - `flow_net.res_blocks.{0..5}` (`in_ln`, `mlp.{0,2}`, `adaLN_modulation.1`)
//! - `flow_net.final_layer` (parameterless `LayerNorm` + `adaLN_modulation.1` + `linear`)

use candle_core::{D, Device, Module, Result, Tensor};
use candle_nn::{
    Embedding, LayerNorm, Linear, VarBuilder, embedding, layer_norm, linear, linear_no_bias,
};

use crate::config::PocketArchConfig;
use crate::model::kv_cache::{DeltaKvCache, LayerKvPrefix, VoiceKvPrefix, split_causal_attention};
use crate::model::state::RequestRng;

/// Precomputed Rotary Positional Embedding (`RoPE`) tables for a single `head_dim`.
///
/// Uses Kyutai's interleaved complex-pair convention (`x.reshape(..., D/2, 2)`).
#[derive(Debug)]
pub(crate) struct RopeCache {
    cos: Tensor,
    sin: Tensor,
    max_seq: usize,
}

impl RopeCache {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "cos/sin in [-1, 1] fit exactly in f32"
    )]
    pub(crate) fn new(
        head_dim: usize,
        max_seq: usize,
        theta: u32,
        device: &Device,
    ) -> Result<Self> {
        let half = head_dim / 2;
        let mut inv_freq = Vec::with_capacity(half);
        for i in 0..half {
            let exp = (2.0 * (i as f64)) / (head_dim as f64);
            inv_freq.push(1.0_f64 / f64::from(theta).powf(exp));
        }
        let mut cos_data = Vec::with_capacity(max_seq * half);
        let mut sin_data = Vec::with_capacity(max_seq * half);
        for pos in 0..max_seq {
            for &freq in &inv_freq {
                let angle = (pos as f64) * freq;
                cos_data.push(angle.cos() as f32);
                sin_data.push(angle.sin() as f32);
            }
        }
        let cos = Tensor::from_vec(cos_data, (max_seq, half), device)?;
        let sin = Tensor::from_vec(sin_data, (max_seq, half), device)?;
        Ok(Self { cos, sin, max_seq })
    }

    /// Apply interleaved complex-pair rotary embedding to `x` of shape `[1, H, T, D]`.
    pub(crate) fn apply(&self, x: &Tensor, pos_offset: usize) -> Result<Tensor> {
        let num_heads = x.dim(1)?;
        let seq_len = x.dim(2)?;
        let head_dim = x.dim(3)?;
        let half = head_dim / 2;
        if pos_offset.saturating_add(seq_len) > self.max_seq {
            candle_core::bail!(
                "RoPE position {}..{} exceeds cache {}",
                pos_offset,
                pos_offset + seq_len,
                self.max_seq
            );
        }
        let cos = self
            .cos
            .narrow(0, pos_offset, seq_len)?
            .reshape((1, 1, seq_len, half, 1))?;
        let sin = self
            .sin
            .narrow(0, pos_offset, seq_len)?
            .reshape((1, 1, seq_len, half, 1))?;
        let pairs = x.reshape((1, num_heads, seq_len, half, 2))?;
        let xr = pairs.narrow(4, 0, 1)?;
        let xi = pairs.narrow(4, 1, 1)?;
        let rot_r = (xr.broadcast_mul(&cos)? - xi.broadcast_mul(&sin)?)?;
        let rot_i = (xr.broadcast_mul(&sin)? + xi.broadcast_mul(&cos)?)?;
        Tensor::cat(&[&rot_r, &rot_i], 4)?.reshape((1, num_heads, seq_len, head_dim))
    }
}

/// One causal transformer layer in `FlowLM`.
#[derive(Debug)]
struct FlowLmLayer {
    norm1: LayerNorm,
    in_proj: Linear,
    out_proj: Linear,
    norm2: LayerNorm,
    linear1: Linear,
    linear2: Linear,
    num_heads: usize,
    head_dim: usize,
}

impl FlowLmLayer {
    fn load(arch: &PocketArchConfig, vb: VarBuilder) -> Result<Self> {
        let d = arch.d_model;
        let head_dim = d / arch.num_heads.max(1);
        let norm1 = layer_norm(d, 1e-5, vb.pp("norm1"))?;
        let in_proj = linear_no_bias(d, d * 3, vb.pp("self_attn").pp("in_proj"))?;
        let out_proj = linear_no_bias(d, d, vb.pp("self_attn").pp("out_proj"))?;
        let norm2 = layer_norm(d, 1e-5, vb.pp("norm2"))?;
        let linear1 = linear_no_bias(d, arch.ffn_dim, vb.pp("linear1"))?;
        let linear2 = linear_no_bias(arch.ffn_dim, d, vb.pp("linear2"))?;
        Ok(Self {
            norm1,
            in_proj,
            out_proj,
            norm2,
            linear1,
            linear2,
            num_heads: arch.num_heads,
            head_dim,
        })
    }

    fn forward(
        &self,
        input: &Tensor,
        rope: &RopeCache,
        pos_offset: usize,
        prefix: Option<&LayerKvPrefix>,
        delta: &mut crate::model::kv_cache::LayerDeltaKv,
    ) -> Result<Tensor> {
        let seq_len = input.dim(1)?;
        let normed = self.norm1.forward(input)?;
        let qkv = self.in_proj.forward(&normed)?;
        let hidden_dim = self.num_heads * self.head_dim;

        let query = qkv
            .narrow(2, 0, hidden_dim)?
            .reshape((1, seq_len, self.num_heads, self.head_dim))?
            .transpose(1, 2)?;
        let key = qkv
            .narrow(2, hidden_dim, hidden_dim)?
            .reshape((1, seq_len, self.num_heads, self.head_dim))?
            .transpose(1, 2)?;
        let value = qkv
            .narrow(2, hidden_dim * 2, hidden_dim)?
            .reshape((1, seq_len, self.num_heads, self.head_dim))?
            .transpose(1, 2)?;

        let query = rope.apply(&query, pos_offset)?;
        let key = rope.apply(&key, pos_offset)?;
        let (delta_k, delta_v) = delta.append(&key, &value)?;

        let scale = 1.0 / (self.head_dim as f64).sqrt();
        let attn = split_causal_attention(&query, prefix, &delta_k, &delta_v, scale)?;
        let attn = attn.transpose(1, 2)?.reshape((1, seq_len, hidden_dim))?;
        let residual = (input + self.out_proj.forward(&attn)?)?;

        let ff = self.norm2.forward(&residual)?;
        let ff = self.linear2.forward(&self.linear1.forward(&ff)?.gelu()?)?;
        residual + ff
    }
}

/// Timestep embedder (`freqs [128]` -> `[cos, sin]` -> `mlp.0` -> `SiLU` -> `mlp.2` -> `RMSNorm` `mlp.3.alpha`).
#[derive(Debug)]
struct TimestepEmbedder {
    freqs: Tensor,
    mlp0: Linear,
    mlp2: Linear,
    rms_alpha: Tensor,
}

impl TimestepEmbedder {
    fn load(flow_dim: usize, vb: VarBuilder) -> Result<Self> {
        let freq_dim = (flow_dim / 2).min(256);
        let half = freq_dim / 2;
        let freqs = vb.get((half,), "freqs")?;
        let mlp0 = linear(freq_dim, flow_dim, vb.pp("mlp").pp("0"))?;
        let mlp2 = linear(flow_dim, flow_dim, vb.pp("mlp").pp("2"))?;
        let rms_alpha = vb.get((flow_dim,), "mlp.3.alpha")?;
        Ok(Self {
            freqs,
            mlp0,
            mlp2,
            rms_alpha,
        })
    }

    fn forward(&self, val: f32) -> Result<Tensor> {
        let args = (&self.freqs * f64::from(val))?.unsqueeze(0)?;
        let emb = Tensor::cat(&[&args.cos()?, &args.sin()?], 1)?;
        let hidden = candle_nn::ops::silu(&self.mlp0.forward(&emb)?)?;
        let hidden = self.mlp2.forward(&hidden)?;
        // Kyutai `_rms_norm`: `var = eps + x.var(dim=-1, unbiased=True, keepdim=True)`.
        let dim = hidden.dim(D::Minus1)?;
        let mean = hidden.mean_keepdim(D::Minus1)?;
        let centered = hidden.broadcast_sub(&mean)?;
        let sum_sq = centered.sqr()?.sum_keepdim(D::Minus1)?;
        let denom = (dim.saturating_sub(1).max(1)) as f64;
        let var = (sum_sq / denom)?;
        let inv_std = (var + 1e-5)?.sqrt()?;
        hidden
            .broadcast_div(&inv_std)?
            .broadcast_mul(&self.rms_alpha.unsqueeze(0)?)
    }
}

/// One `AdaLN` residual block inside `SimpleMLPAdaLN`.
#[derive(Debug)]
struct AdaLnResBlock {
    norm: LayerNorm,
    mlp1: Linear,
    mlp2: Linear,
    modulation: Linear,
}

impl AdaLnResBlock {
    fn load(dim: usize, vb: VarBuilder) -> Result<Self> {
        let norm = layer_norm(dim, 1e-6, vb.pp("in_ln"))?;
        let mlp1 = linear(dim, dim, vb.pp("mlp").pp("0"))?;
        let mlp2 = linear(dim, dim, vb.pp("mlp").pp("2"))?;
        let modulation = linear(dim, dim * 3, vb.pp("adaLN_modulation").pp("1"))?;
        Ok(Self {
            norm,
            mlp1,
            mlp2,
            modulation,
        })
    }

    fn forward(&self, x: &Tensor, cond: &Tensor) -> Result<Tensor> {
        let dim = x.dim(D::Minus1)?;
        let mods = self.modulation.forward(&candle_nn::ops::silu(cond)?)?;
        let shift = mods.narrow(D::Minus1, 0, dim)?;
        let scale = mods.narrow(D::Minus1, dim, dim)?;
        let gate = mods.narrow(D::Minus1, dim * 2, dim)?;

        let h = self.norm.forward(x)?;
        let h = (h.broadcast_mul(&(scale + 1.0)?)? + shift)?;
        let h = self
            .mlp2
            .forward(&candle_nn::ops::silu(&self.mlp1.forward(&h)?)?)?;
        x + gate.broadcast_mul(&h)?
    }
}

/// Parameterless `LayerNorm` (matches `PyTorch` `nn.LayerNorm(dim, elementwise_affine=False, eps=1e-6)`).
fn layer_norm_no_affine(x: &Tensor, eps: f64) -> Result<Tensor> {
    let mean = x.mean_keepdim(D::Minus1)?;
    let centered = x.broadcast_sub(&mean)?;
    let var = centered.sqr()?.mean_keepdim(D::Minus1)?;
    centered.broadcast_div(&(var + eps)?.sqrt()?)
}

/// LSD flow-matching velocity estimator (`SimpleMLPAdaLN`).
#[derive(Debug)]
struct FlowNet {
    input_proj: Linear,
    cond_embed: Linear,
    time_embed_s: TimestepEmbedder,
    time_embed_t: TimestepEmbedder,
    res_blocks: Vec<AdaLnResBlock>,
    final_mod: Linear,
    final_linear: Linear,
    flow_dim: usize,
}

impl FlowNet {
    fn load(arch: &PocketArchConfig, vb: VarBuilder) -> Result<Self> {
        let fd = arch.flow_dim;
        let input_proj = linear(arch.latent_dim, fd, vb.pp("input_proj"))?;
        let cond_embed = linear(arch.d_model, fd, vb.pp("cond_embed"))?;
        let time_embed_s = TimestepEmbedder::load(fd, vb.pp("time_embed").pp("0"))?;
        let time_embed_t = TimestepEmbedder::load(fd, vb.pp("time_embed").pp("1"))?;
        let mut res_blocks = Vec::with_capacity(arch.flow_depth);
        for i in 0..arch.flow_depth {
            res_blocks.push(AdaLnResBlock::load(fd, vb.pp("res_blocks").pp(i))?);
        }
        let final_mod = linear(
            fd,
            fd * 2,
            vb.pp("final_layer").pp("adaLN_modulation").pp("1"),
        )?;
        let final_linear = linear(fd, arch.latent_dim, vb.pp("final_layer").pp("linear"))?;
        Ok(Self {
            input_proj,
            cond_embed,
            time_embed_s,
            time_embed_t,
            res_blocks,
            final_mod,
            final_linear,
            flow_dim: fd,
        })
    }

    /// Predict flow velocity `v(x, cond, s, t)` of shape `[1, latent_dim]`.
    fn velocity(
        &self,
        latent: &Tensor,
        cond_proj: &Tensor,
        start_t: f32,
        end_t: f32,
    ) -> Result<Tensor> {
        let emb_s = self.time_embed_s.forward(start_t)?;
        let emb_t = self.time_embed_t.forward(end_t)?;
        let time_avg = ((emb_s + emb_t)? * 0.5)?;
        let cond = (cond_proj + time_avg)?;

        let mut hidden = self.input_proj.forward(latent)?;
        for block in &self.res_blocks {
            hidden = block.forward(&hidden, &cond)?;
        }

        let mods = self.final_mod.forward(&candle_nn::ops::silu(&cond)?)?;
        let shift = mods.narrow(D::Minus1, 0, self.flow_dim)?;
        let scale = mods.narrow(D::Minus1, self.flow_dim, self.flow_dim)?;
        let normed = layer_norm_no_affine(&hidden, 1e-6)?;
        let hidden = (normed.broadcast_mul(&(scale + 1.0)?)? + shift)?;
        self.final_linear.forward(&hidden)
    }

    /// Multi-step Lagrangian Self-Distillation (LSD) Euler integration from `s=0` to `t=1`.
    fn lsd_sample(
        &self,
        transformer_out: &Tensor,
        steps: usize,
        temperature: f32,
        rng: &mut RequestRng,
        latent_dim: usize,
    ) -> Result<Tensor> {
        let cond_proj = self.cond_embed.forward(transformer_out)?;
        let std_dev = temperature.max(0.0).sqrt();
        let mut state = rng.normal_tensor(latent_dim, std_dev, transformer_out.device())?;
        let num_steps = steps.max(1);
        let dt = 1.0_f32 / (num_steps as f32);
        for idx in 0..num_steps {
            let start_t = (idx as f32) * dt;
            let end_t = start_t + dt;
            let vel = self.velocity(&state, &cond_proj, start_t, end_t)?;
            state = (state + (vel * f64::from(dt))?)?;
        }
        Ok(state)
    }
}

/// Complete `FlowLM` model: text embedding, speaker projection, causal transformer,
/// EOS head, latent normalization stats, and LSD flow head.
#[derive(Debug)]
pub struct FlowLmModel {
    arch: PocketArchConfig,
    conditioner: Embedding,
    input_linear: Linear,
    speaker_proj: Linear,
    bos_before_voice: Option<Tensor>,
    layers: Vec<FlowLmLayer>,
    out_norm: LayerNorm,
    out_eos: Linear,
    flow_net: FlowNet,
    bos_emb: Tensor,
    emb_mean: Tensor,
    emb_std: Tensor,
    rope: RopeCache,
    device: Device,
}

impl FlowLmModel {
    /// Load `FlowLM` weights from a Candle [`VarBuilder`] (safetensors or synthetic test `VarMap`).
    pub fn load(arch: PocketArchConfig, vb: VarBuilder, device: &Device) -> Result<Self> {
        let conditioner = embedding(
            arch.vocab_size,
            arch.d_model,
            vb.pp("conditioner").pp("embed"),
        )?;
        let input_linear = linear_no_bias(arch.latent_dim, arch.d_model, vb.pp("input_linear"))?;
        let speaker_weight = vb.get((arch.d_model, arch.latent_dim), "speaker_proj_weight")?;
        let speaker_proj = Linear::new(speaker_weight, None);
        let bos_before_voice = vb.get((1, 1, arch.d_model), "bos_before_voice").ok();

        let mut layers = Vec::with_capacity(arch.num_layers);
        for i in 0..arch.num_layers {
            layers.push(FlowLmLayer::load(
                &arch,
                vb.pp("transformer").pp("layers").pp(i),
            )?);
        }
        let out_norm = layer_norm(arch.d_model, 1e-5, vb.pp("out_norm"))?;
        let out_eos = linear(arch.d_model, 1, vb.pp("out_eos"))?;
        let flow_net = FlowNet::load(&arch, vb.pp("flow_net"))?;

        let bos_emb = vb.get((arch.latent_dim,), "bos_emb")?;
        let emb_mean = vb.get((arch.latent_dim,), "emb_mean")?;
        let emb_std = vb.get((arch.latent_dim,), "emb_std")?;

        let head_dim = arch.d_model / arch.num_heads.max(1);
        let rope = RopeCache::new(head_dim, 4096, arch.rope_theta, device)?;

        Ok(Self {
            arch,
            conditioner,
            input_linear,
            speaker_proj,
            bos_before_voice,
            layers,
            out_norm,
            out_eos,
            flow_net,
            bos_emb,
            emb_mean,
            emb_std,
            rope,
            device: device.clone(),
        })
    }

    /// Build an immutable [`VoiceKvPrefix`] by running Mimi-encoded voice prompt
    /// latents `[1, T_voice, latent_dim]` through the `FlowLM` backbone once.
    pub fn build_voice_prefix(&self, voice_latents: &Tensor) -> Result<VoiceKvPrefix> {
        let projected = self.speaker_proj.forward(voice_latents)?;
        let mut h = if let Some(ref bos) = self.bos_before_voice {
            Tensor::cat(&[bos, &projected], 1)?
        } else {
            projected
        };
        let seq_len = h.dim(1)?;
        let head_dim = self.arch.d_model / self.arch.num_heads.max(1);
        let mut cache = DeltaKvCache::new(
            self.arch.num_layers,
            self.arch.num_heads,
            seq_len,
            head_dim,
            &self.device,
        )?;
        let mut prefix_layers = Vec::with_capacity(self.arch.num_layers);
        for (idx, layer) in self.layers.iter().enumerate() {
            if let Some(delta) = cache.layers.get_mut(idx) {
                h = layer.forward(&h, &self.rope, 0, None, delta)?;
            }
        }
        for delta in cache.layers {
            let (k, v) = delta.active_kv()?;
            prefix_layers.push(LayerKvPrefix { k, v });
        }
        Ok(VoiceKvPrefix {
            layers: prefix_layers,
            prefix_len: seq_len,
        })
    }

    /// Prefill text token IDs into `delta_cache` (attending zero-copy to `voice_prefix`).
    pub fn prefill_text(
        &self,
        token_ids: &[u32],
        voice_prefix: &VoiceKvPrefix,
        delta_cache: &mut DeltaKvCache,
    ) -> Result<()> {
        let ids = Tensor::from_vec(token_ids.to_vec(), (1, token_ids.len()), &self.device)?;
        let mut h = self.conditioner.forward(&ids)?;
        let pos_offset = voice_prefix.prefix_len + delta_cache.current_len();
        for (idx, layer) in self.layers.iter().enumerate() {
            let Some(delta) = delta_cache.layers.get_mut(idx) else {
                candle_core::bail!("missing delta KV layer {idx}");
            };
            h = layer.forward(&h, &self.rope, pos_offset, voice_prefix.layer(idx), delta)?;
        }
        Ok(())
    }

    /// Run one autoregressive `FlowLM` step given the previous normalized audio latent
    /// (or `None` on step 0, which uses learned `bos_emb`).
    ///
    /// Returns `(next_norm_latent [1, latent_dim], denorm_latent [1, latent_dim], eos_logit)`.
    pub fn step(
        &self,
        prev_latent: Option<&Tensor>,
        voice_prefix: &VoiceKvPrefix,
        delta_cache: &mut DeltaKvCache,
        lsd_steps: usize,
        temperature: f32,
        rng: &mut RequestRng,
    ) -> Result<(Tensor, Tensor, f32)> {
        let inp = match prev_latent {
            Some(lat) => lat.reshape((1, 1, self.arch.latent_dim))?,
            None => self.bos_emb.reshape((1, 1, self.arch.latent_dim))?,
        };
        let mut h = self.input_linear.forward(&inp)?;
        let pos_offset = voice_prefix.prefix_len + delta_cache.current_len();
        for (idx, layer) in self.layers.iter().enumerate() {
            let Some(delta) = delta_cache.layers.get_mut(idx) else {
                candle_core::bail!("missing delta KV layer {idx}");
            };
            h = layer.forward(&h, &self.rope, pos_offset, voice_prefix.layer(idx), delta)?;
        }

        let h_norm = self.out_norm.forward(&h)?.squeeze(1)?;
        let eos_logit = self
            .out_eos
            .forward(&h_norm)?
            .flatten_all()?
            .to_vec1::<f32>()?
            .first()
            .copied()
            .unwrap_or(-10.0);

        let next_norm =
            self.flow_net
                .lsd_sample(&h_norm, lsd_steps, temperature, rng, self.arch.latent_dim)?;
        let denorm = next_norm
            .broadcast_mul(&self.emb_std.reshape((1, self.arch.latent_dim))?)?
            .broadcast_add(&self.emb_mean.reshape((1, self.arch.latent_dim))?)?;
        Ok((next_norm, denorm, eos_logit))
    }
}
