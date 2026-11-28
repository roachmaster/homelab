//! Step bindings for features/worker_cooldown.feature, driving the real
//! ProviderCooldown admission cache in forge-work-worker with a deterministic clock.

use forge_scene::bdd::{ensure, verify, Steps};
use forge_work_worker::config::{parse_config, WorkerConfig};
use forge_work_worker::cooldown::{CooldownClock, ProviderCooldown};
use forge_work_worker::lease::Lease;
use forge_work_worker::ports_worker::CooldownPort;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

struct TempRoot(PathBuf);

impl TempRoot {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("forge-scene-cooldown-{}", Uuid::new_v4().simple())))
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct World {
    _root: Option<TempRoot>,
    now: Arc<Mutex<f64>>,
    cooldown: Option<ProviderCooldown>,
    allowed: Vec<String>,
    advertised_count: usize,
}

impl Default for World {
    fn default() -> Self {
        Self {
            _root: None,
            now: Arc::new(Mutex::new(1_000.0)),
            cooldown: None,
            allowed: Vec::new(),
            advertised_count: 0,
        }
    }
}

fn config_for(state_root: &Path) -> WorkerConfig {
    let sample = json!({
        "worker_id": "worker-bdd",
        "owner": "forge",
        "database": {"host": "/run/postgresql", "port": 5432, "name": "forge_agent", "user": "forge_coding_worker"},
        "account": {"user": "forgeqa", "uid": 1001, "gid": 1001, "home": "/home/forgeqa"},
        "workspace_root": "/srv/forge/workspaces",
        "state_root": state_root.to_string_lossy(),
        "projects": {"homelab": "/srv/repos/homelab"},
        "profiles": {
            "codex-author": {"backend": "codex", "model": "gpt-5", "modes": ["plan", "review", "implement"]},
            "codex-backup": {"backend": "codex", "model": "gpt-5-mini", "modes": ["plan", "review", "implement"]}
        },
        "binaries": {"codex": "/usr/local/bin/codex"}
    });
    parse_config(&sample).expect("valid worker config")
}

fn primary_lease() -> Lease {
    Lease {
        task_id: "11111111-1111-1111-1111-111111111111".to_string(),
        project_alias: "homelab".to_string(),
        base_revision: "a".repeat(40),
        mode: "implement".to_string(),
        agent_profile: "codex-author".to_string(),
        prompt: "do it".to_string(),
        acceptance_criteria: "done".to_string(),
        recipe: json!({"backend": "codex", "model": "gpt-5"}),
        attempt_id: "22222222-2222-2222-2222-222222222222".to_string(),
        attempt_token: "33333333-3333-3333-3333-333333333333".to_string(),
        worker_id: "worker-bdd".to_string(),
        incarnation: "44444444-4444-4444-4444-444444444444".to_string(),
        lease_expires_at: "2026-11-27T17:00:00Z".to_string(),
        deadline_at: "2026-11-27T18:00:00Z".to_string(),
        unit_name: "forge-task-bdd".to_string(),
        task_kind: "feature".to_string(),
        defect: None,
        resume_workspace_digest: None,
        resume_attempt_id: None,
        plan_decision_id: None,
        feedback_id: None,
        feedback_sha256: None,
        repair_source_review_id: None,
        repair_candidate_commit: None,
        scope_plan_decision_id: None,
        parent_task_id: None,
        workflow_id: None,
        workflow_review_id: None,
        review_target_sha256: None,
        workflow_feedback_review_id: None,
        workflow_feedback_sha256: None,
    }
}

fn quota_diagnostic() -> Value {
    json!({
        "version": 1,
        "code": "provider_quota_exhausted",
        "error_sha256": "a".repeat(64),
        "retry_after_seconds": 600,
    })
}

fn refresh_selection(w: &mut World) -> Result<(), String> {
    let cooldown = w.cooldown.as_ref().ok_or("cooldown not initialized")?;
    let (allowed, advertised) = cooldown.selection();
    w.allowed = allowed;
    w.advertised_count = advertised.len();
    Ok(())
}

