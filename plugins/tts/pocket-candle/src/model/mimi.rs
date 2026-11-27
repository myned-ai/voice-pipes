//! Streaming Mimi neural audio codec (both `SEANet` decoder and encoder),
//! aligned directly to Kyutai's `kyutai/pocket-tts` safetensors layout:
//! - Decoder: `quantizer.output_proj` -> depthwise `upsample.convtr.convtr` (16x) ->
//!   `decoder_transformer` (2 layers, `RoPE`, `layer_scale`) -> `decoder.model.{0,2,3,5,6,8,9,11}` (6x5x4 = 120x; total 1920x)
//! - Encoder: `encoder.model.{0,1,3,4,6,7,9,11}` (4x5x6 = 120x) ->
//!   `encoder_transformer` (2 layers) -> `downsample.conv.conv` (16x; total 1920x, 512 -> 32)

use candle_core::{DType, Device, Module, Result, Tensor};
use candle_nn::{
    Conv1d, Conv1dConfig, ConvTranspose1d, ConvTranspose1dConfig, LayerNorm, Linear, VarBuilder,
    conv_transpose1d, conv_transpose1d_no_bias, conv1d, conv1d_no_bias, layer_norm, linear_no_bias,
};

use crate::config::{FRAME_SAMPLES, PocketArchConfig};
use crate::model::flow_lm::RopeCache;
use crate::model::kv_cache::{DeltaKvCache, split_causal_attention};

/// Causal 1D convolution with an explicit streaming history ring buffer (`[1, C_in, k - 1]`).
#[derive(Debug)]
pub struct StreamingCausalConv1d {
    conv: Conv1d,
    in_channels: usize,
    kernel_size: usize,
    stride: usize,
    dilation: usize,
}

impl StreamingCausalConv1d {
    pub fn load(
        in_c: usize,
        out_c: usize,
        kernel_size: usize,
        stride: usize,
        dilation: usize,
        vb: VarBuilder,
    ) -> Result<Self> {
        let cfg = Conv1dConfig {
            padding: 0,
            stride,
            dilation,
            groups: 1,
            cudnn_fwd_algo: None,
        };
        let conv = conv1d(in_c, out_c, kernel_size, cfg, vb)?;
        Ok(Self {
            conv,
            in_channels: in_c,
            kernel_size,
            stride,
            dilation,
        })
    }

    pub fn load_no_bias(
        in_c: usize,
        out_c: usize,
        kernel_size: usize,
        stride: usize,
        vb: VarBuilder,
    ) -> Result<Self> {
        let cfg = Conv1dConfig {
            padding: 0,
            stride,
            dilation: 1,
            groups: 1,
            cudnn_fwd_algo: None,
        };
        let conv = conv1d_no_bias(in_c, out_c, kernel_size, cfg, vb)?;
        Ok(Self {
            conv,
            in_channels: in_c,
            kernel_size,
            stride,
            dilation: 1,
        })
    }

    pub fn context_len(&self) -> usize {
        let eff_k = self
            .kernel_size
            .saturating_sub(1)
            .saturating_mul(self.dilation)
            .saturating_add(1);
        eff_k.saturating_sub(self.stride)
    }

    pub fn init_state(&self, device: &Device) -> Result<Tensor> {
        Tensor::zeros(
            (1, self.in_channels, self.context_len()),
            DType::F32,
            device,
        )
    }

    pub fn forward_streaming(&self, x: &Tensor, state: &mut Tensor) -> Result<Tensor> {
        let ctx = self.context_len();
        if ctx == 0 {
            return self.conv.forward(x);
        }
        let combined = Tensor::cat(&[state as &Tensor, x], 2)?;
        let total = combined.dim(2)?;
        let out = self.conv.forward(&combined)?;
        let keep_start = total.saturating_sub(ctx);
        *state = combined.narrow(2, keep_start, ctx)?.contiguous()?;
        Ok(out)
    }

