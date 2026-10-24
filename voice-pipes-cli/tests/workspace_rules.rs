//! The workspace's structural rules, read from the manifests and the
//! sources.
//!
//! The engine stays a runtime an adopter can depend on without taking any
//! provider: providers depend only on the SDK and the shared infrastructure
//! crates, never on the engine or on each other, and never contain `unsafe`.
//! Only this crate, the composition root, sees both sides.
//!
//! Each rule is a function from what it reads to the problems it finds, and
//! has a test that hands it a broken input and sees it reported: a rule that
//! cannot fail holds nothing. Sources are read as Rust syntax, not lines, so
//! a comment, a string or a line break cannot hide or fake what a rule looks
//! for.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use syn::visit::Visit;

/// Crates a provider may depend on inside the workspace.
const INFRA: &[&str] = &["voice-pipes-sdk", "voice-pipes-onnx", "voice-pipes-net"];

/// Prefixes that make a member a provider crate.
const PROVIDER_PREFIXES: &[&str] = &[
    "voice-pipes-stt-",
    "voice-pipes-tts-",
    "voice-pipes-llm-",
    "voice-pipes-vad-",
    "voice-pipes-turn-",
    "voice-pipes-s2s-",
];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .to_path_buf()
}

fn manifest(dir: &str) -> toml::Table {
    let path = root().join(dir).join("Cargo.toml");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    text.parse()
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn members() -> Vec<String> {
    manifest(".")["workspace"]["members"]
        .as_array()
        .expect("workspace.members")
        .iter()
        .map(|m| m.as_str().expect("member path").to_string())
        .collect()
}

fn providers() -> Vec<String> {
    let providers: Vec<String> = members().into_iter().filter(|m| is_provider(m)).collect();
    assert!(!providers.is_empty(), "no provider crates found");
    providers
}

fn is_provider(name: &str) -> bool {
    PROVIDER_PREFIXES.iter().any(|p| name.starts_with(p))
}

fn toml(text: &str) -> toml::Table {
    text.parse().expect("test manifest")
}

/// Assert that a rule found nothing.
fn assert_holds(problems: &[String]) {
    assert!(problems.is_empty(), "{problems:#?}");
}

/// Assert that a rule reported `broken`, and says why in words containing
/// `why`.
fn assert_reports(problems: &[String], why: &str) {
    assert!(
        problems.iter().any(|p| p.contains(why)),
        "expected a problem mentioning {why:?}, got {problems:#?}"
    );
}

// ── Manifests ───────────────────────────────────────────────

/// Every crate a manifest depends on in the given sections, by package name:
/// a dependency renamed with `package = "..."` counts as the package it is,
/// and `[target.'cfg(...)'.<section>]` tables count as well as the plain ones.
fn deps(m: &toml::Table, sections: &[&str]) -> BTreeSet<String> {
    let targets = m
        .get("target")
        .and_then(toml::Value::as_table)
        .into_iter()
        .flat_map(|t| t.values().filter_map(toml::Value::as_table));
    std::iter::once(m)
        .chain(targets)
        .flat_map(|table| sections.iter().filter_map(|s| table.get(*s)))
        .filter_map(toml::Value::as_table)
        .flat_map(|t| {
            t.iter().map(|(key, spec)| {
                spec.get("package")
                    .and_then(toml::Value::as_str)
                    .unwrap_or(key)
                    .to_string()
            })
        })
        .collect()
}

/// Workspace members a manifest depends on, in the given sections.
fn workspace_deps(m: &toml::Table, sections: &[&str], members: &[String]) -> BTreeSet<String> {
    deps(m, sections)
        .into_iter()
        .filter(|d| members.contains(d))
        .collect()
}

/// Network clients a provider must not use directly: it gets them from
/// `voice-pipes-net` (which re-exports `reqwest` and `tokio-tungstenite`), so
/// the TLS stack, timeouts, redirect policy and message limits are the ones
/// decided there. A gRPC provider uses `tonic`, which is not on the list; it
/// runs the net crate's endpoint check itself.
const NETWORK_CLIENTS: &[&str] = &[
    "reqwest",
    "tokio-tungstenite",
    "tungstenite",
    "hyper",
    "hyper-util",
    "ureq",
    "isahc",
    "surf",
    "attohttpc",
];

const ALL_SECTIONS: &[&str] = &["dependencies", "build-dependencies", "dev-dependencies"];

/// A provider depends on the SDK and the shared infrastructure only: not on
/// the engine (which would make every adopter of the provider take the
/// runtime's whole graph) and not on another provider.
fn provider_dependency_problems(p: &str, m: &toml::Table, members: &[String]) -> Vec<String> {
    workspace_deps(m, ALL_SECTIONS, members)
        .iter()
        .filter(|d| !INFRA.contains(&d.as_str()))
        .map(|d| format!("{p} depends on {d}; providers may use only {INFRA:?}"))
        .collect()
}

#[test]
fn providers_depend_only_on_the_sdk_and_shared_infrastructure() {
    let members = members();
    for p in providers() {
        assert_holds(&provider_dependency_problems(&p, &manifest(&p), &members));
    }
}

/// A provider reaches the network only through `voice-pipes-net`: no direct
/// HTTP or WebSocket client, in any section that ships, under any name.
fn network_client_problems(p: &str, m: &toml::Table) -> Vec<String> {
    deps(m, &["dependencies", "build-dependencies"])
        .into_iter()
        .filter(|d| NETWORK_CLIENTS.contains(&d.as_str()))
        .map(|d| format!("{p} depends on {d} directly; take it from voice-pipes-net"))
        .collect()
}

#[test]
fn providers_reach_the_network_only_through_the_net_crate() {
    for p in providers() {
        assert_holds(&network_client_problems(&p, &manifest(&p)));
    }
}

/// Every pre-release requirement in a manifest is exact. Cargo reads
/// `"2.0.0-rc.12"` as "this or any later pre-release", and release
/// candidates break their API between one another, so a resolve without the
/// lock file (`cargo install` without `--locked`) can pick one that does not
/// compile. Covers the workspace table, every section, and target tables.
fn pre_release_pin_problems(name: &str, m: &toml::Table) -> Vec<String> {
    let mut tables: Vec<&toml::Table> = vec![m];
    if let Some(ws) = m.get("workspace").and_then(toml::Value::as_table) {
        tables.push(ws);
    }
    if let Some(targets) = m.get("target").and_then(toml::Value::as_table) {
        tables.extend(targets.values().filter_map(toml::Value::as_table));
    }
    let mut problems = Vec::new();
    for table in tables {
        for section in ALL_SECTIONS {
            let Some(specs) = table.get(*section).and_then(toml::Value::as_table) else {
                continue;
            };
            for (dep, spec) in specs {
                let req = spec
                    .as_str()
                    .or_else(|| spec.get("version").and_then(toml::Value::as_str));
                let Some(req) = req else { continue };
                let loose = req
                    .split(',')
                    .map(str::trim)
                    .any(|c| c.contains('-') && !c.starts_with('='));
                if loose {
                    problems.push(format!(
                        "{name}: {dep} = \"{req}\" is a pre-release that is not pinned; write \"={}\"",
                        req.trim().trim_start_matches(['^', '~'])
                    ));
                }
            }
        }
    }
    problems
}

#[test]
fn pre_release_dependencies_are_pinned_exactly() {
    assert_holds(&pre_release_pin_problems("Cargo.toml", &manifest(".")));
    for member in members() {
        assert_holds(&pre_release_pin_problems(&member, &manifest(&member)));
    }
}

#[test]
fn the_pre_release_rule_catches_a_loose_candidate_anywhere() {
    for broken in [
        "[workspace.dependencies]\nort = { version = \"2.0.0-rc.12\" }",
        "[dependencies]\nort = \"2.0.0-rc.12\"",
        "[dependencies]\nort = \"^2.0.0-rc.12\"",
        "[dev-dependencies]\nfoo = { version = \">=1.0.0-beta.1, <2\" }",
        "[target.'cfg(windows)'.build-dependencies]\nfoo = \"1.0.0-alpha\"",
    ] {
        assert_reports(&pre_release_pin_problems("x", &toml(broken)), "not pinned");
    }
    for fine in [
        "[workspace.dependencies]\nort = { version = \"=2.0.0-rc.12\", default-features = false }",
        "[dependencies]\nserde = \"1.0\"",
        "[dependencies]\nort.workspace = true",
        "[dependencies]\nvoice-pipes-sdk = { path = \"../voice-pipes-sdk\", version = \"0.4\" }",
    ] {
        assert_holds(&pre_release_pin_problems("x", &toml(fine)));
    }
}

/// The features of `krate` a manifest switches on: in its dependency specs
/// (every section and target, the workspace table included, under any
/// name) and in its feature lists (`krate/feature`, `krate?/feature`).
fn features_enabled_for(m: &toml::Table, krate: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut tables: Vec<&toml::Table> = vec![m];
    if let Some(ws) = m.get("workspace").and_then(toml::Value::as_table) {
        tables.push(ws);
    }
    if let Some(targets) = m.get("target").and_then(toml::Value::as_table) {
        tables.extend(targets.values().filter_map(toml::Value::as_table));
    }
    for table in tables {
        for section in ALL_SECTIONS.iter().copied().chain(["dependencies"]) {
            let Some(specs) = table.get(section).and_then(toml::Value::as_table) else {
                continue;
            };
            for (key, spec) in specs {
                let name = spec
                    .get("package")
                    .and_then(toml::Value::as_str)
                    .unwrap_or(key);
                if name == krate
                    && let Some(features) = spec.get("features")
                {
                    found.extend(strings(features));
                }
            }
        }
    }
    if let Some(features) = m.get("features").and_then(toml::Value::as_table) {
        for entry in features.values().flat_map(strings) {
            if let Some((c, f)) = entry.split_once("?/").or_else(|| entry.split_once('/'))
                && c == krate
            {
                found.insert(f.to_string());
            }
        }
    }
    found
}

/// No crate turns on reqwest's blocking client: a blocking call on the
/// runtime stalls every task on its thread, and the client it builds is
/// outside `voice_pipes_net::http_client`'s timeouts and redirect policy.
fn blocking_http_problem(member: &str, m: &toml::Table) -> Option<String> {
    features_enabled_for(m, "reqwest")
        .contains("blocking")
        .then(|| format!("{member} enables reqwest's `blocking` feature"))
}

#[test]
fn no_crate_enables_the_blocking_http_client() {
    assert_eq!(blocking_http_problem(".", &manifest(".")), None);
    for member in members() {
        assert_eq!(blocking_http_problem(&member, &manifest(&member)), None);
    }
}

#[test]
fn the_blocking_rule_sees_every_way_to_enable_it() {
    for m in [
        "[dependencies]\nreqwest = { version = \"0.12\", features = [\"json\", \"blocking\"] }",
        "[workspace.dependencies]\nreqwest = { version = \"0.12\", features = [\"blocking\"] }",
        "[dev-dependencies]\nhttp = { package = \"reqwest\", version = \"0.12\", features = [\"blocking\"] }",
        "[target.'cfg(unix)'.dependencies]\nreqwest = { version = \"0.12\", features = [\"blocking\"] }",
        "[features]\nsync = [\"reqwest/blocking\"]",
        "[features]\nsync = [\"reqwest?/blocking\"]",
    ] {
        assert!(blocking_http_problem("x", &toml(m)).is_some(), "{m}");
    }
    assert!(
        blocking_http_problem(
            "x",
            &toml("[dependencies]\nreqwest = { version = \"0.12\", features = [\"json\"] }")
        )
        .is_none()
    );
}

/// The dependency rules see through renames and target-specific tables, so
/// neither can hide a dependency from them.
#[test]
fn the_dependency_rules_catch_a_renamed_or_target_specific_dependency() {
    let members = ["voice-pipes".to_string(), "voice-pipes-sdk".to_string()];
    let m = toml(
        r#"
        [dependencies]
        engine = { package = "voice-pipes", path = "../voice-pipes" }
        sdk = { package = "voice-pipes-sdk", path = "../voice-pipes-sdk" }
        [target.'cfg(unix)'.dependencies]
        reqwest = "0.12"
    "#,
    );
    let found = deps(&m, &["dependencies"]);
    assert!(found.contains("voice-pipes"), "{found:?}");
    assert!(!found.contains("engine"), "{found:?}");
    assert_reports(
        &provider_dependency_problems("voice-pipes-tts-x", &m, &members),
        "depends on voice-pipes;",
    );
    assert_reports(&network_client_problems("voice-pipes-tts-x", &m), "reqwest");
}

/// The engine names no provider, and the contracts and infrastructure crates
/// depend on nothing above them: the SDK and the ONNX crate on no workspace
/// crate, the net crate on the SDK only.
fn layering_problems(manifests: &BTreeMap<&str, toml::Table>, members: &[String]) -> Vec<String> {
    let mut problems = Vec::new();
    if let Some(engine) = manifests.get("voice-pipes") {
        for d in workspace_deps(engine, ALL_SECTIONS, members) {
            if is_provider(&d) {
                problems.push(format!("the engine depends on provider {d}"));
            }
        }
    }
    for (base, allowed) in [
        ("voice-pipes-sdk", &[][..]),
        ("voice-pipes-onnx", &[][..]),
        ("voice-pipes-net", &["voice-pipes-sdk"][..]),
    ] {
        let Some(m) = manifests.get(base) else {
            continue;
        };
        for d in workspace_deps(m, &["dependencies", "build-dependencies"], members) {
            if !allowed.contains(&d.as_str()) {
                problems.push(format!("{base} depends on workspace crate {d}"));
            }
        }
    }
    problems
}

#[test]
fn the_engine_and_the_contracts_depend_on_no_provider() {
    let members = members();
    let manifests: BTreeMap<&str, toml::Table> = [
        "voice-pipes",
        "voice-pipes-sdk",
        "voice-pipes-onnx",
        "voice-pipes-net",
    ]
    .into_iter()
    .filter(|m| members.iter().any(|x| x == m))
    .map(|m| (m, manifest(m)))
    .collect();
    assert_eq!(
        manifests.len(),
        4,
        "the engine and the base crates are members"
    );
    assert_holds(&layering_problems(&manifests, &members));
}

#[test]
fn the_layering_rule_catches_each_upward_dependency() {
    let members: Vec<String> = ["voice-pipes", "voice-pipes-sdk", "voice-pipes-tts-x"]
        .map(String::from)
        .to_vec();
    let mut manifests = BTreeMap::new();
    manifests.insert(
        "voice-pipes",
        toml("[dev-dependencies]\nvoice-pipes-tts-x = { path = \"x\" }"),
    );
    manifests.insert(
        "voice-pipes-net",
        toml("[dependencies]\nvoice-pipes = { path = \"e\" }"),
    );
    let problems = layering_problems(&manifests, &members);
    assert_reports(
        &problems,
        "the engine depends on provider voice-pipes-tts-x",
    );
    assert_reports(
        &problems,
        "voice-pipes-net depends on workspace crate voice-pipes",
    );
}

/// Every crate is held to the workspace's lint policy, so code moved out of
/// the engine keeps the checks it had there.
fn lint_policy_problem(member: &str, m: &toml::Table) -> Option<String> {
    let inherits = m
        .get("lints")
        .and_then(|l| l.get("workspace"))
        .and_then(toml::Value::as_bool)
        == Some(true);
    (!inherits).then(|| format!("{member}/Cargo.toml must set `[lints] workspace = true`"))
}

#[test]
fn every_member_uses_the_workspace_lints() {
    for member in members() {
        assert_eq!(lint_policy_problem(&member, &manifest(&member)), None);
    }
}

#[test]
fn the_lint_rule_catches_a_member_without_the_policy() {
    assert!(lint_policy_problem("x", &toml("[lints]\nworkspace = false")).is_some());
    assert!(lint_policy_problem("x", &toml("[package]\nname = \"x\"")).is_some());
    assert!(lint_policy_problem("x", &toml("[lints]\nworkspace = true")).is_none());
}

/// A provider with its own `ort-cuda` feature runs on the CPU unless the
/// binary switches it on, so this crate's `ort-cuda` forwards to every such
/// provider it can link (`dep?/feature` enables nothing not already linked).
fn cuda_forwarding_problems(
    cli: &toml::Table,
    providers: &BTreeMap<String, toml::Table>,
) -> Vec<String> {
    let forwarded: BTreeSet<String> = cli
        .get("features")
        .and_then(|f| f.get("ort-cuda"))
        .map(strings)
        .unwrap_or_default()
        .into_iter()
        .collect();
    let linked = cli
        .get("dependencies")
        .and_then(toml::Value::as_table)
        .map(|t| t.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    linked
        .iter()
        .filter_map(|dep| providers.get(dep).map(|m| (dep, m)))
        .filter(|(_, m)| has_feature(m, "ort-cuda"))
        .filter_map(|(dep, _)| {
            let want = format!("{dep}?/ort-cuda");
            (!forwarded.contains(&want)).then(|| {
                format!("{dep} declares ort-cuda but voice-pipes-cli's ort-cuda does not forward `{want}`")
            })
        })
        .collect()
}

fn has_feature(m: &toml::Table, feature: &str) -> bool {
    m.get("features")
        .and_then(toml::Value::as_table)
        .is_some_and(|f| f.contains_key(feature))
}

#[test]
fn ort_cuda_reaches_every_onnx_provider() {
    let providers: BTreeMap<String, toml::Table> = providers()
        .into_iter()
        .map(|p| {
            let m = manifest(&p);
            (p, m)
        })
        .collect();
    let with_cuda = providers
        .values()
        .filter(|m| has_feature(m, "ort-cuda"))
        .count();
    assert!(
        with_cuda >= 2,
        "expected the ONNX providers to be found, found {with_cuda}"
    );
    assert_holds(&cuda_forwarding_problems(
        &manifest("voice-pipes-cli"),
        &providers,
    ));
}

#[test]
fn the_cuda_rule_catches_a_provider_left_on_the_cpu() {
    let mut providers = BTreeMap::new();
    providers.insert(
        "voice-pipes-stt-a".to_string(),
        toml("[features]\nort-cuda = []"),
    );
    providers.insert(
        "voice-pipes-stt-b".to_string(),
        toml("[features]\nort-cuda = []"),
    );
    let cli = toml(
        r#"[dependencies]
voice-pipes-stt-a = { path = "a", optional = true }
voice-pipes-stt-b = { path = "b", optional = true }
[features]
ort-cuda = ["voice-pipes-stt-a?/ort-cuda"]"#,
    );
    let problems = cuda_forwarding_problems(&cli, &providers);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert_reports(&problems, "voice-pipes-stt-b?/ort-cuda");
}

/// ONNX providers that build CPU sessions only, by design: each runs a
/// small model once per pause (the turn detectors) or once per 32 ms window
/// (Silero), where a GPU round trip costs more than the inference. Every
/// other provider on `ort` must take `ort-cuda`.
const CPU_ONLY_ONNX: &[&str] = &[
    "voice-pipes-turn-smart",
    "voice-pipes-turn-vap",
    "voice-pipes-vad-silero",
];

/// A provider on ONNX Runtime either has an `ort-cuda` feature (which the
/// rule above holds the CLI to forwarding) or is named CPU-only, so a new
/// ONNX plugin cannot silently miss the GPU build. The list stays true: a
/// CPU-only entry that gains `ort-cuda`, or stops using `ort`, fails.
fn onnx_gpu_problem(p: &str, m: &toml::Table, cpu_only: bool) -> Option<String> {
    let on_ort = deps(m, &["dependencies"]).contains("ort");
    let has_cuda = has_feature(m, "ort-cuda");
    match (cpu_only, on_ort, has_cuda) {
        (true, false, _) => Some(format!("{p} is listed CPU-only but does not use ort")),
        (true, true, true) => Some(format!("{p} is listed CPU-only but has ort-cuda")),
        (false, true, false) => Some(format!(
            "{p} uses ort without an ort-cuda feature; add one, or list it in CPU_ONLY_ONNX with the reason"
        )),
        _ => None,
    }
}

#[test]
fn every_onnx_provider_takes_ort_cuda_or_is_named_cpu_only() {
    for p in providers() {
        let cpu_only = CPU_ONLY_ONNX.contains(&p.as_str());
        assert_eq!(onnx_gpu_problem(&p, &manifest(&p), cpu_only), None);
    }
}

#[test]
fn the_onnx_gpu_rule_catches_each_drift() {
    let ort = toml("[dependencies]\nort = \"2\"");
    let ort_cuda = toml("[dependencies]\nort = \"2\"\n[features]\nort-cuda = []");
    let none = toml("[dependencies]\nserde = \"1\"");
    assert!(
        onnx_gpu_problem("p", &ort, false).is_some(),
        "ort, no cuda, not listed"
    );
    assert!(
        onnx_gpu_problem("p", &ort_cuda, true).is_some(),
        "listed but has cuda"
    );
    assert!(
        onnx_gpu_problem("p", &none, true).is_some(),
        "listed but not on ort"
    );
    assert!(onnx_gpu_problem("p", &ort_cuda, false).is_none());
    assert!(onnx_gpu_problem("p", &ort, true).is_none());
    assert!(onnx_gpu_problem("p", &none, false).is_none());
}

/// Workspace crates other than this one that may link a provider, and
/// which. The bench runs VAP directly in its turn-detection bake-off (with
/// explicit parameters, not through a config), and its corpus test scores
/// Parakeet with the bench's own WER metrics; anything else goes through
/// the registry.
const PROVIDER_USERS: &[(&str, &[&str])] = &[(
    "voice-pipes-bench",
    &["voice-pipes-turn-vap", "voice-pipes-stt-parakeet"],
)];

/// Only this crate, the composition root, links providers freely; any other
/// member linking one is on the list above, and the list names only links
/// that exist.
fn provider_link_problem(m: &str, manifest: &toml::Table, members: &[String]) -> Option<String> {
    let linked: BTreeSet<String> = workspace_deps(manifest, ALL_SECTIONS, members)
        .into_iter()
        .filter(|d| is_provider(d))
        .collect();
    let allowed: BTreeSet<String> = PROVIDER_USERS
        .iter()
        .filter(|(user, _)| *user == m)
        .flat_map(|(_, ps)| ps.iter().map(|p| (*p).to_string()))
        .collect();
    (linked != allowed)
        .then(|| format!("{m} links providers {linked:?}; PROVIDER_USERS allows {allowed:?}"))
}

#[test]
fn only_listed_members_link_a_provider() {
    let members = members();
    for m in members
        .iter()
        .filter(|m| !is_provider(m) && *m != "voice-pipes-cli")
    {
        assert_eq!(provider_link_problem(m, &manifest(m), &members), None);
    }
}

#[test]
fn the_provider_link_rule_catches_an_unlisted_link_and_a_stale_entry() {
    let members: Vec<String> = ["voice-pipes-turn-vap", "voice-pipes-tts-x"]
        .map(String::from)
        .to_vec();
    let links_x = toml("[dev-dependencies]\nvoice-pipes-tts-x = { path = \"x\" }");
    assert!(provider_link_problem("voice-pipes", &links_x, &members).is_some());
    let links_nothing = toml("[dependencies]\nserde = \"1\"");
    assert!(
        provider_link_problem("voice-pipes-bench", &links_nothing, &members).is_some(),
        "a listed link that no longer exists"
    );
}

/// Every provider crate has its own CODEOWNERS entry, so its owners can be
/// set apart from the engine's.
fn codeowners_problems(owners: &str, providers: &[String]) -> Vec<String> {
    providers
        .iter()
        .filter(|p| {
            let entry = format!("/{p}/");
            !owners
                .lines()
                .any(|l| l.split_whitespace().next() == Some(entry.as_str()))
        })
        .map(|p| format!(".github/CODEOWNERS has no `/{p}/` line"))
        .collect()
}

#[test]
fn every_provider_has_a_codeowners_entry() {
    let owners =
        std::fs::read_to_string(root().join(".github/CODEOWNERS")).expect(".github/CODEOWNERS");
    assert_holds(&codeowners_problems(&owners, &providers()));
}

#[test]
fn the_codeowners_rule_catches_a_missing_or_commented_entry() {
    let providers = ["voice-pipes-tts-x".to_string()];
    assert_holds(&codeowners_problems("/voice-pipes-tts-x/ @a\n", &providers));
    assert_reports(
        &codeowners_problems("# /voice-pipes-tts-x/ @a\n", &providers),
        "no `/voice-pipes-tts-x/`",
    );
    assert_reports(
        &codeowners_problems("/voice-pipes-tts-xy/ @a\n", &providers),
        "no `/voice-pipes-tts-x/`",
    );
}

// ── Sources ─────────────────────────────────────────────────

/// Every `.rs` file under `dir`.
fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// A crate's sources: its library and binaries, and its tests, examples and
/// benches, which are crates of their own that the library's attributes do
/// not cover.
fn crate_sources(member: &str) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for dir in ["src", "tests", "examples", "benches"] {
        rust_files(&root().join(member).join(dir), &mut files);
    }
    files
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn parse(text: &str, path: &Path) -> syn::File {
    syn::parse_file(text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn syntax(src: &str) -> syn::File {
    syn::parse_file(src).expect("test source")
}

/// Whether `attrs` hold `#[cfg(test)]`: the item is compiled for tests only.
fn is_test_only(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| {
        a.path().is_ident("cfg") && a.parse_args::<syn::Ident>().is_ok_and(|i| i == "test")
    })
}

/// Whether `attrs` hold a lint attribute `level` (`forbid`, `deny`, ...)
/// naming `lint`.
fn has_lint(attrs: &[syn::Attribute], level: &str, lint: &str) -> bool {
    attrs.iter().any(|a| {
        a.path().is_ident(level)
            && a.parse_args_with(
                syn::punctuated::Punctuated::<syn::Path, syn::Token![,]>::parse_terminated,
            )
            .is_ok_and(|lints| lints.iter().any(|p| p.is_ident(lint)))
    })
}

/// Provider code is safe Rust: the library forbids `unsafe` for itself, and
/// its tests, examples and benches, which that attribute does not reach,
/// write none. FFI stays in the audited crates that exist for it (`ort`,
/// `cpal`); a build script is a separate crate and is not covered.
fn unsafe_problems(file: &syn::File, is_library_root: bool) -> Vec<String> {
    struct Unsafe(Vec<String>);
    impl Visit<'_> for Unsafe {
        fn visit_expr_unsafe(&mut self, i: &syn::ExprUnsafe) {
            self.0.push("an unsafe block".into());
            syn::visit::visit_expr_unsafe(self, i);
        }
        fn visit_signature(&mut self, i: &syn::Signature) {
            if i.unsafety.is_some() {
                self.0.push(format!("unsafe fn {}", i.ident));
            }
            syn::visit::visit_signature(self, i);
        }
        fn visit_item_impl(&mut self, i: &syn::ItemImpl) {
            if i.unsafety.is_some() {
                self.0.push("an unsafe impl".into());
            }
            syn::visit::visit_item_impl(self, i);
        }
        fn visit_item_trait(&mut self, i: &syn::ItemTrait) {
            if i.unsafety.is_some() {
                self.0.push(format!("unsafe trait {}", i.ident));
            }
            syn::visit::visit_item_trait(self, i);
        }
        fn visit_item_foreign_mod(&mut self, i: &syn::ItemForeignMod) {
            self.0.push("an extern block".into());
            syn::visit::visit_item_foreign_mod(self, i);
        }
    }
    if is_library_root {
        return if has_lint(&file.attrs, "forbid", "unsafe_code") {
            Vec::new()
        } else {
            vec!["the library root does not declare #![forbid(unsafe_code)]".into()]
        };
    }
    let mut found = Unsafe(Vec::new());
    found.visit_file(file);
    found.0
}

#[test]
fn every_provider_forbids_unsafe_code() {
    for p in providers() {
        let lib = root().join(&p).join("src/lib.rs");
        for file in crate_sources(&p) {
            let is_root = file == lib;
            // The library's own modules are under its forbid.
            if !is_root && file.starts_with(root().join(&p).join("src")) {
                continue;
            }
            let problems = unsafe_problems(&parse(&read(&file), &file), is_root);
            assert!(problems.is_empty(), "{}: {problems:?}", file.display());
        }
    }
}

#[test]
fn the_unsafe_rule_reads_attributes_and_code_not_text() {
    assert!(unsafe_problems(&syntax("#![forbid(unsafe_code)]\nfn f() {}"), true).is_empty());
    assert!(
        unsafe_problems(&syntax("#![forbid(missing_docs, unsafe_code)]"), true).is_empty(),
        "one lint among several"
    );
    assert!(
        !unsafe_problems(&syntax("// #![forbid(unsafe_code)]\nfn f() {}"), true).is_empty(),
        "a comment is not the attribute"
    );
    assert!(
        !unsafe_problems(&syntax("#![deny(unsafe_code)]"), true).is_empty(),
        "deny can be allowed again below"
    );
    assert!(
        !unsafe_problems(&syntax("mod m { #![forbid(unsafe_code)] }"), true).is_empty(),
        "a module's forbid is not the crate's"
    );
    for src in [
        "fn f() { unsafe { std::hint::unreachable_unchecked() } }",
        "unsafe fn f() {}",
        "struct S; unsafe impl Send for S {}",
        "unsafe extern \"C\" { fn g(); }",
        "#[test] fn t() { let _ = || unsafe { 1 }; }",
    ] {
        assert!(!unsafe_problems(&syntax(src), false).is_empty(), "{src}");
    }
    assert!(
        unsafe_problems(&syntax("fn f() { let s = \"unsafe { }\"; }"), false).is_empty(),
        "a string is not code"
    );
}

/// The lints that keep a provider from panicking on what it parses: a
/// vendor's frames, a model file, a client's settings.
const PARSING_LINTS: &[&str] = &[
    "indexing_slicing",
    "unwrap_used",
    "expect_used",
    "cast_possible_truncation",
];

/// Which of [`PARSING_LINTS`] a crate root does not deny outside tests
/// (`#![cfg_attr(not(test), deny(clippy::…))]`, or a plain `deny`).
fn undenied_parsing_lints(file: &syn::File) -> Vec<String> {
    type Metas = syn::punctuated::Punctuated<syn::Meta, syn::Token![,]>;
    fn denied(meta: &syn::Meta, out: &mut BTreeSet<String>) {
        if let syn::Meta::List(list) = meta
            && list.path.is_ident("deny")
            && let Ok(lints) = list.parse_args_with(Metas::parse_terminated)
        {
            for lint in lints {
                let segs: Vec<String> = lint
                    .path()
                    .segments
                    .iter()
                    .map(|s| s.ident.to_string())
                    .collect();
                if let [tool, name] = segs.as_slice()
                    && tool == "clippy"
                {
                    out.insert(name.clone());
                }
            }
        }
    }
    let mut found = BTreeSet::new();
    for attr in &file.attrs {
        if attr.path().is_ident("deny") {
            denied(&attr.meta, &mut found);
        } else if attr.path().is_ident("cfg_attr")
            && let Ok(args) = attr.parse_args_with(Metas::parse_terminated)
        {
            let mut args = args.into_iter();
            let only_outside_tests = args.next().is_some_and(|cond| {
                matches!(&cond, syn::Meta::List(l) if l.path.is_ident("not")
                    && l.parse_args::<syn::Ident>().is_ok_and(|i| i == "test"))
            });
            if only_outside_tests {
                for meta in args {
                    denied(&meta, &mut found);
                }
            }
        }
    }
    PARSING_LINTS
        .iter()
        .filter(|l| !found.contains(**l))
        .map(|l| (*l).to_string())
        .collect()
}

/// A provider's library denies the parsing lints, so a vendor frame, a
/// model file or a client's setting that is not what it expects is an
/// error, never a panic. Its tests are exempt.
#[test]
fn every_provider_denies_the_panicking_parsing_lints() {
    for p in providers() {
        let lib = root().join(&p).join("src/lib.rs");
        let missing = undenied_parsing_lints(&parse(&read(&lib), &lib));
        assert!(
            missing.is_empty(),
            "{p}/src/lib.rs does not deny clippy::{missing:?} outside tests"
        );
    }
}

#[test]
fn the_parsing_lints_rule_reads_the_crate_attributes() {
    let all = "clippy::indexing_slicing, clippy::unwrap_used, clippy::expect_used, clippy::cast_possible_truncation";
    let outside_tests = format!("#![cfg_attr(not(test), deny({all}))]");
    assert!(undenied_parsing_lints(&syntax(&outside_tests)).is_empty());
    assert!(undenied_parsing_lints(&syntax(&format!("#![deny({all})]"))).is_empty());
    assert_eq!(
        undenied_parsing_lints(&syntax(
            "#![cfg_attr(not(test), deny(clippy::indexing_slicing, clippy::unwrap_used, clippy::expect_used))]"
        )),
        ["cast_possible_truncation"]
    );
    for src in [
        format!("// {outside_tests}"),
        format!("#![cfg_attr(test, deny({all}))]"),
        format!("#![warn({all})]"),
        format!("mod m {{ #![deny({all})] }}"),
    ] {
        assert_eq!(undenied_parsing_lints(&syntax(&src)).len(), 4, "{src}");
    }
}

/// Where a crate builds an ONNX session itself instead of through
/// `voice_pipes_onnx::open_session`: a `Session::builder` call under any
/// name `Session` is imported as, or the builder type by name.
fn direct_session_builders(file: &syn::File) -> Vec<String> {
    struct Builders {
        session_names: BTreeSet<String>,
        found: Vec<String>,
    }
    impl Builders {
        fn check(&mut self, path: &syn::Path, qself: Option<&syn::QSelf>) {
            let segs: Vec<String> = path.segments.iter().map(|s| s.ident.to_string()).collect();
            if segs.iter().any(|s| s == "SessionBuilder") {
                self.found.push(segs.join("::"));
                return;
            }
            let owner = match (qself, segs.as_slice()) {
                (Some(q), [.., last]) if last == "builder" => match &*q.ty {
                    syn::Type::Path(t) => t.path.segments.last().map(|s| s.ident.to_string()),
                    _ => None,
                },
                (None, [.., owner, last]) if last == "builder" => Some(owner.clone()),
                _ => None,
            };
            if owner.is_some_and(|o| self.session_names.contains(&o)) {
                self.found.push(segs.join("::"));
            }
        }
    }
    impl Visit<'_> for Builders {
        fn visit_expr_path(&mut self, i: &syn::ExprPath) {
            self.check(&i.path, i.qself.as_ref());
            syn::visit::visit_expr_path(self, i);
        }
        fn visit_type_path(&mut self, i: &syn::TypePath) {
            self.check(&i.path, i.qself.as_ref());
            syn::visit::visit_type_path(self, i);
        }
        fn visit_use_tree(&mut self, i: &syn::UseTree) {
            match i {
                syn::UseTree::Rename(r) if r.ident == "Session" => {
                    self.session_names.insert(r.rename.to_string());
                }
                syn::UseTree::Name(n) if n.ident == "SessionBuilder" => {
                    self.found.push("use SessionBuilder".into());
                }
                _ => {}
            }
            syn::visit::visit_use_tree(self, i);
        }
    }
    let mut b = Builders {
        session_names: BTreeSet::from(["Session".to_string()]),
        found: Vec::new(),
    };
    // Aliases first, wherever the `use` sits in the file.
    for item in &file.items {
        if let syn::Item::Use(u) = item {
            b.visit_item_use(u);
        }
    }
    b.found.clear();
    b.visit_file(file);
    b.found
}

/// A provider opens its ONNX sessions through `voice_pipes_onnx::open_session`,
/// so thread, optimisation and execution-provider policy is one policy for
/// the process rather than one per crate: in its library, tests, examples
/// and benches.
#[test]
fn providers_open_onnx_sessions_through_the_shared_opener() {
    let mut checked = 0;
    for p in providers() {
        for file in crate_sources(&p) {
            let found = direct_session_builders(&parse(&read(&file), &file));
            assert!(
                found.is_empty(),
                "{} builds an ONNX session directly ({found:?}); use voice_pipes_onnx::open_session",
                file.display()
            );
            checked += 1;
        }
    }
    assert!(checked > 0, "no provider sources found");
}

#[test]
fn the_session_rule_sees_every_spelling_and_ignores_text() {
    for src in [
        "fn f() { let b = Session::builder(); }",
        "fn f() { let b = ort::session::Session::builder(); }",
        "use ort::session::Session as S; fn f() { S::builder(); }",
        "fn f() { <Session>::builder(); }",
        "use ort::session::builder::SessionBuilder;",
        "fn f(b: ort::session::builder::SessionBuilder) {}",
    ] {
        assert!(!direct_session_builders(&syntax(src)).is_empty(), "{src}");
    }
    for src in [
        "// Session::builder() is not called here\nfn f() {}",
        "fn f() { let s = \"Session::builder(\"; }",
        "fn f() { voice_pipes_onnx::open_session(p, o); }",
        "fn f() { Config::builder(); }",
    ] {
        assert!(direct_session_builders(&syntax(src)).is_empty(), "{src}");
    }
}

/// The crate that keeps its own version: the SDK's is the promise plugin
/// authors build on. Every other member takes the workspace's.
const OWN_VERSION: &[&str] = &["voice-pipes-sdk"];

/// Whether `m`'s version is the workspace's (`version.workspace = true`).
fn inherits_workspace_version(m: &toml::Table) -> bool {
    m.get("package")
        .and_then(|p| p.get("version"))
        .and_then(|v| v.get("workspace"))
        .and_then(toml::Value::as_bool)
        == Some(true)
}

/// The crates move in lockstep: one version for all but the SDK, so a
/// release is one number and no crate is left behind at an older one.
#[test]
fn every_crate_but_the_sdk_takes_the_workspace_version() {
    for m in members() {
        let inherits = inherits_workspace_version(&manifest(&m));
        if OWN_VERSION.contains(&m.as_str()) {
            assert!(!inherits, "{m} keeps its own version");
        } else {
            assert!(inherits, "{m} must take `version.workspace = true`");
        }
    }
}

/// The version `[workspace.dependencies]` gives each workspace crate, the
/// one crates.io resolves it by, is the crate's own: the workspace version,
/// or the SDK's. A bump that misses one fails here, not at publication.
#[test]
fn workspace_dependencies_name_each_crates_own_version() {
    let root = manifest(".");
    let workspace_version = root["workspace"]["package"]["version"]
        .as_str()
        .expect("[workspace.package] version");
    let deps = root["workspace"]["dependencies"]
        .as_table()
        .expect("[workspace.dependencies]");
    for m in members() {
        let own = if OWN_VERSION.contains(&m.as_str()) {
            manifest(&m)["package"]["version"]
                .as_str()
                .expect("its own version")
                .to_string()
        } else {
            workspace_version.to_string()
        };
        let Some(dep) = deps.get(&m) else {
            continue;
        };
        assert_eq!(
            dep.get("version").and_then(toml::Value::as_str),
            Some(own.as_str()),
            "[workspace.dependencies] {m} must say version = \"{own}\""
        );
    }
}

#[test]
fn the_version_rule_tells_inherited_from_declared() {
    assert!(inherits_workspace_version(&toml(
        "[package]\nversion.workspace = true"
    )));
    assert!(!inherits_workspace_version(&toml(
        "[package]\nversion = \"0.1.0\""
    )));
    assert!(!inherits_workspace_version(&toml(
        "[package]\nname = \"x\""
    )));
}

/// The `#[instrument]` attributes in `file` that do not say `skip_all`, by
/// line: each records every argument it does not skip into its span, and a
/// provider's arguments are its payload (the text to speak, the audio, the
/// token ids drawn from them).
fn instrument_without_skip_all(file: &syn::File) -> Vec<usize> {
    struct Spans(Vec<usize>);
    impl Visit<'_> for Spans {
        fn visit_attribute(&mut self, i: &syn::Attribute) {
            let is_instrument = i
                .path()
                .segments
                .last()
                .is_some_and(|s| s.ident == "instrument");
            if is_instrument {
                let skips_all = match &i.meta {
                    syn::Meta::List(list) => list.tokens.clone().into_iter().any(
                        |t| matches!(&t, proc_macro2::TokenTree::Ident(id) if id == "skip_all"),
                    ),
                    _ => false,
                };
                if !skips_all {
                    self.0.push(i.pound_token.span.start().line);
                }
            }
            syn::visit::visit_attribute(self, i);
        }
    }
    let mut spans = Spans(Vec::new());
    spans.visit_file(file);
    spans.0
}

