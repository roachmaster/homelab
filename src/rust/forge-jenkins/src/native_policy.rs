//! Pure native v4 Jenkins policy. Historical v1-v3 plans are defined separately.
//! Construction and descriptor hashing perform no process, file or network I/O.

use serde_json::{json, Map, Value};

/// The v3 event profile (mirrored in forge-work's external DTOs).
pub const NATIVE_PROFILE: &str = "forge-sdlc-jenkins-v4";

/// The closed set of failure kinds a phase plan can report
/// (the Python frozenset has twelve members).
pub const FAILURE_KINDS: [&str; 12] = [
    "compilation",
    "tests",
    "browser",
    "lint",
    "formatting",
    "planning",
    "review",
    "merge",
    "infrastructure",
    "verification",
    "feedback_delivery",
    "reviewer_correction",
];

pub const ALL_FEATURES: [&str; 4] = [
    "PACKAGE=",
    "FEATURES=",
    "ALL_FEATURES=1",
    "NO_DEFAULT_FEATURES=0",
];

pub const FEATURE_VARIANTS: [(&str, &str, &str, bool); 10] = [
    ("remote-https", "forge-remote-https", "", true),
    ("ide-remote", "forge-ide", "remote", true),
    ("ide-minimal", "forge-ide", "", true),
    ("template-minimal", "forge-template", "", true),
    ("terminal-minimal", "forge-terminal", "", true),
    ("local-minimal", "forge-local", "", false),
    ("terminal-jobs", "forge-terminal", "jobs", true),
    ("terminal-disk", "forge-terminal", "disk", true),
    ("system-node", "forge-system", "node", false),
    ("web-minimal", "forge-ide-web", "", true),
];

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeCiPhase {
    pub phase_id: String,
    pub kind: String,
    pub argv: Vec<String>,
    pub logical_gate: String,
}

fn phase(phase_id: &str, kind: &str, argv: &[&str], logical_gate: &str) -> NativeCiPhase {
    NativeCiPhase {
        phase_id: phase_id.to_string(),
        kind: kind.to_string(),
        argv: argv.iter().map(|item| item.to_string()).collect(),
        logical_gate: logical_gate.to_string(),
    }
}

fn variant_flags(package: &str, features: &str) -> Vec<String> {
    vec![
        format!("PACKAGE={package}"),
        format!("FEATURES={features}"),
        "ALL_FEATURES=0".to_string(),
        "NO_DEFAULT_FEATURES=1".to_string(),
    ]
}

fn make_phase(phase_id: &str, kind: &str, target: &str, flags: &[String]) -> NativeCiPhase {
    let mut argv = vec!["/usr/bin/make".to_string(), target.to_string()];
    argv.extend(flags.iter().cloned());
    NativeCiPhase {
        phase_id: phase_id.to_string(),
        kind: kind.to_string(),
        argv,
        logical_gate: "check".to_string(),
    }
}