    pub fn forward_offline(&self, x: &Tensor) -> Result<Tensor> {
        let ctx = self.context_len();
        if ctx == 0 {
            return self.conv.forward(x);
        }
        let pad = Tensor::zeros((x.dim(0)?, self.in_channels, ctx), DType::F32, x.device())?;
        let combined = Tensor::cat(&[&pad, x], 2)?;
        self.conv.forward(&combined)
    }

    pub fn stride(&self) -> usize {
        self.stride
    }
}

/// Causal transposed 1D convolution with an overlap-add tail buffer (`[1, C_out, k - stride]`).
#[derive(Debug)]
pub struct StreamingCausalConvTranspose1d {
    conv_t: ConvTranspose1d,
    out_channels: usize,
    kernel_size: usize,
    stride: usize,
}

impl StreamingCausalConvTranspose1d {
    pub fn load(
        in_c: usize,
        out_c: usize,
        kernel_size: usize,
        stride: usize,
        vb: VarBuilder,
    ) -> Result<Self> {
        let cfg = ConvTranspose1dConfig {
            padding: 0,
            output_padding: 0,
            stride,
            dilation: 1,
            groups: 1,
        };
        let conv_t = conv_transpose1d(in_c, out_c, kernel_size, cfg, vb)?;
        Ok(Self {
            conv_t,
            out_channels: out_c,
            kernel_size,
            stride,
        })
    }

    pub fn load_depthwise_no_bias(
        channels: usize,
        kernel_size: usize,
        stride: usize,
        vb: VarBuilder,
    ) -> Result<Self> {
        let cfg = ConvTranspose1dConfig {
            padding: 0,
            output_padding: 0,
            stride,
            dilation: 1,
            groups: channels,
        };
        let conv_t = conv_transpose1d_no_bias(channels, channels, kernel_size, cfg, vb)?;
        Ok(Self {
            conv_t,
            out_channels: channels,
            kernel_size,
            stride,
        })
    }

    pub fn overlap_len(&self) -> usize {
        self.kernel_size.saturating_sub(self.stride)
    }

    pub fn init_state(&self, device: &Device) -> Result<Tensor> {
        Tensor::zeros(
            (1, self.out_channels, self.overlap_len()),
            DType::F32,
            device,
        )
    }

    pub fn forward_streaming(&self, x: &Tensor, overlap: &mut Tensor) -> Result<Tensor> {
        let raw = self.conv_t.forward(x)?;
        let ov = self.overlap_len();
        if ov == 0 {
            return Ok(raw);
        }
        let total = raw.dim(2)?;
        let emit_len = total.saturating_sub(ov);
        let head = (&raw.narrow(2, 0, ov)? + (overlap as &Tensor))?;
        let combined = if total > ov {
            let tail = raw.narrow(2, ov, total - ov)?;
            Tensor::cat(&[&head, &tail], 2)?
        } else {
            head
        };
        let mut next_overlap = combined.narrow(2, emit_len, ov)?;
        if let Some(bias) = self.conv_t.bias() {
            let b = bias.reshape((1, self.out_channels, 1))?;
            next_overlap = next_overlap.broadcast_sub(&b)?;
        }
        *overlap = next_overlap.contiguous()?;
        combined.narrow(2, 0, emit_len)
    }
}

/// Residual `SEANet` block (ELU -> dilated causal Conv1d -> ELU -> 1x1 Conv1d + skip).
#[derive(Debug)]
struct SeaNetResBlock {
    conv1: StreamingCausalConv1d,
    conv2: StreamingCausalConv1d,
}

impl SeaNetResBlock {
    fn load(channels: usize, vb: VarBuilder) -> Result<Self> {
        let hidden = (channels / 2).max(1);
        let conv1 = StreamingCausalConv1d::load(
            channels,
            hidden,
            3,
            1,
            1,
            vb.pp("block").pp("1").pp("conv"),
        )?;
        let conv2 = StreamingCausalConv1d::load(
            hidden,
            channels,
            1,
            1,
            1,
            vb.pp("block").pp("3").pp("conv"),
        )?;
        Ok(Self { conv1, conv2 })
    }

