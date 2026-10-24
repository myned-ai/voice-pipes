use super::*;
use candle_core::quantized::{GgmlDType, QTensor};
use gguf_file::Value;

const VOCAB: usize = 16;
const HIDDEN: usize = 8;
const HEADS: u32 = 2;
const KV_HEADS: u32 = 1;
const HEAD_DIM: usize = 4;
const FFN: usize = 16;

/// A deterministic weight tensor of `shape`, small enough that a forward
/// pass stays finite.
fn weight(shape: &[usize], seed: usize) -> QTensor {
    let n: usize = shape.iter().product();
    #[expect(
        clippy::cast_precision_loss,
        reason = "small test indices, exact in f32"
    )]
    let values: Vec<f32> = (0..n)
        .map(|i| ((i + seed) as f32 * 0.37).sin() * 0.1)
        .collect();
    let tensor = Tensor::from_vec(values, shape, &Device::Cpu).expect("weight tensor");
    QTensor::quantize(&tensor, GgmlDType::F32).expect("f32 qtensor")
}

/// A one-layer GGUF of `arch` ("llama" or "gemma3") with the metadata and
/// tensors candle's loader for it reads, in memory.
fn tiny_model(arch: &str) -> Vec<u8> {
    let p = |key: &str| format!("{arch}.{key}");
    let mut metadata: Vec<(String, Value)> = vec![
        ("general.architecture".into(), Value::String(arch.into())),
        (p("attention.head_count"), Value::U32(HEADS)),
        (p("attention.head_count_kv"), Value::U32(KV_HEADS)),
        (p("block_count"), Value::U32(1)),
        (p("embedding_length"), Value::U32(8)),
        (p("attention.layer_norm_rms_epsilon"), Value::F32(1e-6)),
        (p("context_length"), Value::U32(64)),
    ];
    let q_dim = HEADS as usize * HEAD_DIM;
    let kv_dim = KV_HEADS as usize * HEAD_DIM;
    let mut tensors: Vec<(String, QTensor)> = vec![
        ("token_embd.weight".into(), weight(&[VOCAB, HIDDEN], 1)),
        ("output_norm.weight".into(), weight(&[HIDDEN], 2)),
        ("blk.0.attn_q.weight".into(), weight(&[q_dim, HIDDEN], 3)),
        ("blk.0.attn_k.weight".into(), weight(&[kv_dim, HIDDEN], 4)),
        ("blk.0.attn_v.weight".into(), weight(&[kv_dim, HIDDEN], 5)),
        (
            "blk.0.attn_output.weight".into(),
            weight(&[HIDDEN, q_dim], 6),
        ),
        ("blk.0.attn_norm.weight".into(), weight(&[HIDDEN], 7)),
        ("blk.0.ffn_norm.weight".into(), weight(&[HIDDEN], 8)),
        ("blk.0.ffn_gate.weight".into(), weight(&[FFN, HIDDEN], 9)),
        ("blk.0.ffn_up.weight".into(), weight(&[FFN, HIDDEN], 10)),
        ("blk.0.ffn_down.weight".into(), weight(&[HIDDEN, FFN], 11)),
    ];
    if arch == "gemma3" {
        metadata.extend([
            (p("attention.key_length"), Value::U32(4)),
            (p("attention.value_length"), Value::U32(4)),
            (p("attention.sliding_window"), Value::U32(4)),
        ]);
        tensors.extend([
            ("blk.0.attn_q_norm.weight".into(), weight(&[HEAD_DIM], 12)),
            ("blk.0.attn_k_norm.weight".into(), weight(&[HEAD_DIM], 13)),
            (
                "blk.0.post_attention_norm.weight".into(),
                weight(&[HIDDEN], 14),
            ),
            ("blk.0.post_ffw_norm.weight".into(), weight(&[HIDDEN], 15)),
        ]);
    } else {
        metadata.push((p("rope.dimension_count"), Value::U32(4)));
    }
    let metadata: Vec<(&str, &Value)> = metadata.iter().map(|(k, v)| (k.as_str(), v)).collect();
    let tensors: Vec<(&str, &QTensor)> = tensors.iter().map(|(k, t)| (k.as_str(), t)).collect();
    let mut out = std::io::Cursor::new(Vec::new());
    gguf_file::write(&mut out, &metadata, &tensors).expect("write gguf");
    out.into_inner()
}

fn load(arch: &str) -> CandleGgufBackend {
    CandleGgufBackend::from_reader(&mut std::io::Cursor::new(tiny_model(arch)), Device::Cpu)
        .unwrap_or_else(|e| panic!("{arch}: {}", e.message))
}

/// What a backend says about continuing a batch is what candle does: the
/// processor feeds a prefilled prompt's remainder at an offset only when
/// the backend says it can, and a wrong "yes" fails every turn.
#[test]
fn each_architecture_says_truly_whether_it_continues_a_batch() {
    for arch in ["llama", "gemma3"] {
        let mut backend = load(arch);
        backend.forward(&[1, 2, 3], 0).expect("a whole prompt");
        backend.forward(&[4], 3).expect("one continuation token");

        let mut backend = load(arch);
        backend.forward(&[1, 2, 3], 0).expect("a whole prompt");
        let batch_at_offset = backend.forward(&[4, 5], 3);
        assert_eq!(
            backend.continues_batches(),
            batch_at_offset.is_ok(),
            "{arch}: continues_batches() disagrees with a batch at an offset ({:?})",
            batch_at_offset.as_ref().err().map(|e| e.message.clone())
        );

        // A batch that runs must also compute what one token at a time does:
        // a "yes" that ran but gave other logits would answer every turn
        // wrongly instead of failing it.
        if let Ok(batched) = batch_at_offset {
            let mut stepwise = load(arch);
            stepwise.forward(&[1, 2, 3], 0).expect("a whole prompt");
            stepwise.forward(&[4], 3).expect("token 4");
            let one_by_one = stepwise.forward(&[5], 4).expect("token 5");
            let a: Vec<f32> = batched.to_vec1().expect("batched logits");
            let b: Vec<f32> = one_by_one.to_vec1().expect("stepwise logits");
            let worst = a
                .iter()
                .zip(&b)
                .map(|(x, y)| (x - y).abs())
                .fold(0.0_f32, f32::max);
            assert!(
                worst < 1e-3,
                "{arch}: a batch at an offset differs by {worst}"
            );
        }
    }
}

/// The window comes from the file's context length, capped at what candle
/// precomputes.
#[test]
fn the_window_is_the_files_context_length() {
    for arch in ["llama", "gemma3"] {
        assert_eq!(load(arch).context_tokens(), 64, "{arch}");
    }
}
