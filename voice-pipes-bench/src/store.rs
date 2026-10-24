//! File-based storage for benchmark experiments, test suites, and results.
//!
//! Layout on disk:
//! ```text
//! benchmarks/
//!   experiments/
//!     {experiment_id}/
//!       experiment.json
//!       {variant_id}.toml          # frozen config snapshot
//!   suites/
//!     {suite_id}/
//!       suite.json
//!       scenarios/
//!         {scenario}.wav
//!   results/
//!     {experiment_id}/
//!       results.json
//! ```

use std::path::{Path, PathBuf};

use tokio::fs;
use tracing::{debug, warn};

use super::types::*;

/// Reject IDs that could escape the store root via path traversal.
/// IDs are used verbatim as directory/file-name segments, so only a
/// conservative slug charset is allowed — this rejects `..`, `/`, `\`, `:`,
/// absolute paths, and anything else that isn't `[A-Za-z0-9_-]`.
fn validate_id(kind: &str, id: &str) -> anyhow::Result<()> {
    if !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "invalid {kind} id {id:?}: only non-empty [A-Za-z0-9_-] is allowed"
        ))
    }
}

/// File-backed benchmark store.
#[derive(Debug, Clone)]
pub struct BenchmarkStore {
    root: PathBuf,
}

impl BenchmarkStore {
    /// Create a new store rooted at the given directory.
    /// Directories are created lazily on first write.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    // ── Experiments ─────────────────────────────────────────

    fn experiments_dir(&self) -> PathBuf {
        self.root.join("experiments")
    }

    fn experiment_dir(&self, id: &str) -> anyhow::Result<PathBuf> {
        validate_id("experiment", id)?;
        Ok(self.experiments_dir().join(id))
    }

    fn experiment_json(&self, id: &str) -> anyhow::Result<PathBuf> {
        Ok(self.experiment_dir(id)?.join("experiment.json"))
    }

    /// Persist a new or updated experiment.
    pub async fn save_experiment(&self, exp: &Experiment) -> anyhow::Result<()> {
        let dir = self.experiment_dir(&exp.id)?;
        fs::create_dir_all(&dir).await?;
        let json = serde_json::to_string_pretty(exp)?;
        fs::write(self.experiment_json(&exp.id)?, json).await?;
        debug!(experiment_id = %exp.id, "saved experiment");
        Ok(())
    }

    /// Load an experiment by ID.
    pub async fn load_experiment(&self, id: &str) -> anyhow::Result<Option<Experiment>> {
        let path = self.experiment_json(id)?;
        if !path.exists() {
            return Ok(None);
        }
        let data = fs::read_to_string(&path).await?;
        let exp: Experiment = serde_json::from_str(&data)?;
        Ok(Some(exp))
    }

    /// List all experiment IDs and metadata.
    pub async fn list_experiments(&self) -> anyhow::Result<Vec<Experiment>> {
        let dir = self.experiments_dir();
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut experiments = Vec::new();
        let mut entries = fs::read_dir(&dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            if !entry.file_type().await?.is_dir() {
                continue;
            }
            let id = entry.file_name().to_string_lossy().to_string();
            match self.load_experiment(&id).await {
                Ok(Some(exp)) => experiments.push(exp),
                Ok(None) => {}
                Err(e) => {
                    warn!(experiment_id = %id, error = %e, "failed to load experiment");
                }
            }
        }
        experiments.sort_by_key(|b| std::cmp::Reverse(b.created_at));
        Ok(experiments)
    }

    /// Delete an experiment and all associated files.
    pub async fn delete_experiment(&self, id: &str) -> anyhow::Result<bool> {
        let dir = self.experiment_dir(id)?;
        if !dir.exists() {
            return Ok(false);
        }
        fs::remove_dir_all(&dir).await?;
        // Also remove results if they exist.
        let results_dir = self.results_dir(id)?;
        if results_dir.exists() {
            fs::remove_dir_all(&results_dir).await?;
        }
        debug!(experiment_id = %id, "deleted experiment");
        Ok(true)
    }

    /// Save a frozen config TOML snapshot for a variant.
    pub async fn save_variant_config(
        &self,
        experiment_id: &str,
        variant_id: &str,
        toml_content: &str,
    ) -> anyhow::Result<PathBuf> {
        validate_id("variant", variant_id)?;
        let dir = self.experiment_dir(experiment_id)?;
        fs::create_dir_all(&dir).await?;
        let path = dir.join(format!("{variant_id}.toml"));
        fs::write(&path, toml_content).await?;
        Ok(path)
    }

    /// Load a frozen config TOML snapshot for a variant.
    pub async fn load_variant_config(
        &self,
        experiment_id: &str,
        variant_id: &str,
    ) -> anyhow::Result<Option<String>> {
        validate_id("variant", variant_id)?;
        let path = self
            .experiment_dir(experiment_id)?
            .join(format!("{variant_id}.toml"));
        if !path.exists() {
            return Ok(None);
        }
        let content = fs::read_to_string(&path).await?;
        Ok(Some(content))
    }