/// Spans name what they record: `#[instrument]` says `skip_all` and lists
/// its `fields`, so no argument (the text being spoken, a transcript's
/// audio, a prompt) reaches a span by default. Every member, providers and
/// engine alike.
#[test]
fn spans_record_no_argument_by_default() {
    let mut checked = 0;
    for p in members() {
        for file in crate_sources(&p) {
            let lines = instrument_without_skip_all(&parse(&read(&file), &file));
            assert!(
                lines.is_empty(),
                "{} has #[instrument] without skip_all at lines {lines:?}; skip_all and name the fields",
                file.display()
            );
            checked += 1;
        }
    }
    assert!(checked > 0, "no sources found");
}

#[test]
fn the_span_rule_sees_each_spelling() {
    for src in [
        "#[instrument] fn f(text: &str) {}",
        "#[tracing::instrument(skip(self))] fn f(&self, text: &str) {}",
        "impl X { #[instrument(fields(n = 1))] fn f(&self) {} }",
        "#[instrument(name = \"skip_all\")] fn f(text: &str) {}",
    ] {
        assert_eq!(instrument_without_skip_all(&syntax(src)).len(), 1, "{src}");
    }
    for src in [
        "#[instrument(skip_all)] fn f(text: &str) {}",
        "#[tracing::instrument(skip_all, fields(chars = text.len()))] fn f(text: &str) {}",
        "// #[instrument] is not used here\nfn f() {}",
    ] {
        assert!(
            instrument_without_skip_all(&syntax(src)).is_empty(),
            "{src}"
        );
    }
}