    fn init_state(&self, device: &Device) -> Result<Tensor> {
        self.conv1.init_state(device)
    }

    fn forward_streaming(&self, x: &Tensor, state: &mut Tensor) -> Result<Tensor> {
        let h = x.elu(1.0)?;
        let h = self.conv1.forward_streaming(&h, state)?;
        let h = h.elu(1.0)?;
        let mut empty = Tensor::zeros((1, 1, 0), DType::F32, x.device())?;
        let h = self.conv2.forward_streaming(&h, &mut empty)?;
        x + h
    }

    fn forward_offline(&self, x: &Tensor) -> Result<Tensor> {
        let h = x.elu(1.0)?;
        let h = self.conv1.forward_offline(&h)?;
        let h = h.elu(1.0)?;
        let h = self.conv2.forward_offline(&h)?;
        x + h
    }
}

/// Mimi local transformer layer with `layer_scale_1` / `layer_scale_2` and streaming KV cache.
#[derive(Debug)]
struct MimiTransformerLayer {
    norm1: LayerNorm,
    in_proj: Linear,
    out_proj: Linear,
    layer_scale_1: Tensor,
    norm2: LayerNorm,
    linear1: Linear,
    linear2: Linear,
    layer_scale_2: Tensor,
    num_heads: usize,
    head_dim: usize,
}

impl MimiTransformerLayer {
    fn load(dim: usize, num_heads: usize, vb: VarBuilder) -> Result<Self> {
        let head_dim = dim / num_heads.max(1);
        let ffn_dim = dim * 4;
        let norm1 = layer_norm(dim, 1e-5, vb.pp("norm1"))?;
        let in_proj = linear_no_bias(dim, dim * 3, vb.pp("self_attn").pp("in_proj"))?;
        let out_proj = linear_no_bias(dim, dim, vb.pp("self_attn").pp("out_proj"))?;
        let layer_scale_1 = vb.get((dim,), "layer_scale_1.scale")?;
        let norm2 = layer_norm(dim, 1e-5, vb.pp("norm2"))?;
        let linear1 = linear_no_bias(dim, ffn_dim, vb.pp("linear1"))?;
        let linear2 = linear_no_bias(ffn_dim, dim, vb.pp("linear2"))?;
        let layer_scale_2 = vb.get((dim,), "layer_scale_2.scale")?;
        Ok(Self {
            norm1,
            in_proj,
            out_proj,
            layer_scale_1,
            norm2,
            linear1,
            linear2,
            layer_scale_2,
            num_heads,
            head_dim,
        })
    }

