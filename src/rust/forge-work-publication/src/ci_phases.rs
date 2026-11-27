//! Closed v3 CI phase data; the privileged verifier mirrors these
//! literals. Port of work_ci_phases.py.
//!
//! These values select real fixed commands, never labels inferred
//! from candidate stdout. Building the plan has no I/O, registry or
//! process effects.

use serde_json::{json, Map, Value};

/// The v3 event profile (mirrored in forge-work's external DTOs).
pub const EVENT_PROFILE: &str = "forge-sdlc-jenkins-v3";

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

const ALL_FEATURES: [&str; 4] = ["PACKAGE=", "FEATURES=", "ALL_FEATURES=1", "NO_DEFAULT_FEATURES=0"];

const FEATURE_VARIANTS: [(&str, &str, &str, bool); 10] = [
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CiPhase {
    pub phase_id: String,
    pub kind: String,
    pub argv: Vec<String>,
    pub logical_gate: String,
}

fn phase(phase_id: &str, kind: &str, argv: &[&str], logical_gate: &str) -> CiPhase {
    CiPhase {
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

fn make_phase(phase_id: &str, kind: &str, target: &str, flags: &[String]) -> CiPhase {
    let mut argv = vec!["/usr/bin/make".to_string(), target.to_string()];
    argv.extend(flags.iter().cloned());
    CiPhase {
        phase_id: phase_id.to_string(),
        kind: kind.to_string(),
        argv,
        logical_gate: "check".to_string(),
    }
}

/// Preserve the complete v2 feature matrix, with a test-build
/// barrier. `branch_scope` is 'main' for the main pipeline; any
/// other value is the branch pipeline.
pub fn event_phases_for_scope(branch_scope: &str) -> Vec<CiPhase> {
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
        phase("imports", "verification", &["/usr/bin/make", "check-imports"], "check"),
        phase(
            "dependencies",
            "verification",
            &["/usr/bin/make", "check-deps"],
            "check",
        ),
    ];
    for target in ["build", "check", "test-compile", "test-release-compile"] {
        phases.push(make_phase(&format!("all-features-{target}"), "compilation", target, &all));
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
        phases.push(make_phase(&format!("all-features-{target}"), kind, target, &all));
    }
    for (identity, package, features, lint) in FEATURE_VARIANTS {
        let flags = variant_flags(package, features);
        phases.push(make_phase(&format!("{identity}-tests"), "tests", "test", &flags));
        if lint {
            phases.push(make_phase(&format!("{identity}-lint"), "lint", "lint", &flags));
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
            &[
                "/usr/bin/python3",
                "-B",
                "deploy/mission-control/backend-suites.py",
                "--scope",
                "main",
            ],
            "backend-suites",
        ));
    } else {
        phases.push(phase(
            "backend-memory",
            "tests",
            &[
                "/usr/bin/python3",
                "-B",
                "deploy/mission-control/backend-suites.py",
                "--scope",
                "branch",
            ],
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
pub fn event_stage_evidence(
    branch_scope: &str,
    checks: &Value,
    failure: Option<&str>,
) -> Result<Value, String> {
    let phases = event_phases_for_scope(branch_scope);
    if let Some(reason) = failure {
        if reason.is_empty() {
            return Err("Invalid typed phase failure reason".to_string());
        }
    }
    let checks = checks
        .as_array()
        .filter(|items| items.len() <= phases.len())
        .ok_or_else(|| "Invalid v3 phase execution count".to_string())?;
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
        if kind_phases
            .iter()
            .all(|index| *index < records.len() && records[*index]["returncode"].as_i64() == Some(0))
        {
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
            Some(record["kind"].as_str().unwrap_or("infrastructure").to_string())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_kinds_match_the_python_frozenset() {
        let mut kinds: Vec<&str> = FAILURE_KINDS.to_vec();
        kinds.sort_unstable();
        kinds.dedup();
        assert_eq!(kinds.len(), FAILURE_KINDS.len());
        assert!(FAILURE_KINDS.contains(&"feedback_delivery"));
        assert!(FAILURE_KINDS.contains(&"reviewer_correction"));
    }

    #[test]
    fn phase_plan_shape_for_both_scopes() {
        let branch = event_phases_for_scope("feature/x");
        let main = event_phases_for_scope("main");
        assert_eq!(branch[0].phase_id, "source-preflight");
        assert_eq!(branch[0].logical_gate, "source-preflight");
        assert_eq!(main[0].logical_gate, "check");
        assert_eq!(branch.last().unwrap().phase_id, "backend-memory");
        assert_eq!(main.last().unwrap().phase_id, "backend-main");
        assert!(main.iter().any(|phase| phase.phase_id == "browser-journeys"));
        assert!(!branch.iter().any(|phase| phase.phase_id == "browser-journeys"));
        // 5 fixed + 4 all-features builds + 10 variant compiles + 1 ui
        // + 4 all-features gates + 18 variant test/lint + 2 tail + 1/2.
        assert_eq!(branch.len(), 45);
        assert_eq!(main.len(), 46);
    }

    #[test]
    fn stage_evidence_binds_to_the_closed_plan() {
        let phases = event_phases_for_scope("feature/x");
        let checks: Vec<Value> = phases
            .iter()
            .enumerate()
            .map(|(index, phase)| {
                json!({"index": index, "argv": phase.argv, "returncode": 0})
            })
            .collect();
        let evidence = event_stage_evidence("feature/x", &Value::Array(checks.clone()), None).unwrap();
        assert_eq!(evidence["failure_kind"], Value::Null);
        assert!(evidence["passed_kinds"]
            .as_array()
            .unwrap()
            .contains(&json!("tests")));
        let mut short = checks.clone();
        short.pop();
        assert!(event_stage_evidence("feature/x", &Value::Array(short), None).is_err());
        let mut failing = checks;
        let last = failing.len() - 1;
        failing[last]["returncode"] = json!(2);
        // A failed phase with no failure reason is refused, exactly
        // as Python raises; with a reason the kind is the phase's.
        assert!(
            event_stage_evidence("feature/x", &Value::Array(failing.clone()), None).is_err()
        );
        let evidence = event_stage_evidence(
            "feature/x",
            &Value::Array(failing),
            Some("phase_failed"),
        )
        .unwrap();
        assert_eq!(evidence["failure_kind"], json!("tests"));
        let evidence = event_stage_evidence(
            "feature/x",
            &Value::Array(vec![]),
            Some("verification_unavailable"),
        )
        .unwrap();
        assert_eq!(evidence["failure_kind"], json!("infrastructure"));
    }
}