/// Names that hold what a person said or will hear, or what a vendor sent
/// back: a field or a format argument by one of these names (or ending in
/// `_` and one of them) puts that payload in a log line.
const PAYLOAD_NAMES: &[&str] = &[
    "text",
    "prompt",
    "fragment",
    "output",
    "transcript",
    "reply",
    "input",
    "body",
];

/// The macros that write a log line.
const EVENT_MACROS: &[&str] = &["trace", "debug", "info", "warn", "error", "event"];

fn is_payload_name(name: &str) -> bool {
    // `has_text`, `is_reply`: whether there is one, not what it says.
    if name.starts_with("has_") || name.starts_with("is_") {
        return false;
    }
    PAYLOAD_NAMES
        .iter()
        .any(|p| name == *p || name.ends_with(&format!("_{p}")))
}

/// The payload names a format string captures inline (`{text}`, `{text:?}`).
fn inline_payload_captures(format: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = format;
    while let Some(open) = rest.find('{') {
        rest = &rest[open + 1..];
        if rest.starts_with('{') {
            rest = &rest[1..];
            continue;
        }
        let end = rest.find('}').unwrap_or(rest.len());
        let name = rest[..end].split(':').next().unwrap_or("").trim();
        if is_payload_name(name) {
            found.push(name.to_string());
        }
        rest = &rest[end..];
    }
    found
}