    fn forward(
        &self,
        input: &Tensor,
        rope: &RopeCache,
        delta: &mut crate::model::kv_cache::LayerDeltaKv,
    ) -> Result<Tensor> {
        let seq_len = input.dim(1)?;
        let dim = self.num_heads * self.head_dim;
        let pos_offset = delta.len();
        let qkv = self.in_proj.forward(&self.norm1.forward(input)?)?;
        let query = qkv
            .narrow(2, 0, dim)?
            .reshape((1, seq_len, self.num_heads, self.head_dim))?
            .transpose(1, 2)?;
        let key = qkv
            .narrow(2, dim, dim)?
            .reshape((1, seq_len, self.num_heads, self.head_dim))?
            .transpose(1, 2)?;
        let value = qkv
            .narrow(2, dim * 2, dim)?
            .reshape((1, seq_len, self.num_heads, self.head_dim))?
            .transpose(1, 2)?;

        let query = rope.apply(&query, pos_offset)?;
        let key = rope.apply(&key, pos_offset)?;
        let (delta_k, delta_v) = delta.append(&key, &value)?;
        // Mimi local attention context window (250 frames at 200 Hz = 1.25s).
        let total_len = delta_k.dim(2)?;
        let win = 250usize.max(seq_len);
        let (win_k, win_v) = if total_len > win {
            let start = total_len - win;
            (
                delta_k.narrow(2, start, win)?,
                delta_v.narrow(2, start, win)?,
            )
        } else {
            (delta_k, delta_v)
        };
        let scale = 1.0 / (self.head_dim as f64).sqrt();
        let attn = split_causal_attention(&query, None, &win_k, &win_v, scale)?
            .transpose(1, 2)?
            .reshape((1, seq_len, dim))?;
        let attn_scaled = self
            .out_proj
            .forward(&attn)?
            .broadcast_mul(&self.layer_scale_1.reshape((1, 1, dim))?)?;
        let residual = (input + attn_scaled)?;

        let ff = self
            .linear2
            .forward(
                &self
                    .linear1
                    .forward(&self.norm2.forward(&residual)?)?
                    .gelu()?,
            )?
            .broadcast_mul(&self.layer_scale_2.reshape((1, 1, dim))?)?;
        residual + ff
    }
}

/// Per-request mutable streaming state for the Mimi decoder.
#[derive(Debug)]
pub struct MimiDecoderState {
    upsample_state: Tensor,
    dec_transformer_kv: DeltaKvCache,
    in_state: Tensor,
    upsample_states: Vec<Tensor>,
    res_states: Vec<Tensor>,
    out_state: Tensor,
    frame_index: usize,
}

/// Streaming Mimi codec: both **Decoder** (12.5 Hz latent -> 24 kHz PCM) and
/// **Encoder** (24 kHz reference WAV -> 12.5 Hz 32-d voice latents).
#[derive(Debug)]
pub struct MimiCodec {
    latent_in_proj: StreamingCausalConv1d,
    upsample: StreamingCausalConvTranspose1d,
    dec_transformer: Vec<MimiTransformerLayer>,
    dec_in_conv: StreamingCausalConv1d,
    dec_upsamples: Vec<StreamingCausalConvTranspose1d>,
    dec_res_blocks: Vec<SeaNetResBlock>,
    dec_out_conv: StreamingCausalConv1d,
    enc_in_conv: StreamingCausalConv1d,
    enc_res_blocks: Vec<SeaNetResBlock>,
    enc_downsamples: Vec<StreamingCausalConv1d>,
    enc_out_conv: StreamingCausalConv1d,
    enc_transformer: Vec<MimiTransformerLayer>,
    downsample: StreamingCausalConv1d,
    rope: RopeCache,
    mimi_dim: usize,
    mimi_heads: usize,
    device: Device,
}