    // ── Test Suites ─────────────────────────────────────────

    fn suites_dir(&self) -> PathBuf {
        self.root.join("suites")
    }

    fn suite_dir(&self, id: &str) -> anyhow::Result<PathBuf> {
        validate_id("suite", id)?;
        Ok(self.suites_dir().join(id))
    }

    fn suite_json(&self, id: &str) -> anyhow::Result<PathBuf> {
        Ok(self.suite_dir(id)?.join("suite.json"))
    }

    /// Persist a new or updated test suite.
    pub async fn save_suite(&self, suite: &TestSuite) -> anyhow::Result<()> {
        let dir = self.suite_dir(&suite.id)?;
        fs::create_dir_all(dir.join("scenarios")).await?;
        let json = serde_json::to_string_pretty(suite)?;
        fs::write(self.suite_json(&suite.id)?, json).await?;
        debug!(suite_id = %suite.id, "saved test suite");
        Ok(())
    }

    /// Load a test suite by ID.
    pub async fn load_suite(&self, id: &str) -> anyhow::Result<Option<TestSuite>> {
        let path = self.suite_json(id)?;
        if !path.exists() {
            return Ok(None);
        }
        let data = fs::read_to_string(&path).await?;
        let suite: TestSuite = serde_json::from_str(&data)?;
        Ok(Some(suite))
    }

    /// List all test suites.
    pub async fn list_suites(&self) -> anyhow::Result<Vec<TestSuite>> {
        let dir = self.suites_dir();
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut suites = Vec::new();
        let mut entries = fs::read_dir(&dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            if !entry.file_type().await?.is_dir() {
                continue;
            }
            let id = entry.file_name().to_string_lossy().to_string();
            match self.load_suite(&id).await {
                Ok(Some(suite)) => suites.push(suite),
                Ok(None) => {}
                Err(e) => {
                    warn!(suite_id = %id, error = %e, "failed to load test suite");
                }
            }
        }
        suites.sort_by_key(|b| std::cmp::Reverse(b.created_at));
        Ok(suites)
    }

    /// Delete a test suite and all scenario files.
    pub async fn delete_suite(&self, id: &str) -> anyhow::Result<bool> {
        let dir = self.suite_dir(id)?;
        if !dir.exists() {
            return Ok(false);
        }
        fs::remove_dir_all(&dir).await?;
        debug!(suite_id = %id, "deleted test suite");
        Ok(true)
    }

    /// Path where a scenario's audio file should be stored.
    pub fn scenario_audio_path(
        &self,
        suite_id: &str,
        scenario_id: &str,
    ) -> anyhow::Result<PathBuf> {
        validate_id("scenario", scenario_id)?;
        Ok(self
            .suite_dir(suite_id)?
            .join("scenarios")
            .join(format!("{scenario_id}.wav")))
    }

    /// Save audio data for a scenario.
    pub async fn save_scenario_audio(
        &self,
        suite_id: &str,
        scenario_id: &str,
        audio_data: &[u8],
    ) -> anyhow::Result<PathBuf> {
        let path = self.scenario_audio_path(suite_id, scenario_id)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
        }
        fs::write(&path, audio_data).await?;
        debug!(suite_id = %suite_id, scenario_id = %scenario_id, "saved scenario audio");
        Ok(path)
    }

    // ── Results ─────────────────────────────────────────────

    fn results_dir(&self, experiment_id: &str) -> anyhow::Result<PathBuf> {
        validate_id("experiment", experiment_id)?;
        Ok(self.root.join("results").join(experiment_id))
    }

    fn results_json(&self, experiment_id: &str) -> anyhow::Result<PathBuf> {
        Ok(self.results_dir(experiment_id)?.join("results.json"))
    }

    /// Persist experiment results.
    pub async fn save_results(&self, results: &ExperimentResults) -> anyhow::Result<()> {
        let dir = self.results_dir(&results.experiment_id)?;
        fs::create_dir_all(&dir).await?;
        let json = serde_json::to_string_pretty(results)?;
        fs::write(self.results_json(&results.experiment_id)?, json).await?;
        debug!(experiment_id = %results.experiment_id, "saved experiment results");
        Ok(())
    }

    /// Load experiment results.
    pub async fn load_results(
        &self,
        experiment_id: &str,
    ) -> anyhow::Result<Option<ExperimentResults>> {
        let path = self.results_json(experiment_id)?;
        if !path.exists() {
            return Ok(None);
        }
        let data = fs::read_to_string(&path).await?;
        let results: ExperimentResults = serde_json::from_str(&data)?;
        Ok(Some(results))
    }

    /// Returns the root path of this store (for use in configuration).
    pub fn root(&self) -> &Path {
        &self.root
    }
}

#[cfg(test)]
#[path = "tests/store.rs"]
mod tests;