/// The last identifier of a run of tokens such as `utterance.text` or
/// `self.turn.text`: the name the value goes by.
fn last_ident(tokens: &[proc_macro2::TokenTree]) -> Option<String> {
    tokens.iter().rev().find_map(|t| match t {
        proc_macro2::TokenTree::Ident(id) => Some(id.to_string()),
        _ => None,
    })
}

/// The payloads one event macro's arguments put in its line: a field
/// named for a payload (`text = %t`, `final_text = ?t`, `text = t`), a
/// shorthand field (`%utterance.text`, `?prompt`), an inline capture in
/// the message (`"{text}"`) or a positional argument (`"{}", text`).
fn payload_arguments(tokens: proc_macro2::TokenStream) -> Vec<String> {
    use proc_macro2::TokenTree;
    let mut segments: Vec<Vec<TokenTree>> = vec![Vec::new()];
    for t in tokens {
        match &t {
            TokenTree::Punct(p) if p.as_char() == ',' => segments.push(Vec::new()),
            _ => segments.last_mut().expect("one segment").push(t),
        }
    }
    let mut found = Vec::new();
    let mut after_message = false;
    for seg in segments.iter().filter(|s| !s.is_empty()) {
        match &seg[0] {
            TokenTree::Literal(lit) => {
                if let Ok(syn::Lit::Str(s)) = syn::parse_str::<syn::Lit>(&lit.to_string()) {
                    found.extend(inline_payload_captures(&s.value()));
                    after_message = true;
                }
            }
            TokenTree::Punct(p) if p.as_char() == '%' || p.as_char() == '?' => {
                if let Some(name) = last_ident(&seg[1..])
                    && is_payload_name(&name)
                {
                    found.push(name);
                }
            }
            _ => {
                // `name = value` (but not `==`): the field's name is what
                // precedes the `=`.
                let eq = seg.iter().position(|t| {
                    matches!(t, TokenTree::Punct(p) if p.as_char() == '='
                        && p.spacing() == proc_macro2::Spacing::Alone)
                });
                if let Some(eq) = eq {
                    // The field's name, or the value it is given
                    // (`kept = %reply`, `x = utterance.text`).
                    if let Some(name) = last_ident(&seg[..eq])
                        .filter(|n| is_payload_name(n))
                        .or_else(|| last_ident(&seg[eq + 1..]).filter(|n| is_payload_name(n)))
                    {
                        found.push(name);
                    }
                } else if after_message
                    && let Some(name) = last_ident(seg)
                    && is_payload_name(&name)
                {
                    found.push(name);
                }
            }
        }
    }
    found
}

/// The payloads in the event macros of `file`, by line, outside code
/// compiled for tests only.
fn payload_log_lines(file: &syn::File) -> Vec<(usize, String)> {
    struct Events(Vec<(usize, String)>);
    impl Visit<'_> for Events {
        fn visit_item_mod(&mut self, i: &syn::ItemMod) {
            if !is_test_only(&i.attrs) {
                syn::visit::visit_item_mod(self, i);
            }
        }
        fn visit_item_fn(&mut self, i: &syn::ItemFn) {
            if !is_test_only(&i.attrs) {
                syn::visit::visit_item_fn(self, i);
            }
        }
        fn visit_macro(&mut self, i: &syn::Macro) {
            let is_event = i
                .path
                .segments
                .last()
                .is_some_and(|s| EVENT_MACROS.contains(&s.ident.to_string().as_str()));
            if is_event {
                let line = i.path.segments[0].ident.span().start().line;
                for name in payload_arguments(i.tokens.clone()) {
                    self.0.push((line, name));
                }
            }
            syn::visit::visit_macro(self, i);
        }
    }
    let mut events = Events(Vec::new());
    events.visit_file(&file.clone());
    events.0
}

/// Log lines carry no payload at any level: what a person said, the prompt,
/// the model's reply and a vendor's response body stay out of every
/// `trace!` to `error!`, which log their length or kind instead. Test code
/// is exempt; library and binary code of every member is not.
#[test]
fn log_lines_carry_no_payload() {
    let mut problems = Vec::new();
    let mut checked = 0;
    for p in members() {
        let mut files = Vec::new();
        rust_files(&root().join(&p).join("src"), &mut files);
        for file in files {
            if file.components().any(|c| c.as_os_str() == "tests") {
                continue;
            }
            for (line, name) in payload_log_lines(&parse(&read(&file), &file)) {
                problems.push(format!("{}:{line} logs `{name}`", file.display()));
            }
            checked += 1;
        }
    }
    assert!(checked > 0, "no sources found");
    assert_holds(&problems);
}