impl MimiCodec {
    /// Load Mimi decoder and encoder weights from `vb` (`mimi.*` namespace).
    pub fn load(arch: &PocketArchConfig, vb: VarBuilder, device: &Device) -> Result<Self> {
        let up_ratio = arch.mimi_upsample_ratio;

        let latent_in_proj = StreamingCausalConv1d::load_no_bias(
            arch.latent_dim,
            arch.mimi_dim,
            1,
            1,
            vb.pp("quantizer").pp("output_proj"),
        )?;
        let upsample = StreamingCausalConvTranspose1d::load_depthwise_no_bias(
            arch.mimi_dim,
            up_ratio * 2,
            up_ratio,
            vb.pp("upsample").pp("convtr").pp("convtr"),
        )?;

        let dec_transformer = load_mimi_transformer(
            arch,
            vb.pp("decoder_transformer").pp("transformer").pp("layers"),
        )?;
        let (dec_in_conv, dec_upsamples, dec_res_blocks, dec_out_conv, base_c) =
            load_seanet_decoder(arch, vb.pp("decoder").pp("model"))?;
        let (enc_in_conv, enc_res_blocks, enc_downsamples, enc_out_conv) =
            load_seanet_encoder(arch, base_c, vb.pp("encoder").pp("model"))?;
        let enc_transformer = load_mimi_transformer(
            arch,
            vb.pp("encoder_transformer").pp("transformer").pp("layers"),
        )?;
        let downsample = StreamingCausalConv1d::load_no_bias(
            arch.mimi_dim,
            arch.latent_dim,
            up_ratio * 2,
            up_ratio,
            vb.pp("downsample").pp("conv").pp("conv"),
        )?;

        let head_dim = arch.mimi_dim / arch.mimi_transformer_heads.max(1);
        let rope = RopeCache::new(head_dim, 65_536, arch.rope_theta, device)?;

        Ok(Self {
            latent_in_proj,
            upsample,
            dec_transformer,
            dec_in_conv,
            dec_upsamples,
            dec_res_blocks,
            dec_out_conv,
            enc_in_conv,
            enc_res_blocks,
            enc_downsamples,
            enc_out_conv,
            enc_transformer,
            downsample,
            rope,
            mimi_dim: arch.mimi_dim,
            mimi_heads: arch.mimi_transformer_heads,
            device: device.clone(),
        })
    }

    /// Allocate a fresh per-request streaming decoder state.
    pub fn init_decoder_state(&self, max_audio_frames: usize) -> Result<MimiDecoderState> {
        let upsample_state = self.upsample.init_state(&self.device)?;
        let head_dim = self.mimi_dim / self.mimi_heads.max(1);
        let max_t_frames = max_audio_frames
            .saturating_mul(self.upsample.stride)
            .max(64);
        let dec_transformer_kv = DeltaKvCache::new(
            self.dec_transformer.len(),
            self.mimi_heads,
            max_t_frames,
            head_dim,
            &self.device,
        )?;
        let in_state = self.dec_in_conv.init_state(&self.device)?;
        let mut upsample_states = Vec::with_capacity(self.dec_upsamples.len());
        for up in &self.dec_upsamples {
            upsample_states.push(up.init_state(&self.device)?);
        }
        let mut res_states = Vec::with_capacity(self.dec_res_blocks.len());
        for res in &self.dec_res_blocks {
            res_states.push(res.init_state(&self.device)?);
        }
        let out_state = self.dec_out_conv.init_state(&self.device)?;
        Ok(MimiDecoderState {
            upsample_state,
            dec_transformer_kv,
            in_state,
            upsample_states,
            res_states,
            out_state,
            frame_index: 0,
        })
    }