fn steps() -> Steps<World> {
    Steps::new()
        .given(
            "the worker cooldown can evaluate configured profiles",
            |w: &mut World, _| {
                let root = TempRoot::new();
                let now = Arc::clone(&w.now);
                let clock: CooldownClock = Arc::new(move || *now.lock().unwrap());
                let mut cooldown = ProviderCooldown::with_clock(config_for(&root.0), clock);
                cooldown.prepare().map_err(|e| format!("{e:?}"))?;
                w._root = Some(root);
                w.cooldown = Some(cooldown);
                Ok(())
            },
        )
        .given(
            "the worker cooldown needs a valid state directory",
            |w, _| {
                let missing = std::env::temp_dir()
                    .join(format!("forge-missing-parent-{}", Uuid::new_v4().simple()))
                    .join("state");
                let now = Arc::clone(&w.now);
                let clock: CooldownClock = Arc::new(move || *now.lock().unwrap());
                w.cooldown = Some(ProviderCooldown::with_clock(config_for(&missing), clock));
                Ok(())
            },
        )
        .when("no quota diagnostics have been recorded", |w, _| {
            refresh_selection(w)
        })
        .when(
            "a quota exhaustion diagnostic is recorded for the primary profile",
            |w, _| {
                let cooldown = w.cooldown.as_mut().ok_or("cooldown not initialized")?;
                cooldown.record(&primary_lease(), &quota_diagnostic());
                refresh_selection(w)
            },
        )
        .when("the clock advances past the retry window", |w, _| {
            *w.now.lock().unwrap() = 1_601.0;
            refresh_selection(w)
        })
        .when(
            "the same attempt diagnostic is recorded twice after the clock moves slightly",
            |w, _| {
                let cooldown = w.cooldown.as_mut().ok_or("cooldown not initialized")?;
                let lease = primary_lease();
                let diag = quota_diagnostic();
                cooldown.record(&lease, &diag);
                *w.now.lock().unwrap() = 1_200.0;
                cooldown.record(&lease, &diag);
                refresh_selection(w)
            },
        )
        .when(
            "the clock advances past the original retry window",
            |w, _| {
                *w.now.lock().unwrap() = 1_601.0;
                refresh_selection(w)
            },
        )
        .when("a malformed quota diagnostic is recorded", |w, _| {
            let cooldown = w.cooldown.as_mut().ok_or("cooldown not initialized")?;
            cooldown.record(&primary_lease(), &json!({"version": 99, "code": "invalid"}));
            refresh_selection(w)
        })
        .when("selection runs against a missing state parent", |w, _| {
            refresh_selection(w)
        })
        .then("both configured profiles remain admitted", |w, _| {
            ensure(
                w.allowed.iter().any(|p| p == "codex-author")
                    && w.allowed.iter().any(|p| p == "codex-backup"),
                format!("expected both profiles, got {:?}", w.allowed),
            )?;
            ensure(
                w.advertised_count == 2,
                format!("expected 2 advertised profiles, got {}", w.advertised_count),
            )
        })
        .then("only the backup profile remains admitted", |w, _| {
            ensure(
                w.allowed == vec!["codex-backup".to_string()],
                format!("expected only codex-backup, got {:?}", w.allowed),
            )?;
            ensure(
                w.advertised_count == 1,
                format!("expected 1 advertised profile, got {}", w.advertised_count),
            )
        })
        .then("every profile is withheld", |w, _| {
            ensure(
                w.allowed.is_empty(),
                format!("expected empty selection, got {:?}", w.allowed),
            )?;
            ensure(
                w.advertised_count == 0,
                format!("expected 0 advertised profiles, got {}", w.advertised_count),
            )
        })
}

#[test]
fn worker_cooldown_feature() {
    let report = verify(
        include_str!("../features/worker_cooldown.feature"),
        &steps(),
    )
    .unwrap_or_else(|errors| panic!("worker_cooldown.feature failed:\n{errors}"));
    assert_eq!(report.scenarios, 6);
}