#[test]
fn the_payload_log_rule_sees_each_spelling() {
    for (src, name) in [
        ("fn f() { info!(text = %t, \"x\"); }", "text"),
        (
            "fn f() { tracing::debug!(final_text = ?t, \"x\"); }",
            "final_text",
        ),
        ("fn f() { warn!(%utterance.text, \"x\"); }", "text"),
        ("fn f() { trace!(prompt = p.as_str(), \"x\"); }", "prompt"),
        ("fn f() { error!(\"vendor said {body}\"); }", "body"),
        (
            "fn f() { info!(\"heard {}\", self.transcript); }",
            "transcript",
        ),
        ("fn f() { info!(\"heard {reply:?}\"); }", "reply"),
        ("fn f() { warn!(kept = %last_reply, \"x\"); }", "last_reply"),
        ("fn f() { warn!(kept = utterance.text, \"x\"); }", "text"),
    ] {
        let found = payload_log_lines(&syntax(src));
        assert!(found.iter().any(|(_, n)| n == name), "{src}: {found:?}");
    }
    for src in [
        "fn f() { info!(chars = text.chars().count(), \"x\"); }",
        "fn f() { info!(text_len = n, ok = a == b, \"x\"); }",
        "fn f() { info!(has_text = t.is_some(), \"x\"); }",
        "fn f() { info!(\"{{text}} is literal\"); }",
        "fn f() { let text = format!(\"{text}\"); }",
        "#[cfg(test)] mod tests { fn f() { info!(text = %t); } }",
    ] {
        assert!(payload_log_lines(&syntax(src)).is_empty(), "{src}");
    }
}

/// The names and aliases a factory implementation in `file` registers:
/// the string its `name()` returns and those its `aliases()` lists, for
/// every `impl ...Factory for ...`.
fn factory_names(file: &syn::File) -> BTreeSet<String> {
    struct Names(BTreeSet<String>);
    impl Visit<'_> for Names {
        fn visit_item_impl(&mut self, i: &syn::ItemImpl) {
            let is_factory = i.trait_.as_ref().is_some_and(|(_, path, _)| {
                path.segments
                    .last()
                    .is_some_and(|s| s.ident.to_string().ends_with("Factory"))
            });
            if is_factory {
                for item in &i.items {
                    if let syn::ImplItem::Fn(f) = item
                        && (f.sig.ident == "name" || f.sig.ident == "aliases")
                    {
                        struct Strings<'a>(&'a mut BTreeSet<String>);
                        impl Visit<'_> for Strings<'_> {
                            fn visit_lit_str(&mut self, i: &syn::LitStr) {
                                self.0.insert(i.value());
                            }
                        }
                        Strings(&mut self.0).visit_block(&f.block);
                    }
                }
            }
            syn::visit::visit_item_impl(self, i);
        }
    }
    let mut names = Names(BTreeSet::new());
    names.visit_file(file);
    names.0
}

/// The string literals in `file` that equal one of `names`, by line,
/// outside attributes (doc comments name providers as examples) and code
/// compiled for tests only.
fn provider_names_in(file: &syn::File, names: &BTreeSet<String>) -> Vec<(usize, String)> {
    struct Literals<'a> {
        names: &'a BTreeSet<String>,
        found: Vec<(usize, String)>,
    }
    impl Visit<'_> for Literals<'_> {
        fn visit_attribute(&mut self, _: &syn::Attribute) {}
        fn visit_item_mod(&mut self, i: &syn::ItemMod) {
            if !is_test_only(&i.attrs) {
                syn::visit::visit_item_mod(self, i);
            }
        }
        fn visit_item_fn(&mut self, i: &syn::ItemFn) {
            if !is_test_only(&i.attrs) {
                syn::visit::visit_item_fn(self, i);
            }
        }
        fn visit_lit_str(&mut self, i: &syn::LitStr) {
            let value = i.value();
            if self.names.contains(&value) {
                self.found.push((i.span().start().line, value));
            }
        }
    }
    let mut literals = Literals {
        names,
        found: Vec::new(),
    };
    literals.visit_file(file);
    literals.found
}

/// Names the engine itself defines that a provider also uses (a provider
/// may share a built-in's word without making it a provider fact):
/// `llm` is the engine's word for the text stage and its metrics, and the
/// OpenAI-compatible processor takes it as an alias.
const ENGINE_OWN_NAMES: &[&str] = &["llm"];

/// The engine names no provider: no string literal in its library code is
/// a name or alias a provider crate's factory registers. A fact about a
/// provider is asked of its factory instead. The engine's own built-ins
/// (the energy VAD, the silence and layered turn modes) are not providers.
#[test]
fn the_engine_names_no_provider() {
    let mut names = BTreeSet::new();
    for p in members().into_iter().filter(|m| is_provider(m)) {
        let mut files = Vec::new();
        rust_files(&root().join(&p).join("src"), &mut files);
        for file in files {
            names.extend(factory_names(&parse(&read(&file), &file)));
        }
    }
    assert!(!names.is_empty(), "no provider names found");
    for own in ENGINE_OWN_NAMES {
        names.remove(*own);
    }
    let mut problems = Vec::new();
    let mut files = Vec::new();
    rust_files(&root().join("voice-pipes").join("src"), &mut files);
    for file in files {
        if file.components().any(|c| c.as_os_str() == "tests") {
            continue;
        }
        for (line, name) in provider_names_in(&parse(&read(&file), &file), &names) {
            problems.push(format!(
                "{}:{line} names provider \"{name}\"",
                file.display()
            ));
        }
    }
    assert_holds(&problems);
}

#[test]
fn the_provider_name_rule_sees_names_and_aliases_but_not_docs_or_tests() {
    let names = factory_names(&syntax(
        "impl SttFactory for F { fn name(&self) -> &str { \"acme\" } fn aliases(&self) -> &[&str] { &[\"acme-v2\"] } }",
    ));
    assert_eq!(
        names,
        BTreeSet::from(["acme".to_string(), "acme-v2".to_string()])
    );
    let found = provider_names_in(
        &syntax("fn rate(p: &str) -> u32 { if p == \"acme-v2\" { 24_000 } else { 16_000 } }"),
        &names,
    );
    assert_eq!(found.len(), 1, "{found:?}");
    for src in [
        "/// Like \"acme\" does.\nfn f() {}",
        "#[cfg(test)] mod tests { fn f() -> &'static str { \"acme\" } }",
        "fn f() -> &'static str { \"acme-ish\" }",
    ] {
        assert!(provider_names_in(&syntax(src), &names).is_empty(), "{src}");
    }
}

/// Words that mark an environment variable as a credential.
const CREDENTIAL_MARKERS: &[&str] = &["KEY", "TOKEN", "SECRET", "PASSWORD", "CREDENTIAL"];

/// Every string constant the file defines, by name.
fn string_consts(file: &syn::File) -> BTreeMap<String, String> {
    struct Consts(BTreeMap<String, String>);
    impl Visit<'_> for Consts {
        fn visit_item_const(&mut self, i: &syn::ItemConst) {
            if let syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(s),
                ..
            }) = &*i.expr
            {
                self.0.insert(i.ident.to_string(), s.value());
            }
        }
    }
    let mut c = Consts(BTreeMap::new());
    c.visit_file(file);
    c.0
}

/// Where code reads a credential itself: an environment lookup
/// (`env::var`, `env::var_os`) of a variable whose name, or the constant
/// naming it, marks a credential, or a call to the host's `resolve_key`.
/// `consts` resolves a constant passed by name.
fn credential_reads(file: &syn::File, consts: &BTreeMap<String, String>) -> Vec<String> {
    struct Reads<'a> {
        consts: &'a BTreeMap<String, String>,
        found: Vec<String>,
    }
    impl Visit<'_> for Reads<'_> {
        fn visit_expr_call(&mut self, i: &syn::ExprCall) {
            if let syn::Expr::Path(f) = &*i.func {
                let segs: Vec<String> = f
                    .path
                    .segments
                    .iter()
                    .map(|s| s.ident.to_string())
                    .collect();
                let names: Vec<String> = match i.args.first() {
                    Some(syn::Expr::Lit(syn::ExprLit {
                        lit: syn::Lit::Str(s),
                        ..
                    })) => vec![s.value()],
                    Some(syn::Expr::Path(p)) => p
                        .path
                        .segments
                        .last()
                        .map(|s| {
                            let name = s.ident.to_string();
                            let value = self.consts.get(&name).cloned().unwrap_or_default();
                            vec![name, value]
                        })
                        .unwrap_or_default(),
                    _ => Vec::new(),
                };
                let reads_env = matches!(
                    segs.as_slice(),
                    [.., env, var] if env == "env" && (var == "var" || var == "var_os")
                );
                let marked = names.iter().any(|n| {
                    let n = n.to_uppercase();
                    CREDENTIAL_MARKERS.iter().any(|m| n.contains(m))
                });
                if (reads_env && marked) || segs.last().is_some_and(|s| s == "resolve_key") {
                    self.found
                        .push(format!("{}({})", segs.join("::"), names.join(" = ")));
                }
            }
            syn::visit::visit_expr_call(self, i);
        }
    }
    let mut r = Reads {
        consts,
        found: Vec::new(),
    };
    r.visit_file(file);
    r.found
}

/// A provider never reads a credential: the host resolves every key, from
/// the config or the variable the factory's `default_api_key_env` names,
/// and hands it over in the config. Test code, which may read a real key
/// for a live test, is not covered.
#[test]
fn providers_take_credentials_from_the_host() {
    let mut problems = Vec::new();
    for p in providers() {
        let files: Vec<_> = crate_sources(&p)
            .into_iter()
            .filter(|f| !is_test_file(f))
            .map(|f| {
                let file = parse(&read(&f), &f);
                (f, file)
            })
            .collect();
        let consts: BTreeMap<String, String> =
            files.iter().flat_map(|(_, f)| string_consts(f)).collect();
        for (path, file) in &files {
            for read in credential_reads(file, &consts) {
                problems.push(format!("{}: {read}", path.display()));
            }
        }
    }
    assert_holds(&problems);
}

#[test]
fn the_credential_rule_sees_literals_constants_and_the_resolver() {
    let consts = BTreeMap::from([("KEY_VAR".to_string(), "ACME_API_KEY".to_string())]);
    for src in [
        "fn f() { std::env::var(\"ACME_API_KEY\"); }",
        "fn f() { env::var_os(\"GOOGLE_OAUTH_ACCESS_TOKEN\"); }",
        "fn f() { std::env::var(KEY_VAR); }",
        "fn f() { std::env::var(crate::config::ACCESS_TOKEN_ENV_VAR); }",
        "fn f() { voice_pipes_sdk::secrets::resolve_key(k, v, e); }",
    ] {
        assert!(!credential_reads(&syntax(src), &consts).is_empty(), "{src}");
    }
    for src in [
        "fn f() { std::env::var(\"GOOGLE_CLOUD_PROJECT\"); }",
        "fn f() { std::env::var(PROJECT_VAR); }",
        "// std::env::var(\"ACME_API_KEY\")\nfn f() {}",
        "fn f() { configured_key(cfg.api_key.as_ref()); }",
    ] {
        assert!(credential_reads(&syntax(src), &consts).is_empty(), "{src}");
    }
}

/// The public enums in `file` without `#[non_exhaustive]`, outside test code.
fn exhaustive_public_enums(file: &syn::File) -> Vec<String> {
    struct Enums(Vec<String>);
    impl Visit<'_> for Enums {
        fn visit_item_mod(&mut self, i: &syn::ItemMod) {
            if !is_test_only(&i.attrs) {
                syn::visit::visit_item_mod(self, i);
            }
        }
        fn visit_item_enum(&mut self, i: &syn::ItemEnum) {
            let exhaustive = !i.attrs.iter().any(|a| a.path().is_ident("non_exhaustive"));
            if matches!(i.vis, syn::Visibility::Public(_)) && exhaustive {
                self.0.push(i.ident.to_string());
            }
        }
    }
    let mut e = Enums(Vec::new());
    e.visit_file(file);
    e.0
}

