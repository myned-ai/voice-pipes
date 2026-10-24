//! One way to open an ONNX Runtime session, for every local model.
//!
//! Threads, graph optimisation and the execution provider are applied here,
//! so a process follows one policy whichever provider opens the session.
//! CUDA is used when the provider asks for it and the build has it
//! (`cuda`); then a registration failure is an error rather than a silent
//! fall back to the CPU, so a misconfigured GPU host is loud about it. A
//! build without CUDA runs the session on the CPU and says so.

use std::path::{Path, PathBuf};

use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;

/// The environment variable that selects the execution provider for the
/// providers that follow the process-wide choice
/// ([`ExecutionTarget::from_env`]).
pub const EXECUTION_PROVIDER_VAR: &str = "VP_ORT_EP";

/// Where a session's inference runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum ExecutionTarget {
    /// The CPU, always available.
    #[default]
    Cpu,
    /// NVIDIA CUDA, when the build has the `cuda` feature.
    Cuda,
}

impl ExecutionTarget {
    /// The target `VP_ORT_EP` asks for: `cuda` (any case) is CUDA,
    /// anything else, or nothing, the CPU.
    pub fn from_env() -> Self {
        Self::parse(std::env::var(EXECUTION_PROVIDER_VAR).ok().as_deref())
    }

    /// The target a setting names, as [`from_env`](Self::from_env) reads it.
    pub fn parse(value: Option<&str>) -> Self {
        if value.is_some_and(|v| v.trim().eq_ignore_ascii_case("cuda")) {
            Self::Cuda
        } else {
            Self::Cpu
        }
    }

    /// Where a session asked to run here actually runs in this build, and
    /// why, for a provider's device report: CUDA asked for in a build
    /// without it runs on the CPU.
    pub fn effective(self) -> (Self, Option<&'static str>) {
        match self {
            Self::Cuda if cfg!(feature = "cuda") => (Self::Cuda, None),
            Self::Cuda => (Self::Cpu, Some("CUDA requested but not compiled in")),
            Self::Cpu => (Self::Cpu, None),
        }
    }

    /// What a provider reports as its device for a session asked to run
    /// here: where it runs (`"cpu"` or `"cuda"`) and, when CUDA was asked
    /// for, a detail naming the setting and, in a build without CUDA, why it
    /// runs on the CPU instead.
    pub fn device_report(self) -> (&'static str, Option<String>) {
        let (runs_on, why) = self.effective();
        let detail = match (self, why) {
            (Self::Cpu, _) => None,
            (_, Some(why)) => Some(format!("{EXECUTION_PROVIDER_VAR}=cuda: {why}")),
            (_, None) => Some(format!("{EXECUTION_PROVIDER_VAR}=cuda")),
        };
        (runs_on.as_str(), detail)
    }

    /// `"cpu"` or `"cuda"`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Cuda => "cuda",
        }
    }
}

/// How to open a session.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SessionOptions {
    /// Threads for one operator (`intra_op_num_threads`); 0 leaves the
    /// choice to ONNX Runtime (one per physical core).
    pub intra_threads: usize,
    /// Where it runs.
    pub target: ExecutionTarget,
    /// Graph optimisation, when not ONNX Runtime's default.
    pub optimization: Option<GraphOptimizationLevel>,
    /// Where to save the optimised graph, for a model that is optimised
    /// once and loaded as saved afterwards.
    pub optimized_model_path: Option<PathBuf>,
}

impl SessionOptions {
    /// `intra_threads` threads (0: ONNX Runtime's choice) on the CPU,
    /// ONNX Runtime's default optimisation.
    pub fn new(intra_threads: usize) -> Self {
        Self {
            intra_threads,
            target: ExecutionTarget::Cpu,
            optimization: None,
            optimized_model_path: None,
        }
    }

    /// With the session running on `target`.
    #[must_use]
    pub fn with_target(mut self, target: ExecutionTarget) -> Self {
        self.target = target;
        self
    }

