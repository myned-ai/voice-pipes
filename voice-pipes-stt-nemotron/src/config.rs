//! The settings the English Nemotron reads from its option table, and their defaults.

pub(crate) const DEFAULT_BLANK_ID: i32 = 1024;
pub(crate) const DEFAULT_MAX_TOKENS_PER_STEP: usize = 8;
pub(crate) const DEFAULT_THREADS: i16 = 4;
/// Session sets loaded per provider: one per concurrent session served.
pub(crate) const DEFAULT_POOL_SIZE: usize = 2;

/// The settings the English Nemotron reads from its option table.
pub(crate) struct NemotronOptions {
    pub(crate) threads: i16,
    pub(crate) blank_id: i32,
    pub(crate) max_tokens_per_step: usize,
    pub(crate) pool_size: usize,
}

impl NemotronOptions {
    pub(crate) fn read(
        cfg: &voice_pipes_sdk::SttConfig,
    ) -> Result<Self, Vec<voice_pipes_sdk::ConfigError>> {
        let mut o = cfg.options();
        let options = Self {
            // 0 leaves the count to ONNX Runtime.
            threads: i16::try_from(o.u64_in(
                "threads",
                0..=256,
                DEFAULT_THREADS.unsigned_abs().into(),
            ))
            .unwrap_or(DEFAULT_THREADS),
            blank_id: i32::try_from(o.u64_in(
                "blank_id",
                0..=1_000_000,
                DEFAULT_BLANK_ID.unsigned_abs().into(),
            ))
            .unwrap_or(DEFAULT_BLANK_ID),
            max_tokens_per_step: o.usize_in(
                "max_tokens_per_step",
                1..=100,
                DEFAULT_MAX_TOKENS_PER_STEP,
            ),
            pool_size: o.usize_in("pool_size", 1..=64, DEFAULT_POOL_SIZE),
        };
        o.finish()?;
        Ok(options)
    }
}