/// Every public enum in the SDK is `#[non_exhaustive]`, so a variant added
/// later does not break a plugin that matches on it.
#[test]
fn the_sdk_enums_can_grow() {
    let mut problems = Vec::new();
    for file in crate_sources("voice-pipes-sdk") {
        if is_test_file(&file) {
            continue;
        }
        for name in exhaustive_public_enums(&parse(&read(&file), &file)) {
            problems.push(format!("{}: pub enum {name}", file.display()));
        }
    }
    assert_holds(&problems);
}

#[test]
fn the_enum_rule_reads_the_attribute() {
    assert_eq!(
        exhaustive_public_enums(&syntax("pub enum A { X }")),
        vec!["A".to_string()]
    );
    for src in [
        "#[non_exhaustive] pub enum A { X }",
        "#[derive(Debug)]\n#[non_exhaustive]\npub enum A { X }",
        "enum Private { X }",
        "pub(crate) enum Inner { X }",
        "#[cfg(test)] mod tests { pub enum T { X } }",
    ] {
        assert!(exhaustive_public_enums(&syntax(src)).is_empty(), "{src}");
    }
}

/// What a crate root makes public: its `pub` modules and items, and each
/// name a `pub use` re-exports (a glob as `path::*`). Test-only items are
/// not API.
fn public_surface(file: &syn::File) -> Vec<String> {
    fn use_leaves(tree: &syn::UseTree, prefix: &str, out: &mut Vec<String>) {
        match tree {
            syn::UseTree::Path(p) => use_leaves(&p.tree, &format!("{prefix}{}::", p.ident), out),
            syn::UseTree::Name(n) => out.push(n.ident.to_string()),
            syn::UseTree::Rename(r) => out.push(r.rename.to_string()),
            syn::UseTree::Glob(_) => out.push(format!("{prefix}*")),
            syn::UseTree::Group(g) => {
                for t in &g.items {
                    use_leaves(t, prefix, out);
                }
            }
        }
    }
    let public = |v: &syn::Visibility| matches!(v, syn::Visibility::Public(_));
    let mut out = Vec::new();
    for item in &file.items {
        let (vis, attrs, name) = match item {
            syn::Item::Mod(i) => (&i.vis, &i.attrs, Some(format!("mod {}", i.ident))),
            syn::Item::Fn(i) => (&i.vis, &i.attrs, Some(i.sig.ident.to_string())),
            syn::Item::Struct(i) => (&i.vis, &i.attrs, Some(i.ident.to_string())),
            syn::Item::Enum(i) => (&i.vis, &i.attrs, Some(i.ident.to_string())),
            syn::Item::Const(i) => (&i.vis, &i.attrs, Some(i.ident.to_string())),
            syn::Item::Static(i) => (&i.vis, &i.attrs, Some(i.ident.to_string())),
            syn::Item::Type(i) => (&i.vis, &i.attrs, Some(i.ident.to_string())),
            syn::Item::Trait(i) => (&i.vis, &i.attrs, Some(i.ident.to_string())),
            syn::Item::Union(i) => (&i.vis, &i.attrs, Some(i.ident.to_string())),
            syn::Item::Use(i) => (&i.vis, &i.attrs, None),
            _ => continue,
        };
        if !public(vis) || is_test_only(attrs) {
            continue;
        }
        match (name, item) {
            (Some(name), _) => out.push(name),
            (None, syn::Item::Use(u)) => use_leaves(&u.tree, "", &mut out),
            (None, _) => {}
        }
    }
    out
}

/// What every provider exposes: its registration entry points.
const ALWAYS_PUBLIC: &[&str] = &["factory", "wizard_choices"];

/// What else a provider exposes, and who uses it: every other module and
/// item is private, so a provider's API is its `factory()`,
/// `wizard_choices()` and what its own examples, tests or the bench drive.
const PUBLIC_API: &[(&str, &[&str])] = &[
    // The WER, offline and streaming examples drive the pipeline's parts.
    (
        "voice-pipes-stt-nemotron",
        &[
            "mod mel_native",
            "mod streaming",
            "mod vocab",
            "NemotronStt",
        ],
    ),
    // The token-identity and feature tests drive the engine directly.
    (
        "voice-pipes-stt-nemotron35",
        &["mod engine", "mod features", "mod model"],
    ),
    // The bench runs VAP directly in its turn-detection bake-off.
    ("voice-pipes-turn-vap", &["VapTurnDetector"]),
    // Providers and configs the crates' own integration tests build.
    ("voice-pipes-tts-kokoro", &["KokoroTts"]),
    (
        "voice-pipes-llm-openai-compat",
        &["OpenAiCompatConfig", "OpenAiCompatProcessor"],
    ),
];

/// The public names a crate root exposes beyond those allowed.
fn unlisted_public(file: &syn::File, allowed: &[&str]) -> Vec<String> {
    public_surface(file)
        .into_iter()
        .filter(|name| !ALWAYS_PUBLIC.contains(&name.as_str()) && !allowed.contains(&name.as_str()))
        .collect()
}

/// A provider exposes nothing beyond the lists above, so its internals do
/// not become API by accident; and each listed name is still exposed, so
/// the list does not outlive what it describes.
#[test]
fn providers_expose_only_the_listed_api() {
    for p in providers() {
        let lib = root().join(&p).join("src/lib.rs");
        let file = parse(&read(&lib), &lib);
        let allowed: Vec<&str> = PUBLIC_API
            .iter()
            .filter(|(crate_name, _)| *crate_name == p)
            .flat_map(|(_, names)| names.iter().copied())
            .collect();
        let extra = unlisted_public(&file, &allowed);
        assert!(
            extra.is_empty(),
            "{p} exposes {extra:?}; make them pub(crate) or list them in PUBLIC_API with who uses them"
        );
        let surface = public_surface(&file);
        let stale: Vec<_> = allowed
            .iter()
            .filter(|a| !surface.iter().any(|s| s == *a))
            .collect();
        assert!(
            stale.is_empty(),
            "PUBLIC_API lists {stale:?} for {p}, which it no longer exposes"
        );
    }
}

#[test]
fn the_api_rule_sees_modules_items_and_re_exports() {
    let file = syntax(
        r#"
        pub mod open;
        pub(crate) mod internal;
        mod private;
        #[cfg(test)]
        pub mod tests;
        pub use inner::{Config, Engine as Core, deep::*};
        pub(crate) use inner::Hidden;
        pub fn factory() {}
        pub struct Provider;
        pub const NAME: &str = "x";
        fn helper() {}
    "#,
    );
    assert_eq!(
        public_surface(&file),
        [
            "mod open",
            "Config",
            "Core",
            "inner::deep::*",
            "factory",
            "Provider",
            "NAME"
        ]
    );
    assert_eq!(
        unlisted_public(&file, &["mod open", "Config"]),
        ["Core", "inner::deep::*", "Provider", "NAME"]
    );
}

// ── The binary's wiring ─────────────────────────────────────

fn strings(v: &toml::Value) -> Vec<String> {
    v.as_array()
        .expect("an array of strings")
        .iter()
        .filter_map(|s| s.as_str().map(str::to_string))
        .collect()
}

/// Everything `feature` switches on in `features`, transitively: other
/// features of the same crate, and entries naming a dependency (`dep:x`,
/// `x/f`, `x?/f`) or a dependency's implicit feature (`x`). A cycle ends
/// where it closes.
fn feature_closure(features: &toml::Table, feature: &str) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut stack = vec![feature.to_string()];
    while let Some(f) = stack.pop() {
        let Some(entries) = features.get(&f) else {
            continue;
        };
        for e in strings(entries) {
            if seen.insert(e.clone()) && features.contains_key(&e) {
                stack.push(e);
            }
        }
    }
    seen
}

/// The crate a feature entry enables, if it names one: `dep:x`, `x/f` or
/// `x?/f`.
fn enabled_crate(entry: &str) -> Option<&str> {
    entry
        .strip_prefix("dep:")
        .or_else(|| entry.split_once("?/").map(|(c, _)| c))
        .or_else(|| entry.split_once('/').map(|(c, _)| c))
}

/// What the binary builds by default, checked against the two lists that
/// define it: the engine features its defaults reach must be the engine's
/// `batteries` (both followed through the features they enable), and the
/// provider crates they reach must be exactly those of the plugins named in
/// `[package.metadata.voice-pipes] default-plugins`. Returns every way the
/// manifests disagree.
fn default_feature_problems(cli: &toml::Table, engine: &toml::Table) -> Vec<String> {
    let mut problems = Vec::new();
    let features = cli["features"].as_table().expect("CLI features");
    let engine_features = engine["features"].as_table().expect("engine features");
    let listed: BTreeSet<String> = cli
        .get("package")
        .and_then(|p| p.get("metadata"))
        .and_then(|m| m.get("voice-pipes"))
        .and_then(|v| v.get("default-plugins"))
        .map(|l| strings(l).into_iter().collect())
        .unwrap_or_default();

    let reached = feature_closure(features, "default");
    let forwarded: BTreeSet<String> = reached
        .iter()
        .filter_map(|e| e.strip_prefix("voice-pipes/"))
        .flat_map(|f| {
            let mut all = feature_closure(engine_features, f);
            all.insert(f.to_string());
            all
        })
        .filter(|f| engine_features.contains_key(f) && f != "batteries")
        .collect();
    let mut batteries = feature_closure(engine_features, "batteries");
    batteries.retain(|f| engine_features.contains_key(f));
    if forwarded != batteries {
        problems.push(format!(
            "the CLI's defaults reach engine features {forwarded:?}, not the engine's `batteries` {batteries:?}"
        ));
    }

    let default_providers: BTreeSet<String> = reached
        .iter()
        .filter_map(|e| enabled_crate(e))
        .filter(|c| is_provider(c))
        .map(str::to_string)
        .collect();
    let mut listed_providers = BTreeSet::new();
    for p in &listed {
        if !features.contains_key(p) {
            problems.push(format!("default plugin `{p}` is not a feature of the CLI"));
            continue;
        }
        let crates: BTreeSet<String> = feature_closure(features, p)
            .iter()
            .filter_map(|e| enabled_crate(e))
            .filter(|c| is_provider(c))
            .map(str::to_string)
            .collect();
        if crates.is_empty() {
            problems.push(format!("default plugin `{p}` enables no provider crate"));
        }
        if !reached.contains(p) {
            problems.push(format!("default plugin `{p}` is not in the CLI's defaults"));
        }
        listed_providers.extend(crates);
    }
    if default_providers != listed_providers {
        problems.push(format!(
            "the CLI's defaults enable providers {default_providers:?}, but `default-plugins` names {listed_providers:?}"
        ));
    }
    if !strings(&engine["features"]["default"]).is_empty() {
        problems.push("the engine must have no default features".into());
    }
    problems
}

/// The binary's default features are the engine's `batteries` plus the
/// plugins it lists: `-p voice-pipes --features batteries` (the engine's own
/// tests, an embedder) builds what `cargo install` builds of the engine, and
/// a plugin is a default only by being named.
#[test]
fn the_cli_defaults_are_the_batteries_and_the_listed_plugins() {
    assert_holds(&default_feature_problems(
        &manifest("voice-pipes-cli"),
        &manifest("voice-pipes"),
    ));
}