    /// Decode a single 12.5 Hz latent frame `[1, latent_dim]` into `FRAME_SAMPLES` (1920)
    /// 24 kHz mono `i16` PCM samples, applying a raised-cosine fade-in on frame 0.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "val is clamped to [-1.0, 1.0] before scaling by i16::MAX"
    )]
    pub fn decode_frame(
        &self,
        latent: &Tensor,
        state: &mut MimiDecoderState,
        fade_in_samples: usize,
    ) -> Result<Vec<i16>> {
        let x = latent.unsqueeze(2)?; // [1, latent_dim, 1]
        let mut empty = Tensor::zeros((1, 1, 0), DType::F32, &self.device)?;
        let x = self.latent_in_proj.forward_streaming(&x, &mut empty)?; // [1, 512, 1]
        let up = self
            .upsample
            .forward_streaming(&x, &mut state.upsample_state)?; // [1, 512, 16]

        let mut t_in = up.transpose(1, 2)?; // [1, 16, 512]
        for (idx, layer) in self.dec_transformer.iter().enumerate() {
            if let Some(delta) = state.dec_transformer_kv.layers.get_mut(idx) {
                t_in = layer.forward(&t_in, &self.rope, delta)?;
            }
        }
        let mut h = self
            .dec_in_conv
            .forward_streaming(&t_in.transpose(1, 2)?, &mut state.in_state)?;

        for i in 0..self.dec_upsamples.len() {
            let (Some(up_conv), Some(up_st), Some(res), Some(res_st)) = (
                self.dec_upsamples.get(i),
                state.upsample_states.get_mut(i),
                self.dec_res_blocks.get(i),
                state.res_states.get_mut(i),
            ) else {
                candle_core::bail!("mimi decoder stage mismatch at {i}");
            };
            h = h.elu(1.0)?;
            h = up_conv.forward_streaming(&h, up_st)?;
            h = res.forward_streaming(&h, res_st)?;
        }
        h = h.elu(1.0)?;
        let wav = self
            .dec_out_conv
            .forward_streaming(&h, &mut state.out_state)?;
        let floats = wav.flatten_all()?.to_vec1::<f32>()?;

        let apply_fade = state.frame_index == 0 && fade_in_samples > 0;
        state.frame_index = state.frame_index.saturating_add(1);

        let mut pcm = Vec::with_capacity(floats.len().max(FRAME_SAMPLES));
        for (idx, &s) in floats.iter().enumerate() {
            let mut val = s.clamp(-1.0, 1.0);
            if apply_fade && idx < fade_in_samples {
                let t = (idx as f32) / (fade_in_samples as f32);
                let gain = 0.5 * (1.0 - (std::f32::consts::PI * t).cos());
                val *= gain;
            }
            let scaled = (val * f32::from(i16::MAX)).round();
            pcm.push(scaled as i16);
        }
        Ok(pcm)
    }

    /// Encode a 24 kHz mono reference waveform (`f32` in `[-1, 1]`) into 12.5 Hz
    /// Mimi latents `[1, T_frames, latent_dim]` for zero-shot voice cloning.
    pub fn encode_audio(&self, samples_24k: &[f32]) -> Result<Tensor> {
        if samples_24k.is_empty() {
            candle_core::bail!("cannot encode empty reference audio");
        }
        let seanet_stride: usize = self
            .enc_downsamples
            .iter()
            .map(StreamingCausalConv1d::stride)
            .product::<usize>()
            .max(1);
        let total_stride = seanet_stride
            .saturating_mul(self.downsample.stride())
            .max(1);
        let padded_len = samples_24k.len().div_ceil(total_stride) * total_stride;
        let mut padded = Vec::with_capacity(padded_len);
        padded.extend_from_slice(samples_24k);
        padded.resize(padded_len, 0.0_f32);

        let x = Tensor::from_vec(padded, (1, 1, padded_len), &self.device)?;
        let mut h = self.enc_in_conv.forward_offline(&x)?;
        for (res, down) in self.enc_res_blocks.iter().zip(self.enc_downsamples.iter()) {
            h = res.forward_offline(&h)?;
            h = h.elu(1.0)?;
            h = down.forward_offline(&h)?;
        }
        h = h.elu(1.0)?;
        let h = self.enc_out_conv.forward_offline(&h)?; // [1, 512, T_200hz]

        let t_len = h.dim(2)?;
        let head_dim = self.mimi_dim / self.mimi_heads.max(1);
        let mut enc_kv = DeltaKvCache::new(
            self.enc_transformer.len(),
            self.mimi_heads,
            t_len.max(1),
            head_dim,
            &self.device,
        )?;
        let mut t_in = h.transpose(1, 2)?;
        for (idx, layer) in self.enc_transformer.iter().enumerate() {
            if let Some(delta) = enc_kv.layers.get_mut(idx) {
                t_in = layer.forward(&t_in, &self.rope, delta)?;
            }
        }
        let down = self.downsample.forward_offline(&t_in.transpose(1, 2)?)?; // [1, latent_dim, T_12.5hz]
        down.transpose(1, 2)?.contiguous()
    }
}