/// Preserve the complete v2 feature matrix, with a test-build
/// barrier. `branch_scope` is 'main' for the main pipeline; any
/// other value is the branch pipeline.
fn phases_for_scope(branch_scope: &str) -> Vec<NativeCiPhase> {
    let all: Vec<String> = ALL_FEATURES.iter().map(|item| item.to_string()).collect();
    let mut phases = vec![
        phase(
            "source-preflight",
            "verification",
            &["/usr/bin/make", "ci-preflight"],
            if branch_scope == "main" {
                "check"
            } else {
                "source-preflight"
            },
        ),
        phase(
            "script-tests",
            "tests",
            &[
                "/usr/bin/python3",
                "-B",
                "-m",
                "unittest",
                "discover",
                "-s",
                "src/rust/scripts/tests",
                "-v",
            ],
            "check",
        ),
        phase(
            "formatting",
            "formatting",
            &["/usr/bin/make", "fmt-check", "PACKAGE=", "CHANGED_ONLY=0"],
            "check",
        ),
        phase(
            "imports",
            "verification",
            &["/usr/bin/make", "check-imports"],
            "check",
        ),
        phase(
            "dependencies",
            "verification",
            &["/usr/bin/make", "check-deps"],
            "check",
        ),
    ];
    for target in ["build", "check", "test-compile", "test-release-compile"] {
        phases.push(make_phase(
            &format!("all-features-{target}"),
            "compilation",
            target,
            &all,
        ));
    }
    for (identity, package, features, _lint) in FEATURE_VARIANTS {
        let flags = variant_flags(package, features);
        phases.push(make_phase(
            &format!("{identity}-test-compile"),
            "compilation",
            "test-compile",
            &flags,
        ));
    }
    phases.push(phase(
        "ui-compilation",
        "compilation",
        &["/usr/bin/make", "ci-ui-compile"],
        "check",
    ));
    for (target, kind) in [
        ("lint", "lint"),
        ("test", "tests"),
        ("test-release", "tests"),
        ("doc", "verification"),
    ] {
        phases.push(make_phase(
            &format!("all-features-{target}"),
            kind,
            target,
            &all,
        ));
    }
    for (identity, package, features, lint) in FEATURE_VARIANTS {
        let flags = variant_flags(package, features);
        phases.push(make_phase(
            &format!("{identity}-tests"),
            "tests",
            "test",
            &flags,
        ));
        if lint {
            phases.push(make_phase(
                &format!("{identity}-lint"),
                "lint",
                "lint",
                &flags,
            ));
        }
    }
    phases.push(phase(
        "ui-contracts",
        "verification",
        &["/usr/bin/make", "ui-check"],
        "check",
    ));
    phases.push(phase(
        "browser-driver-tests",
        "tests",
        &["/usr/bin/make", "ui-qa-check"],
        "check",
    ));
    if branch_scope == "main" {
        phases.push(phase(
            "browser-journeys",
            "browser",
            &["/usr/bin/make", "ui-qa"],
            "real-browser",
        ));
        phases.push(phase(
            "backend-main",
            "tests",
            &["/usr/bin/make", "backend-suites", "SCOPE=main"],
            "backend-suites",
        ));
    } else {
        phases.push(phase(
            "backend-memory",
            "tests",
            &["/usr/bin/make", "backend-suites", "SCOPE=branch"],
            "in-memory-backend",
        ));
    }
    phases
}

fn is_int(value: &Value) -> bool {
    value.as_i64().is_some() || value.as_u64().is_some()
}

/// Bind observed exit codes to the closed plan, never to stdout
/// markers. Returns the stage-evidence object
/// `{failure_kind, passed_kinds, phase_results}`.
pub fn native_stage_evidence(
    branch_scope: &str,
    checks: &Value,
    failure: Option<&str>,
) -> Result<Value, String> {
    let phases = native_phases_for_scope(branch_scope)?;
    if let Some(reason) = failure {
        if reason.is_empty() {
            return Err("Invalid typed phase failure reason".to_string());
        }
    }
    let checks = checks
        .as_array()
        .filter(|items| items.len() <= phases.len())
        .ok_or_else(|| "Invalid v4 phase execution count".to_string())?;
    let mut records: Vec<Value> = Vec::new();
    for (index, check) in checks.iter().enumerate() {
        let phase = &phases[index];
        let object = check.as_object();
        let valid = object.is_some_and(|object| {
            object.len() == 3
                && object.contains_key("index")
                && object.contains_key("argv")
                && object.contains_key("returncode")
                && is_int(&object["index"])
                && object["index"].as_i64() == Some(index as i64)
                && object["argv"]
                    == Value::Array(
                        phase
                            .argv
                            .iter()
                            .map(|item| Value::String(item.clone()))
                            .collect(),
                    )
                && is_int(&object["returncode"])
                && object["returncode"]
                    .as_i64()
                    .is_some_and(|code| (-255..=255).contains(&code))
                && (index == checks.len() - 1 || object["returncode"].as_i64() == Some(0))
        });
        if !valid {
            return Err("Phase result does not match actual closed execution order".to_string());
        }
        let mut record = Map::new();
        record.insert("index".to_string(), json!(index));
        record.insert("phase_id".to_string(), json!(phase.phase_id));
        record.insert("kind".to_string(), json!(phase.kind));
        record.insert("argv".to_string(), check["argv"].clone());
        record.insert("returncode".to_string(), check["returncode"].clone());
        records.push(Value::Object(record));
    }
    let failed: Option<Value> = records
        .last()
        .filter(|record| record["returncode"].as_i64() != Some(0))
        .cloned();
    if failure.is_none() && (failed.is_some() || records.len() != phases.len()) {
        return Err("Incomplete phase execution cannot claim success".to_string());
    }
    let mut passed: Vec<&str> = Vec::new();
    for phase in &phases {
        if passed.contains(&phase.kind.as_str()) {
            continue;
        }
        let kind_phases: Vec<usize> = phases
            .iter()
            .enumerate()
            .filter(|(_, item)| item.kind == phase.kind)
            .map(|(index, _)| index)
            .collect();
        if kind_phases.iter().all(|index| {
            *index < records.len() && records[*index]["returncode"].as_i64() == Some(0)
        }) {
            passed.push(phase.kind.as_str());
        }
    }
    passed.sort_unstable();
    let records_value = Value::Array(records);
    if failure == Some("source_changed_during_checks") {
        return Ok(json!({
            "failure_kind": "verification",
            "passed_kinds": [],
            "phase_results": records_value,
        }));
    }
    // The failed record's kind is one of the closed phase kinds by
    // construction; resolve it directly from the record.
    let kind: Option<String> = if matches!(
        failure,
        Some("cancelled_or_timed_out")
            | Some("verification_unavailable")
            | Some("deadline_exceeded")
            | Some("gate_manifest_mismatch")
    ) {
        Some("infrastructure".to_string())
    } else if let Some(record) = failed {
        let code = record["returncode"].as_i64().unwrap_or(0);
        if code == 124 || code < 0 {
            Some("infrastructure".to_string())
        } else {
            Some(
                record["kind"]
                    .as_str()
                    .unwrap_or("infrastructure")
                    .to_string(),
            )
        }
    } else if failure.is_some() {
        Some("verification".to_string())
    } else {
        None
    };
    Ok(json!({
        "failure_kind": kind,
        "passed_kinds": passed,
        "phase_results": records_value,
    }))
}

