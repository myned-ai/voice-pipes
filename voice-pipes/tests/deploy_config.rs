//! The shipped deploy configs must fail closed.
//!
//! Every pipeline and orchestrator TOML under `deploy/docker/config/` is
//! parsed with the real config types and must carry `[auth] required = true`,
//! so a stack started from these files refuses to run any surface without
//! its token. Worker files must still parse (they carry no auth block).

use std::path::{Path, PathBuf};

use voice_pipes::api::types::{OrchestratorConfig, WorkerConfig};
use voice_pipes::config::PipelineConfig;

fn config_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("deploy/docker/config")
}

fn toml_files(prefix: &str) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(config_dir())
        .expect("deploy/docker/config exists")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.extension().is_some_and(|e| e == "toml")
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with(prefix))
        })
        .collect();
    files.sort();
    assert!(
        !files.is_empty(),
        "no {prefix}*.toml under {}",
        config_dir().display()
    );
    files
}

#[test]
fn every_shipped_pipeline_config_requires_auth() {
    for path in toml_files("pipeline-") {
        let text = std::fs::read_to_string(&path).unwrap();
        let cfg: PipelineConfig = toml::from_str(&text)
            .unwrap_or_else(|e| panic!("{} does not parse: {e}", path.display()));
        let auth = cfg
            .auth
            .unwrap_or_else(|| panic!("{} has no [auth] section", path.display()));
        assert!(
            auth.required,
            "{} must set [auth] required = true",
            path.display()
        );
    }
}

#[test]
fn shipped_orchestrator_config_requires_auth() {
    let path = config_dir().join("orchestrator.toml");
    let text = std::fs::read_to_string(&path).unwrap();
    let cfg: OrchestratorConfig = toml::from_str(&text).expect("orchestrator.toml parses");
    assert!(
        cfg.auth.required,
        "orchestrator.toml must set [auth] required = true"
    );
}

#[test]
fn every_shipped_worker_config_parses() {
    for path in toml_files("worker-") {
        let text = std::fs::read_to_string(&path).unwrap();
        let cfg: WorkerConfig = toml::from_str(&text)
            .unwrap_or_else(|e| panic!("{} does not parse: {e}", path.display()));
        // In-container workers must bind all interfaces for the orchestrator
        // to reach them by service name; the host publication is loopback
        // (docker-compose.yml) and the API needs VP_INTERNAL_TOKEN.
        assert_eq!(cfg.debug_bind, "0.0.0.0", "{}", path.display());
        assert!(
            cfg.advertise_address.is_some(),
            "{} needs advertise_address for the compose network",
            path.display()
        );
    }
}