fn load_mimi_transformer(
    arch: &PocketArchConfig,
    vb: VarBuilder,
) -> Result<Vec<MimiTransformerLayer>> {
    let mut layers = Vec::with_capacity(arch.mimi_transformer_layers);
    for i in 0..arch.mimi_transformer_layers {
        layers.push(MimiTransformerLayer::load(
            arch.mimi_dim,
            arch.mimi_transformer_heads,
            vb.pp(i),
        )?);
    }
    Ok(layers)
}

#[expect(
    clippy::type_complexity,
    reason = "internal loader tuple for SEANet decoder"
)]
fn load_seanet_decoder(
    arch: &PocketArchConfig,
    dec_vb: VarBuilder,
) -> Result<(
    StreamingCausalConv1d,
    Vec<StreamingCausalConvTranspose1d>,
    Vec<SeaNetResBlock>,
    StreamingCausalConv1d,
    usize,
)> {
    let ratios = &arch.mimi_ratios;
    let num_stages = ratios.len();
    let top_channels = arch.mimi_dim;
    let dec_in_conv = StreamingCausalConv1d::load(
        top_channels,
        top_channels,
        7,
        1,
        1,
        dec_vb.pp("0").pp("conv"),
    )?;
    let mut dec_upsamples = Vec::with_capacity(num_stages);
    let mut dec_res_blocks = Vec::with_capacity(num_stages);
    let mut cur_c = top_channels;
    for (i, &ratio) in ratios.iter().enumerate() {
        let next_c = (cur_c / 2).max(arch.mimi_base_channels);
        let up = StreamingCausalConvTranspose1d::load(
            cur_c,
            next_c,
            ratio * 2,
            ratio,
            dec_vb.pp(2 + i * 3).pp("convtr"),
        )?;
        let res = SeaNetResBlock::load(next_c, dec_vb.pp(3 + i * 3))?;
        dec_upsamples.push(up);
        dec_res_blocks.push(res);
        cur_c = next_c;
    }
    let dec_out_conv =
        StreamingCausalConv1d::load(cur_c, 1, 3, 1, 1, dec_vb.pp(2 + num_stages * 3).pp("conv"))?;
    Ok((
        dec_in_conv,
        dec_upsamples,
        dec_res_blocks,
        dec_out_conv,
        cur_c,
    ))
}

fn load_seanet_encoder(
    arch: &PocketArchConfig,
    base_c: usize,
    enc_vb: VarBuilder,
) -> Result<(
    StreamingCausalConv1d,
    Vec<SeaNetResBlock>,
    Vec<StreamingCausalConv1d>,
    StreamingCausalConv1d,
)> {
    let ratios = &arch.mimi_ratios;
    let num_stages = ratios.len();
    let top_channels = arch.mimi_dim;
    let enc_in_conv = StreamingCausalConv1d::load(1, base_c, 7, 1, 1, enc_vb.pp("0").pp("conv"))?;
    let mut enc_res_blocks = Vec::with_capacity(num_stages);
    let mut enc_downsamples = Vec::with_capacity(num_stages);
    let mut enc_c = base_c;
    for (i, &ratio) in ratios.iter().rev().enumerate() {
        let next_c = (enc_c * 2).min(top_channels);
        let res = SeaNetResBlock::load(enc_c, enc_vb.pp(1 + i * 3))?;
        let down = StreamingCausalConv1d::load(
            enc_c,
            next_c,
            ratio * 2,
            ratio,
            1,
            enc_vb.pp(3 + i * 3).pp("conv"),
        )?;
        enc_res_blocks.push(res);
        enc_downsamples.push(down);
        enc_c = next_c;
    }
    let enc_out_conv = StreamingCausalConv1d::load(
        enc_c,
        top_channels,
        3,
        1,
        1,
        enc_vb.pp(2 + num_stages * 3).pp("conv"),
    )?;
    Ok((enc_in_conv, enc_res_blocks, enc_downsamples, enc_out_conv))
}