/// Policy/runner/verifier authority is protected as source, not by inferred labels.
pub const NATIVE_PROTECTED_PATHS: &[&str] =
    &["Makefile", "src/rust/Cargo.toml", "src/rust/Cargo.lock"];
pub const NATIVE_PROTECTED_PREFIXES: &[&str] = &[
    ".github/",
    ".cargo/",
    "tests/",
    ".config/",
    "src/rust/scripts/",
    "src/rust/forge-ci-verifier/",
    "src/rust/forge-jenkins/",
    "src/rust/forge-work-publication/",
    "src/rust/forge-work-worker/",
    "src/rust/forge-work/",
    "src/rust/forge-postgres/",
    "src/rust/forge-portal-release-actor/",
    "deploy/mission-control/sql/",
    "deploy/mission-control/jenkins/",
];

fn scope_kind(scope: &str) -> Result<&'static str, String> {
    if scope == "main" {
        return Ok("main");
    }
    if scope == "detached" {
        return Ok("branch");
    }
    let slug = scope
        .strip_prefix("branch/")
        .ok_or("Invalid native CI scope")?;
    let edge = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit();
    if slug.is_empty()
        || slug.len() > 58
        || slug.contains("..")
        || !slug.starts_with(edge)
        || !slug.ends_with(edge)
        || !slug
            .chars()
            .all(|c| edge(c) || matches!(c, '.' | '_' | '-'))
    {
        return Err("Invalid native CI scope".into());
    }
    Ok("branch")
}
pub fn native_phases_for_scope(scope: &str) -> Result<Vec<NativeCiPhase>, String> {
    scope_kind(scope)?;
    Ok(phases_for_scope(scope))
}
pub fn native_gate_contract(scope: &str) -> Result<String, String> {
    scope_kind(scope)?;
    Ok(format!("{NATIVE_PROFILE}:{scope}"))
}
pub fn native_coverage(scope: &str) -> Result<Value, String> {
    let kind = scope_kind(scope)?;
    Ok(
        json!({"scope":kind,"browser":if kind=="main"{"required"}else{"not_run"},
        "database":if kind=="main"{"required_zero_skips"}else{"not_run"},
        "backend_suites":if kind=="main"{"rust"}else{"rust_memory"}}),
    )
}
pub const NATIVE_PROTECTED_SUBSTRINGS: &[&str] = &["/.cargo/", "/tests/", "/.config/"];
pub const NATIVE_PROTECTED_NAME_PREFIXES: &[&str] = &["test_"];
pub const NATIVE_PROTECTED_BASENAMES: &[&str] = &[
    "Makefile",
    "makefile",
    "GNUmakefile",
    "Cargo.toml",
    "Cargo.lock",
    "build.rs",
    "rust-toolchain",
    "rust-toolchain.toml",
    "clippy.toml",
    ".clippy.toml",
    "rustfmt.toml",
    ".rustfmt.toml",
    "deny.toml",
    "work_publication_checks.py",
    "work_verification.py",
    "work_verification_init.py",
    "work_policy.py",
    "work_external_delivery.py",
];
pub fn native_policy_descriptor() -> Value {
    sorted_json(json!({"version":1,"verification_profile":NATIVE_PROFILE,
        "protected_paths":NATIVE_PROTECTED_PATHS,"protected_prefixes":NATIVE_PROTECTED_PREFIXES,
        "protected_basenames":NATIVE_PROTECTED_BASENAMES,"protected_substrings":NATIVE_PROTECTED_SUBSTRINGS,
        "protected_name_prefixes":NATIVE_PROTECTED_NAME_PREFIXES,
        "branch_phases":phases_for_scope("branch/descriptor"),"main_phases":phases_for_scope("main"),
        "branch_coverage":native_coverage("branch/descriptor").expect("fixed scope"),
        "main_coverage":native_coverage("main").expect("fixed scope")}))
}
fn sorted_json(value: Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut entries: Vec<_> = object.into_iter().collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(
                entries
                    .into_iter()
                    .map(|(key, value)| (key, sorted_json(value)))
                    .collect(),
            )
        }
        Value::Array(values) => Value::Array(values.into_iter().map(sorted_json).collect()),
        scalar => scalar,
    }
}
pub fn native_policy_sha256() -> String {
    use sha2::{Digest, Sha256};
    let raw = serde_json::to_vec(&native_policy_descriptor()).expect("closed JSON policy");
    format!("{:x}", Sha256::digest(raw))
}
pub fn native_protects(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or_default();
    NATIVE_PROTECTED_PATHS.contains(&path)
        || NATIVE_PROTECTED_PREFIXES
            .iter()
            .any(|p| path.starts_with(p))
        || NATIVE_PROTECTED_SUBSTRINGS.iter().any(|p| path.contains(p))
        || NATIVE_PROTECTED_BASENAMES.contains(&name)
        || NATIVE_PROTECTED_NAME_PREFIXES
            .iter()
            .any(|p| name.starts_with(p))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_scopes_and_backend_coverage_are_closed() {
        for (scope, count, tail) in [
            ("main", 46, "SCOPE=main"),
            ("detached", 45, "SCOPE=branch"),
            ("branch/feature-codex", 45, "SCOPE=branch"),
        ] {
            let p = native_phases_for_scope(scope).unwrap();
            assert_eq!(p.len(), count);
            assert_eq!(
                p.last().unwrap().argv,
                vec!["/usr/bin/make", "backend-suites", tail]
            );
            assert_eq!(
                native_gate_contract(scope).unwrap(),
                format!("{NATIVE_PROFILE}:{scope}")
            );
            assert!(!p.iter().any(|x| x
                .argv
                .iter()
                .any(|a| a.starts_with("deploy/") && a.ends_with(".py"))));
        }
        for bad in [
            "",
            "feature/codex",
            "branch/Bad",
            "branch/a..b",
            "branch/a;id",
            "main:extra",
        ] {
            assert!(native_phases_for_scope(bad).is_err());
        }
        assert!(native_protects("src/rust/forge-ci-verifier/src/host.rs"));
        assert!(native_protects(
            "src/rust/forge-jenkins/src/native_policy.rs"
        ));
    }
    #[test]
    fn stage_evidence_cannot_substitute_legacy_backend_or_skip_checks() {
        let phases = native_phases_for_scope("main").unwrap();
        let checks: Vec<Value> = phases
            .iter()
            .enumerate()
            .map(|(index, p)| json!({"index":index,"argv":p.argv,"returncode":0}))
            .collect();
        let evidence = native_stage_evidence("main", &json!(checks), None).unwrap();
        assert!(evidence["failure_kind"].is_null());
        let mut altered = checks.clone();
        altered.last_mut().unwrap()["argv"] = json!([
            "/usr/bin/python3",
            "-B",
            "deploy/mission-control/backend-suites.py",
            "--scope",
            "main"
        ]);
        assert!(native_stage_evidence("main", &json!(altered), None).is_err());
        assert!(native_stage_evidence("main", &json!(&checks[..45]), None).is_err());
    }
    #[test]
    fn all_common_build_and_ci_protection_conventions_are_retained() {
        for path in [
            ".github/workflows/ci.yml",
            "app/.cargo/config.toml",
            "app/tests/check.rs",
            "nested/.config/config.json",
            "src/example/Cargo.toml",
            "src/example/build.rs",
            "src/example/test_security.rs",
            "deploy/mission-control/jenkins/unit.service",
        ] {
            assert!(native_protects(path), "unprotected {path}");
        }
        assert!(!native_protects("src/rust/forge-ide-ui/src/page.rs"));
    }
}
