//! The factory the binary registers for `[turn_detection] mode = "vap"`.

use std::path::Path;

use voice_pipes_sdk::ConfigError;
use voice_pipes_sdk::turn::{TurnConfig, TurnDetector, TurnDetectorFactory};

use crate::config::{MIN_INTERVAL_MS, POOL_SIZE_RANGE, THREADS_RANGE, VapSettings};
use crate::detector::VapTurnDetector;

/// The factory the binary registers.
pub fn factory() -> Box<dyn TurnDetectorFactory> {
    Box::new(VapTurnFactory)
}

/// Factory for the VAP turn detector.
#[derive(Debug)]
pub(crate) struct VapTurnFactory;

impl TurnDetectorFactory for VapTurnFactory {
    fn name(&self) -> &'static str {
        "vap"
    }

    fn validate(&self, cfg: &TurnConfig) -> Vec<ConfigError> {
        VapSettings::parse(cfg).err().unwrap_or_default()
    }

    fn build(
        &self,
        cfg: &TurnConfig,
        _ctx: &voice_pipes_sdk::BuildContext<'_>,
    ) -> Result<Box<dyn TurnDetector>, voice_pipes_sdk::BuildError> {
        voice_pipes_sdk::options::validated(self.validate(cfg))?;
        let s = VapSettings::parse(cfg).map_err(|errors| {
            let messages: Vec<String> = errors
                .iter()
                .map(|e| format!("{}: {}", e.field, e.message))
                .collect();
            anyhow::anyhow!("{}", messages.join("; "))
        })?;
        Ok(Box::new(VapTurnDetector::with_pool_config(
            Path::new(&s.model_path),
            s.pool_size,
            s.threads,
            s.threshold,
            s.min_interval_ms,
            cfg.silence_timeout_ms,
        )?))
    }

    fn param_schema(&self) -> Vec<voice_pipes_sdk::schema::ParamDescriptor> {
        use voice_pipes_sdk::schema::ParamDescriptor;
        // Read once when the detector is built: shown, changed by a restart.
        vec![
            ParamDescriptor::static_path(
                "vap_model_path",
                "VAP model path",
                "Path to the VAP ONNX export (scripts/export-vap-onnx.py). Runs on the CPU whatever VP_ORT_EP says.",
            ),
            ParamDescriptor::mutable_float(
                "vap_threshold",
                "VAP threshold",
                "Predicted probability that the user yields the turn above which it ends (range 0.0-1.0, default 0.5).",
                0.0,
                1.0,
            )
            .restart_required(),
            ParamDescriptor::mutable_int(
                "vap_min_interval_ms",
                "VAP min interval (ms)",
                "Shortest time between two model runs (default 200 ms).",
                0,
                i64::try_from(*MIN_INTERVAL_MS.end()).unwrap_or(i64::MAX),
            )
            .restart_required(),
            ParamDescriptor::mutable_int(
                "vap_pool_size",
                "VAP pool size",
                "Shared ONNX session pool size across concurrent WebSocket sessions (default 2).",
                i64::try_from(*POOL_SIZE_RANGE.start()).unwrap_or(1),
                i64::try_from(*POOL_SIZE_RANGE.end()).unwrap_or(128),
            )
            .restart_required(),
            ParamDescriptor::mutable_int(
                "vap_threads",
                "VAP threads",
                "Intra-op threads per VAP ONNX session (default 2, or VP_ORT_THREADS).",
                i64::try_from(*THREADS_RANGE.start()).unwrap_or(0),
                i64::try_from(*THREADS_RANGE.end()).unwrap_or(256),
            )
            .restart_required(),
        ]
    }
}