    /// With graph optimisation at `level`.
    #[must_use]
    pub fn with_optimization(mut self, level: GraphOptimizationLevel) -> Self {
        self.optimization = Some(level);
        self
    }

    /// With the optimised graph saved to `path`.
    #[must_use]
    pub fn with_optimized_model_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.optimized_model_path = Some(path.into());
        self
    }
}

/// A session could not be opened. The message names the step and the model
/// file; a provider maps it into its own error type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionOpenError(String);

impl std::fmt::Display for SessionOpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SessionOpenError {}

/// Open the model at `path` as `options` say.
///
/// # Errors
/// When ONNX Runtime refuses a setting or the model, and when the session
/// asks for CUDA on a host where it cannot be registered.
pub fn open_session(path: &Path, options: &SessionOptions) -> Result<Session, SessionOpenError> {
    let mut builder = Session::builder()
        .map_err(failed("session builder", path))?
        .with_intra_threads(options.intra_threads)
        .map_err(failed("session threads", path))?;
    if let Some(level) = options.optimization {
        builder = builder
            .with_optimization_level(level)
            .map_err(failed("graph optimisation", path))?;
    }
    if let Some(saved) = &options.optimized_model_path {
        builder = builder
            .with_optimized_model_path(saved)
            .map_err(failed("optimised model path", path))?;
    }
    match options.target {
        ExecutionTarget::Cpu => {}
        #[cfg(feature = "cuda")]
        ExecutionTarget::Cuda => {
            builder = builder
                .with_execution_providers([cuda_provider()])
                .map_err(failed("CUDA execution provider", path))?;
            tracing::info!(model = %path.display(), "ort: CUDA execution provider registered");
        }
        #[cfg(not(feature = "cuda"))]
        ExecutionTarget::Cuda => {
            tracing::warn!(
                model = %path.display(),
                "ort: CUDA requested but not compiled in; running on the CPU"
            );
        }
    }
    builder
        .commit_from_file(path)
        .map_err(failed("loading the model", path))
}

/// A model's output was not what the code reading it expects: fewer
/// outputs than it reads, or a negative length. The message names what was
/// read; a provider maps it into its own inference error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelOutputError(String);

impl std::fmt::Display for ModelOutputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ModelOutputError {}

/// A tensor dimension the model reported, as a length; `what` names it in
/// the error for a negative one (a dynamic axis ONNX Runtime left unset).
///
/// # Errors
/// When `value` is negative.
pub fn dim(value: i64, what: &str) -> Result<usize, ModelOutputError> {
    usize::try_from(value)
        .map_err(|_negative| ModelOutputError(format!("the model reported {what} of {value}")))
}

/// Output `i` of a session run, from the session `what` names: an export
/// with fewer outputs than the code reads is an error, not a panic.
///
/// # Errors
/// When the run has no output `i`.
pub fn nth_output<'v>(
    outputs: &'v ort::session::SessionOutputs<'_>,
    i: usize,
    what: &str,
) -> Result<ort::value::ValueRef<'v>, ModelOutputError> {
    nth(outputs.values(), i, what)
}

/// Item `i` of a run's outputs, or the error naming the session.
fn nth<T>(
    mut values: impl Iterator<Item = T>,
    i: usize,
    what: &str,
) -> Result<T, ModelOutputError> {
    values
        .nth(i)
        .ok_or_else(|| ModelOutputError(format!("the {what} returned no output {i}")))
}

/// The CUDA execution provider, set to fail rather than fall back:
/// ONNX Runtime's default logs a registration failure and runs the session
/// on the CPU.
#[cfg(any(feature = "cuda", test))]
fn cuda_provider() -> ort::ep::ExecutionProviderDispatch {
    ort::ep::CUDA::default().build().error_on_failure()
}

/// The error for `step` failing on the model at `path`.
fn failed<'a, E: std::fmt::Display>(
    step: &'static str,
    path: &'a Path,
) -> impl FnOnce(E) -> SessionOpenError + 'a {
    move |e| SessionOpenError(format!("{step} for {}: {e}", path.display()))
}

#[cfg(test)]
#[path = "tests/session.rs"]
mod tests;