/// The rule has teeth: a plugin made a default without being listed, a
/// listed plugin that enables no provider, a battery left out, and a
/// provider reached only through another feature are each reported.
#[test]
fn the_default_features_rule_catches_each_drift() {
    let engine = toml(
        r#"[features]
default = []
batteries = ["stt-parakeet", "tts"]
tts = ["tts-piper"]
tts-piper = []
stt-parakeet = []"#,
    );
    let cli = |default: &str, listed: &str| -> toml::Table {
        toml(&format!(
            r#"[package.metadata.voice-pipes]
default-plugins = [{listed}]
[features]
default = [{default}]
engine = ["voice-pipes/batteries"]
stt-parakeet = ["voice-pipes/stt-parakeet"]
tts = ["voice-pipes/tts"]
tts-kokoro = ["dep:voice-pipes-tts-kokoro"]
bundle = ["tts-kokoro", "voice-pipes-stt-google?/grpc"]
fast = ["dep:some-helper"]"#
        ))
    };
    assert_holds(&default_feature_problems(
        &cli(r#""stt-parakeet", "tts", "tts-kokoro""#, r#""tts-kokoro""#),
        &engine,
    ));
    assert_holds(&default_feature_problems(
        &cli(r#""engine", "tts-kokoro""#, r#""tts-kokoro""#),
        &engine,
    ));

    let unlisted = cli(r#""stt-parakeet", "tts", "tts-kokoro""#, "");
    assert_reports(
        &default_feature_problems(&unlisted, &engine),
        "enable providers",
    );

    let not_a_provider = cli(r#""stt-parakeet", "tts", "fast""#, r#""fast""#);
    assert_reports(
        &default_feature_problems(&not_a_provider, &engine),
        "enables no provider",
    );

    let missing_battery = cli(r#""stt-parakeet""#, "");
    assert_reports(
        &default_feature_problems(&missing_battery, &engine),
        "batteries",
    );

    let hidden = cli(r#""engine", "bundle""#, r#""tts-kokoro""#);
    assert_reports(
        &default_feature_problems(&hidden, &engine),
        "voice-pipes-stt-google",
    );

    let listed_not_default = cli(r#""engine""#, r#""tts-kokoro""#);
    assert_reports(
        &default_feature_problems(&listed_not_default, &engine),
        "not in the CLI's defaults",
    );
}

/// Every provider member can reach the binary: an optional dependency of
/// the CLI, a feature that enables it, and a call into it where the CLI
/// registers its plugins. A provider missing any of these is built and
/// tested but never shipped.
fn provider_wiring_problems(
    cli: &toml::Table,
    registration: &syn::File,
    providers: &[String],
) -> Vec<String> {
    struct Calls(BTreeSet<String>);
    impl Calls {
        /// The identifiers in a macro's arguments, where a registration
        /// written through a local macro names the crate.
        fn tokens(&mut self, tokens: proc_macro2::TokenStream) {
            for token in tokens {
                match token {
                    proc_macro2::TokenTree::Ident(ident) => {
                        self.0.insert(ident.to_string());
                    }
                    proc_macro2::TokenTree::Group(group) => self.tokens(group.stream()),
                    _ => {}
                }
            }
        }
    }
    impl Visit<'_> for Calls {
        fn visit_path(&mut self, i: &syn::Path) {
            if let Some(first) = i.segments.first() {
                self.0.insert(first.ident.to_string());
            }
            syn::visit::visit_path(self, i);
        }
        fn visit_macro(&mut self, i: &syn::Macro) {
            self.tokens(i.tokens.clone());
            syn::visit::visit_macro(self, i);
        }
    }
    let mut calls = Calls(BTreeSet::new());
    calls.visit_file(registration);
    let deps = cli.get("dependencies").and_then(toml::Value::as_table);
    let features = cli.get("features").and_then(toml::Value::as_table);
    let mut problems = Vec::new();
    for p in providers {
        let optional = deps
            .and_then(|d| d.get(p))
            .and_then(|spec| spec.get("optional"))
            .and_then(toml::Value::as_bool)
            == Some(true);
        if !optional {
            problems.push(format!(
                "{p} is not an optional dependency of voice-pipes-cli"
            ));
        }
        let featured = features.is_some_and(|f| {
            f.values()
                .flat_map(strings)
                .any(|e| enabled_crate(&e) == Some(p.as_str()))
        });
        if !featured {
            problems.push(format!("no voice-pipes-cli feature enables {p}"));
        }
        if !calls.0.contains(&p.replace('-', "_")) {
            problems.push(format!("voice-pipes-cli never registers {p}"));
        }
    }
    problems
}

#[test]
fn every_provider_is_wired_into_the_binary() {
    let main = root().join("voice-pipes-cli/src/main.rs");
    assert_holds(&provider_wiring_problems(
        &manifest("voice-pipes-cli"),
        &parse(&read(&main), &main),
        &providers(),
    ));
}

#[test]
fn the_wiring_rule_catches_each_missing_link() {
    let providers = ["voice-pipes-tts-x".to_string()];
    let cli =
        |dep: &str, feature: &str| toml(&format!("[dependencies]\n{dep}\n[features]\n{feature}"));
    let registers =
        syntax("fn register() { voice_pipes::tts::register(voice_pipes_tts_x::factory); }");
    let wired = cli(
        "voice-pipes-tts-x = { path = \"x\", optional = true }",
        "tts-x = [\"dep:voice-pipes-tts-x\"]",
    );
    assert_holds(&provider_wiring_problems(&wired, &registers, &providers));
    assert_reports(
        &provider_wiring_problems(
            &cli(
                "voice-pipes-tts-x = { path = \"x\" }",
                "tts-x = [\"dep:voice-pipes-tts-x\"]",
            ),
            &registers,
            &providers,
        ),
        "not an optional dependency",
    );
    assert_reports(
        &provider_wiring_problems(
            &cli(
                "voice-pipes-tts-x = { path = \"x\", optional = true }",
                "other = []",
            ),
            &registers,
            &providers,
        ),
        "no voice-pipes-cli feature",
    );
    let comment_only = syntax("// voice_pipes_tts_x::factory\nfn register() {}");
    assert_reports(
        &provider_wiring_problems(&wired, &comment_only, &providers),
        "never registers",
    );
}

// ── Images ──────────────────────────────────────────────────

/// The list a compose `args` item sets: plain (`- FEATURES=...`) or the
/// anchored definition (`- &name FEATURES=...`). An alias (`- *name`)
/// repeats a list already read.
fn features_item(line: &str) -> Option<&str> {
    let item = line.trim().strip_prefix("- ")?;
    let item = match item.strip_prefix('&') {
        Some(anchored) => anchored.split_once(' ')?.1.trim_start(),
        None => item,
    };
    item.strip_prefix("FEATURES=")
}

/// The features the compose file builds the images with, unioned in order:
/// the workers' list, defined once, and the smaller lists of the other
/// images. The same reading `scripts/image-features.py` makes, which CI
/// tests with.
fn compose_feature_lists(compose: &str) -> Vec<String> {
    let mut all: Vec<String> = Vec::new();
    for list in compose.lines().filter_map(features_item) {
        for f in list.split(',').map(str::trim).filter(|f| !f.is_empty()) {
            if !all.iter().any(|x| x == f) {
                all.push(f.to_string());
            }
        }
    }
    all
}

/// Why a Dockerfile's default `FEATURES` is not `expected`, if it is not:
/// a plain `docker build` must produce the image that ships.
fn dockerfile_feature_problem(name: &str, dockerfile: &str, expected: &[String]) -> Option<String> {
    let Some(value) = dockerfile
        .lines()
        .find_map(|l| l.trim().strip_prefix("ARG FEATURES="))
    else {
        return Some(format!("{name} declares no ARG FEATURES default"));
    };
    let declared: BTreeSet<&str> = value
        .trim_matches('"')
        .split(',')
        .map(str::trim)
        .filter(|f| !f.is_empty())
        .collect();
    let wanted: BTreeSet<&str> = expected.iter().map(String::as_str).collect();
    (declared != wanted).then(|| {
        format!(
            "{name} builds {declared:?} by default, but the compose file builds that image with {wanted:?}"
        )
    })
}

/// The GPU switches the CUDA image builds on top of the workers' list.
const GPU_SWITCHES: &str = "cuda,ort-cuda";

/// Why the CUDA Dockerfile does not build the workers' list with the GPU
/// switches on top, if it does not.
fn gpu_switches_problem(dockerfile: &str) -> Option<String> {
    let declared = dockerfile
        .lines()
        .find_map(|l| l.trim().strip_prefix("ARG GPU_FEATURES="))
        .map(|v| v.trim_matches('"'));
    if declared != Some(GPU_SWITCHES) {
        return Some(format!(
            "Dockerfile.cuda must declare ARG GPU_FEATURES=\"{GPU_SWITCHES}\", has {declared:?}"
        ));
    }
    (!dockerfile.contains("--features \"${GPU_FEATURES},${FEATURES}\"")).then(|| {
        "Dockerfile.cuda must build with --features \"${GPU_FEATURES},${FEATURES}\"".to_string()
    })
}

/// Each Dockerfile builds, by default, exactly the features the compose file
/// (and CI, through `scripts/image-features.py`) uses for its image; the
/// CUDA one adds only the GPU switches.
#[test]
fn the_dockerfiles_default_to_the_image_feature_lists() {
    let read = |rel: &str| {
        std::fs::read_to_string(root().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
    };
    let features = compose_feature_lists(&read("voice-pipes/deploy/docker/docker-compose.yml"));
    assert!(!features.is_empty(), "no FEATURES lists in compose");
    for file in ["voice-pipes/Dockerfile", "voice-pipes/Dockerfile.cuda"] {
        if let Some(problem) = dockerfile_feature_problem(file, &read(file), &features) {
            panic!("{problem}");
        }
    }
    if let Some(problem) = gpu_switches_problem(&read("voice-pipes/Dockerfile.cuda")) {
        panic!("{problem}");
    }
}

/// The rule has teeth: a missing, empty or drifted default is reported, the
/// order of the list does not matter, an anchored list is read and its
/// aliases add nothing, and the CUDA image's switches are held.
#[test]
fn the_dockerfile_rule_catches_each_drift() {
    let features = compose_feature_lists(
        "    - &w FEATURES=realtime,tts-kokoro\n    - *w\n    - FEATURES=realtime,turn-smart\n",
    );
    assert_eq!(features, ["realtime", "tts-kokoro", "turn-smart"]);
    let check = |text: &str| dockerfile_feature_problem("Dockerfile", text, &features);
    assert!(check("ARG FEATURES=\"turn-smart,realtime,tts-kokoro\"\n").is_none());
    assert!(check("FROM rust\n").is_some(), "no default");
    assert!(check("ARG FEATURES=\"\"\n").is_some(), "an empty default");
    assert!(
        check("ARG FEATURES=realtime,tts-kokoro\n").is_some(),
        "a missing feature"
    );
    let good = "ARG GPU_FEATURES=\"cuda,ort-cuda\"\nRUN cargo build --features \"${GPU_FEATURES},${FEATURES}\"\n";
    assert!(gpu_switches_problem(good).is_none());
    assert!(gpu_switches_problem("RUN cargo build --features \"${FEATURES}\"\n").is_some());
    assert!(
        gpu_switches_problem(&good.replace("${GPU_FEATURES},", "")).is_some(),
        "switches declared but not built"
    );
}

// ── Function length ─────────────────────────────────────────

/// Code lines past which a function is too long to read as one thing.
/// Clippy's `too_many_lines` holds the same line for plain functions, but
/// it does not count an `async fn`'s body, where the longest ones are.
const MAX_FUNCTION_LINES: usize = 100;

/// Functions over [`MAX_FUNCTION_LINES`] today, by file and name, to be
/// split (the plan's 3.6) and removed from here. A new long function fails;
/// one that is split and still listed fails too, so the list only shrinks.
const LONG_FUNCTIONS: &[(&str, &str)] = &[
    ("voice-pipes-bench/src/http.rs", "run_experiment"),
    (
        "voice-pipes-bench/src/runner/barge_in.rs",
        "collect_voice_barge_in",
    ),
    (
        "voice-pipes-bench/src/runner/barge_in.rs",
        "run_barge_in_session",
    ),
    ("voice-pipes-bench/src/runner/collect.rs", "collect_outcome"),
    (
        "voice-pipes-bench/src/runner/collect.rs",
        "collect_outcome_audio_bridge",
    ),
    (
        "voice-pipes-bench/src/runner/conversation.rs",
        "run_one_turn",
    ),
    ("voice-pipes-bench/src/runner/mod.rs", "run_experiment"),
    ("voice-pipes-bench/src/runner/mod.rs", "run_scenario"),
    ("voice-pipes-bench/src/runner/summary.rs", "compute_summary"),
    (
        "voice-pipes-bench/src/runner/synthetic_caller.rs",
        "run_synthetic_caller_scenario",
    ),
    (
        "voice-pipes-bench/src/runner/synthetic_caller.rs",
        "run_synthetic_caller_session",
    ),
    (
        "voice-pipes-bench/src/runner/tts_only.rs",
        "aggregate_tts_only_outcomes",
    ),
    ("voice-pipes-cli/src/main.rs", "run"),
    (
        "voice-pipes-llm-gguf/src/generation.rs",
        "process_stream_inner",
    ),
    ("voice-pipes-llm-gguf/src/schema.rs", "schema"),
    ("voice-pipes-llm-openai-compat/src/schema.rs", "schema"),
    ("voice-pipes-s2s-elevenlabs/src/lib.rs", "parse_event"),
    ("voice-pipes-s2s-gemini/src/lib.rs", "build_session_update"),
    ("voice-pipes-s2s-gemini/src/lib.rs", "parse_event"),
    ("voice-pipes-s2s-openai/src/lib.rs", "parse_event"),
    ("voice-pipes-stt-assemblyai/src/schema.rs", "schema"),
    ("voice-pipes-stt-google/src/schema.rs", "schema"),
    (
        "voice-pipes-stt-nemotron/src/provider.rs",
        "transcribe_stream",
    ),
    (
        "voice-pipes-stt-nemotron35/src/provider.rs",
        "transcribe_stream",
    ),
    ("voice-pipes/src/agent/gateway.rs", "pump"),
    ("voice-pipes/src/agent/tool_backend.rs", "pump"),
    (
        "voice-pipes/src/api/orchestrator/pipeline_graph.rs",
        "build_pipeline_graph",
    ),
    ("voice-pipes/src/api/probe.rs", "probe_vad"),
    ("voice-pipes/src/audio/io.rs", "run_mic_stream_loop"),
    ("voice-pipes/src/audit/streaming.rs", "write_event"),
    ("voice-pipes/src/bin/component_server.rs", "main"),
    ("voice-pipes/src/commands/check.rs", "check_stt"),
    ("voice-pipes/src/commands/interactive.rs", "run_client_loop"),
    ("voice-pipes/src/commands/live.rs", "run_live"),
    ("voice-pipes/src/commands/serve.rs", "bootstrap_transport"),
    (
        "voice-pipes/src/commands/serve.rs",
        "spawn_orchestrator_client",
    ),
    (
        "voice-pipes/src/pipeline/orchestrate.rs",
        "process_utterance",
    ),
    ("voice-pipes/src/pipeline/speculative.rs", "handle_partial"),
    ("voice-pipes/src/pipeline/tts.rs", "spawn_synthesis_task"),
    ("voice-pipes/src/pipeline/tts.rs", "tts_task"),
    ("voice-pipes/src/pipeline/vad.rs", "vad_filter_task"),
    ("voice-pipes/src/realtime/client.rs", "connect_with"),
    ("voice-pipes-stt-moonshine/src/lib.rs", "transcribe_stream"),
    ("voice-pipes-stt-parakeet/src/lib.rs", "transcribe_stream"),
    (
        "voice-pipes/src/transport/serve/realtime_runner/context/provider_handlers.rs",
        "handle_agent_event",
    ),
    (
        "voice-pipes/src/transport/serve/realtime_runner/context/provider_handlers.rs",
        "handle_provider_closed",
    ),
    (
        "voice-pipes/src/transport/serve/realtime_runner/context/provider_handlers.rs",
        "handle_provider_response_completed",
    ),
    (
        "voice-pipes/src/transport/serve/realtime_runner/mod.rs",
        "run_realtime_session_inner",
    ),
    (
        "voice-pipes/src/transport/serve/runner/context.rs",
        "handle_agent_event",
    ),
    (
        "voice-pipes/src/transport/serve/runner/context.rs",
        "handle_server_barge_in",
    ),
    (
        "voice-pipes/src/transport/serve/runner/context.rs",
        "handle_utterance",
    ),
    (
        "voice-pipes/src/transport/serve/runner/mod.rs",
        "run_session_inner",
    ),
    (
        "voice-pipes/src/transport/serve/runner/utils.rs",
        "handle_tts_only",
    ),
    (
        "voice-pipes/src/transport/serve/setup.rs",
        "negotiate_and_announce",
    ),
    (
        "voice-pipes/src/transport/serve/setup.rs",
        "perform_handshake",
    ),
    (
        "voice-pipes/src/transport/serve/setup.rs",
        "setup_audio_pipeline",
    ),
    ("voice-pipes/src/transport/ws/mod.rs", "new"),
    ("voice-pipes/src/transport/ws/mod.rs", "send"),
];

/// The functions in `file` (its source `text`) over `max` code lines: lines
/// of the body that hold more than whitespace or a comment. Test code, by
/// attribute or by module, is not counted; a test reads as a script.
fn long_functions(file: &syn::File, text: &str, max: usize) -> Vec<(String, usize)> {
    struct Long<'a> {
        lines: Vec<&'a str>,
        max: usize,
        found: Vec<(String, usize)>,
    }
    impl Long<'_> {
        fn measure(&mut self, name: &syn::Ident, attrs: &[syn::Attribute], body: &syn::Block) {
            let is_test = attrs.iter().any(|a| {
                a.path().is_ident("test")
                    || a.path().segments.last().is_some_and(|s| s.ident == "test")
            });
            if is_test || is_test_only(attrs) {
                return;
            }
            let span = body.brace_token.span.join();
            let (start, end) = (span.start().line, span.end().line);
            let code = self.lines[start.saturating_sub(1)..end.min(self.lines.len())]
                .iter()
                .filter(|l| {
                    let t = l.trim();
                    !t.is_empty() && !t.starts_with("//")
                })
                .count();
            if code > self.max {
                self.found.push((name.to_string(), code));
            }
        }
    }
    impl Visit<'_> for Long<'_> {
        fn visit_item_mod(&mut self, i: &syn::ItemMod) {
            if !is_test_only(&i.attrs) {
                syn::visit::visit_item_mod(self, i);
            }
        }
        fn visit_item_fn(&mut self, i: &syn::ItemFn) {
            self.measure(&i.sig.ident, &i.attrs, &i.block);
            syn::visit::visit_item_fn(self, i);
        }
        fn visit_impl_item_fn(&mut self, i: &syn::ImplItemFn) {
            self.measure(&i.sig.ident, &i.attrs, &i.block);
            syn::visit::visit_impl_item_fn(self, i);
        }
        fn visit_trait_item_fn(&mut self, i: &syn::TraitItemFn) {
            if let Some(body) = &i.default {
                self.measure(&i.sig.ident, &i.attrs, body);
            }
            syn::visit::visit_trait_item_fn(self, i);
        }
    }
    let mut long = Long {
        lines: text.lines().collect(),
        max,
        found: Vec::new(),
    };
    long.visit_file(file);
    long.found
}

/// Whether `path` is test code: a file under a `tests` directory, or a
/// crate's tests, examples or benches.
fn is_test_file(path: &Path) -> bool {
    path.components().any(|c| {
        matches!(
            c.as_os_str().to_str(),
            Some("tests" | "examples" | "benches")
        )
    })
}

/// No function in the workspace's code is longer than
/// [`MAX_FUNCTION_LINES`], except those listed while they wait to be split.
#[test]
fn functions_stay_short_enough_to_read() {
    let listed: BTreeSet<(String, String)> = LONG_FUNCTIONS
        .iter()
        .map(|(f, n)| ((*f).to_string(), (*n).to_string()))
        .collect();
    let mut found = BTreeSet::new();
    let mut unlisted = Vec::new();
    for member in members() {
        for file in crate_sources(&member) {
            let rel = file
                .strip_prefix(root())
                .expect("under the root")
                .to_string_lossy()
                .replace('\\', "/");
            if is_test_file(Path::new(&rel)) {
                continue;
            }
            let text = read(&file);
            for (name, lines) in long_functions(&parse(&text, &file), &text, MAX_FUNCTION_LINES) {
                let key = (rel.clone(), name.clone());
                if !listed.contains(&key) {
                    unlisted.push(format!("{rel}: {name} ({lines} lines)"));
                }
                found.insert(key);
            }
        }
    }
    let split: Vec<_> = listed.difference(&found).collect();
    assert!(
        unlisted.is_empty(),
        "functions over {MAX_FUNCTION_LINES} code lines; extract the steps:\n{}",
        unlisted.join("\n")
    );
    assert!(
        split.is_empty(),
        "LONG_FUNCTIONS lists functions that are no longer long; remove them: {split:?}"
    );
}

#[test]
fn the_length_rule_counts_code_lines_in_every_kind_of_function() {
    let body = |n: usize| {
        (0..n).fold(String::new(), |mut body, i| {
            let _ = writeln!(body, "    let x{i} = {i};");
            body
        })
    };
    let text = format!(
        "fn short() {{\n{}}}\n\
         async fn long_async() {{\n{}\n    // a comment\n\n}}\n\
         impl S {{ fn long_method(&self) {{\n{}}} }}\n\
         trait T {{ fn long_default() {{\n{}}} }}\n\
         #[test]\nfn long_test() {{\n{}}}\n\
         #[cfg(test)]\nmod tests {{ fn long_helper() {{\n{}}} }}\n",
        body(3),
        body(5),
        body(5),
        body(5),
        body(5),
        body(5),
    );
    let found = long_functions(&syntax(&text), &text, 5);
    let names: Vec<&str> = found.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        names,
        ["long_async", "long_method", "long_default"],
        "{found:?}"
    );
    assert!(
        found.iter().all(|(_, lines)| *lines == 7),
        "braces count, comments and blanks do not: {found:?}"
    );
}

// ── Metrics Conformance Rule (§4.7, M5) ─────────────────────

/// The helper functions from `voice_pipes_sdk::testing` that satisfy the
/// metric conformance requirement (§4.7, M5).
const CONFORMANCE_HELPERS: &[&str] = &[
    "assert_stt_metrics_conformance",
    "assert_tts_metrics_conformance",
    "assert_llm_metrics_conformance",
    "assert_vad_metrics_conformance",
    "assert_s2s_metrics_conformance",
    "two_utterances_on_one_stream",
    "back_to_back_utterances_stay_apart",
];

fn calls_conformance_helper(file: &syn::File) -> bool {
    struct Visitor {
        found: bool,
    }
    impl Visit<'_> for Visitor {
        fn visit_expr_call(&mut self, i: &syn::ExprCall) {
            if let syn::Expr::Path(p) = &*i.func
                && let Some(seg) = p.path.segments.last()
                && CONFORMANCE_HELPERS.iter().any(|h| seg.ident == *h)
            {
                self.found = true;
                return;
            }
            syn::visit::visit_expr_call(self, i);
        }
    }
    let mut v = Visitor { found: false };
    v.visit_file(file);
    v.found
}

/// Why `provider` does not run a metrics conformance helper, if it does not.
fn provider_conformance_problems(p: &str, files: &[PathBuf]) -> Vec<String> {
    let runs_helper = files.iter().any(|file| {
        let text = read(file);
        if let Ok(syntax) = syn::parse_file(&text) {
            calls_conformance_helper(&syntax)
        } else {
            false
        }
    });
    if runs_helper {
        Vec::new()
    } else {
        vec![format!(
            "{p} does not run a metrics conformance helper (assert_*_metrics_conformance)"
        )]
    }
}

/// Every provider crate runs at least one metrics conformance helper from
/// `voice_pipes_sdk::testing`, proving that its metric breakdowns reflect
/// delays and respect invariants (§4.7, M5).
#[test]
fn every_provider_runs_the_metrics_conformance_helper() {
    for p in providers() {
        let files = crate_sources(&p);
        assert_holds(&provider_conformance_problems(&p, &files));
    }
}

#[test]
fn the_metrics_conformance_rule_catches_a_provider_without_the_check() {
    let without = syntax("fn dummy_test() { assert!(true); }");
    assert!(!calls_conformance_helper(&without));

    let with = syntax(
        "fn conformance_test() { assert_stt_metrics_conformance(|| async {}, vec![], 16000, Duration::ZERO); }",
    );
    assert!(calls_conformance_helper(&with));

    let problems = provider_conformance_problems("voice-pipes-stt-dummy", &[]);
    assert_reports(&problems, "does not run a metrics conformance helper");
}
