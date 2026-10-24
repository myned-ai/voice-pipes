use super::*;

/// `VP_ORT_EP` selects CUDA only by name, in any case and with stray
/// whitespace; anything else, or nothing, is the CPU.
#[test]
fn only_cuda_by_name_selects_cuda() {
    for v in ["cuda", "CUDA", " Cuda "] {
        assert_eq!(
            ExecutionTarget::parse(Some(v)),
            ExecutionTarget::Cuda,
            "{v}"
        );
    }
    for v in [None, Some(""), Some("cpu"), Some("tensorrt"), Some("cuda0")] {
        assert_eq!(ExecutionTarget::parse(v), ExecutionTarget::Cpu, "{v:?}");
    }
}

/// A build without CUDA reports a CUDA request as running on the CPU, with
/// the reason; the CPU is always the CPU.
#[test]
fn the_effective_target_follows_the_build() {
    assert_eq!(
        ExecutionTarget::Cpu.effective(),
        (ExecutionTarget::Cpu, None)
    );
    let (target, why) = ExecutionTarget::Cuda.effective();
    if cfg!(feature = "cuda") {
        assert_eq!((target, why), (ExecutionTarget::Cuda, None));
    } else {
        assert_eq!(target, ExecutionTarget::Cpu);
        assert!(why.is_some_and(|w| w.contains("not compiled")));
    }
}

/// The device report names where the session runs, and says nothing more
/// for the CPU; asked for CUDA it names the setting, and in a build without
/// CUDA why it is the CPU.
#[test]
fn the_device_report_says_where_and_why() {
    assert_eq!(ExecutionTarget::Cpu.device_report(), ("cpu", None));
    let (runs_on, detail) = ExecutionTarget::Cuda.device_report();
    let detail = detail.expect("CUDA asked for is named");
    assert!(detail.starts_with("VP_ORT_EP=cuda"), "{detail}");
    if cfg!(feature = "cuda") {
        assert_eq!(runs_on, "cuda");
    } else {
        assert_eq!(runs_on, "cpu");
        assert!(detail.contains("not compiled"), "{detail}");
    }
}

/// A length the model reported is taken as it is; a negative one is an
/// error naming what was read and the value.
#[test]
fn a_negative_dimension_is_an_error_naming_it() {
    assert_eq!(dim(0, "mel bins"), Ok(0));
    assert_eq!(dim(128, "mel bins"), Ok(128));
    let err = dim(-1, "mel bins").expect_err("negative");
    assert_eq!(err.to_string(), "the model reported mel bins of -1");
}

/// The output a run has is returned; one past the last is an error naming
/// the session and the index, not a panic.
#[test]
fn a_missing_output_is_an_error_naming_the_session() {
    let outputs = ["logits", "state"];
    assert_eq!(nth(outputs.iter(), 1, "decoder"), Ok(&"state"));
    let err = nth(outputs.iter(), 2, "decoder").expect_err("only two outputs");
    assert_eq!(err.to_string(), "the decoder returned no output 2");
}

/// A model that cannot be loaded is an error naming the file, not a panic.
#[test]
fn a_missing_model_is_an_error_naming_it() {
    let path = std::env::temp_dir().join("vp-onnx-no-such-model.onnx");
    let err = open_session(&path, &SessionOptions::new(1)).expect_err("no model there");
    assert!(
        err.to_string().contains("vp-onnx-no-such-model.onnx"),
        "{err}"
    );
}

/// Registering CUDA where ONNX Runtime cannot run it is an error naming
/// the provider. ONNX Runtime's default logs the failure and runs the
/// session on the CPU, so a GPU host with a broken CUDA install would run
/// every model on the CPU while reporting CUDA. This build has no CUDA,
/// which is exactly such a host.
#[cfg(not(feature = "cuda"))]
#[test]
fn a_cuda_provider_that_cannot_register_is_an_error() {
    let Err(err) = Session::builder()
        .expect("session builder")
        .with_execution_providers([cuda_provider()])
    else {
        panic!("CUDA registered in a build without it: the failure fell back to the CPU");
    };
    // The session opener names the step and the model around ort's message.
    let named = failed("CUDA execution provider", std::path::Path::new("m.onnx"))(err);
    assert!(
        named
            .to_string()
            .starts_with("CUDA execution provider for m.onnx: "),
        "{named}"
    );
}
