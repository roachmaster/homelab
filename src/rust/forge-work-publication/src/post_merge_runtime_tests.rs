use super::*;
use crate::trusted_delivery::SourceArtifactRejected;
use std::sync::Mutex;

fn config() -> PublicationConfig {
    let worker = forge_work_worker::config::parse_config(&json!({
        "worker_id":"fixture-worker", "owner":"fixture-owner",
        "database":{"host":"/run/postgresql","port":5432,"name":"forge_agent","user":"fixture"},
        "account":{"user":"fixture","uid":1000,"gid":1000,"home":"/srv/home"},
        "workspace_root":"/srv/workspaces", "state_root":"/srv/state",
        "projects":{"homelab":"/srv/source"},
        "profiles":{"codex-primary":{"backend":"codex","model":"fixture-model","modes":["plan"]}},
        "binaries":{"codex":"/usr/bin/false"}, "autonomous_projects":["homelab"]
    }))
    .unwrap();
    crate::publication_config::parse_config(&json!({
        "worker_config":"/srv/worker.json", "repositories":{"homelab":"fixture/project"},
        "github_config_dir":"/srv/home/.config/gh", "merge_enabled":true,"hosted_provider":"jenkins",
        "jenkins_delivery_config":"/srv/branch.json", "post_merge_enabled":true,
        "post_merge_jenkins_delivery_config":"/srv/main.json"
    }), &worker, None).unwrap()
}

#[derive(Default)]
struct Store {
    finishes: Mutex<Vec<(String, Value, String)>>,
    checkpoints: Mutex<usize>,
}
impl PostMergeStorePort for Store {
    fn post_merge_claim(&self, _: &str) -> Result<Option<Value>, ProviderError> {
        Ok(None)
    }
    fn post_merge_renew(&self, _: &Value) -> Result<bool, ProviderError> {
        Ok(true)
    }
    fn post_merge_start_allowed(&self, _: &Value) -> Result<bool, ProviderError> {
        Ok(true)
    }
    fn post_merge_checkpoint(&self, _: &Value, _: &Value) -> Result<bool, ProviderError> {
        *self.checkpoints.lock().unwrap() += 1;
        Ok(true)
    }
    fn post_merge_finish(
        &self,
        _: &Value,
        status: &str,
        receipt: &Value,
        summary: &str,
    ) -> Result<bool, ProviderError> {
        self.finishes
            .lock()
            .unwrap()
            .push((status.into(), receipt.clone(), summary.into()));
        Ok(true)
    }
}
struct Provider(Option<ProviderError>);
impl PostMergeProviderPort for Provider {
    fn prepare(&mut self, _: &Value, lease: RequireLease) -> Result<Value, ProviderError> {
        lease()?;
        Err(self.0.take().unwrap())
    }
    fn verify(&mut self, _: &Value, _: &Value, _: RequireLease) -> Result<Value, ProviderError> {
        panic!("unsubmitted source must not run Jenkins")
    }
    fn cancel(&mut self, _: &Value, _: &Value, _: RequireLease) -> Result<Value, ProviderError> {
        panic!("unsubmitted source has no Jenkins process")
    }
}
fn job() -> Value {
    json!({"post_merge_id":Uuid::new_v4().to_string(),"owner_id":"fixture-owner",
        "project_alias":"homelab","workflow_id":Uuid::new_v4().to_string(),
        "correlation_id":Uuid::new_v4().to_string(),"lease_token":Uuid::new_v4().to_string(),
        "merge_commit":"a".repeat(40),"merge_tree":"b".repeat(40),"request":null,"receipt":null})
}
fn rejection(commit: &str, tree: &str) -> ProviderError {
    ProviderError::SourceArtifacts(Box::new(
        SourceArtifactRejected::new(
            commit,
            tree,
            tree,
            vec![(
                "__pycache__/fixture.pyc".into(),
                "private_or_generated_artifact".into(),
            )],
            1,
        )
        .unwrap(),
    ))
}

#[test]
fn permanent_source_rejection_finishes_failed_only_before_submission() {
    let store = Arc::new(Store::default());
    let mut worker = PostMergeWorker::new(
        config(),
        store.clone(),
        Box::new(Provider(Some(rejection(&"a".repeat(40), &"b".repeat(40))))),
    )
    .unwrap();
    worker.process(&job()).unwrap();
    let outcomes = store.finishes.lock().unwrap();
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].0, "failed");
    assert_eq!(outcomes[0].1["process_started"], false);
    assert_eq!(outcomes[0].1["reason"], "source_artifact_rejected");
    assert_eq!(*store.checkpoints.lock().unwrap(), 0);
}

#[test]
fn mismatched_source_and_transient_errors_remain_pending_without_a_fake_result() {
    for error in [
        rejection(&"c".repeat(40), &"b".repeat(40)),
        ProviderError::Other("temporary transport observation".into()),
    ] {
        let store = Arc::new(Store::default());
        let mut worker =
            PostMergeWorker::new(config(), store.clone(), Box::new(Provider(Some(error)))).unwrap();
        assert!(worker.process(&job()).is_err());
        let outcomes = store.finishes.lock().unwrap();
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].0, "pending");
        assert_ne!(outcomes[0].1["reason"], "source_artifact_rejected");
        assert_eq!(*store.checkpoints.lock().unwrap(), 0);
    }
}

#[test]
fn main_failure_tail_keeps_immutable_header_and_redacts_before_utf8_bounds() {
    let request = json!({"action_id":Uuid::new_v4().to_string()});
    let receipt = json!({"checks":[{"index":44,"returncode":2}]});
    let summary = format!(
        "{}\nAuthorization: Bearer fixture-secret\nQA_CHECK journey_location=3893:5",
        "研".repeat(1800)
    );
    assert!(!diagnostic_text(&summary, 2048).contains("journey_location"));
    let result = failed_check_diagnostic(&request, &receipt, &summary);
    assert!(result.len() <= 2048);
    assert!(result.contains(request["action_id"].as_str().unwrap()));
    assert!(result.contains("fixed check 44, exit 2"));
    assert!(result.contains("[bounded diagnostic tail]"));
    assert!(!result.contains("fixture-secret"));
    assert!(result.ends_with("QA_CHECK journey_location=3893:5"));
    assert_eq!(
        failed_check_diagnostic(&request, &json!({}), &summary),
        diagnostic_text(&summary, 2048)
    );
}

#[test]
fn protected_main_bytes_require_exact_request_receipt_and_log_identity() {
    let request = json!({"action_id":Uuid::new_v4().to_string(),"workflow_id":Uuid::new_v4().to_string(),
        "correlation_id":Uuid::new_v4().to_string(),"candidate_commit":"a".repeat(40),"candidate_tree":"b".repeat(40),
        "integration_base":"a".repeat(40),"branch_scope":"main","verification_profile":SCOPED_PROFILE,
        "gate_manifest_sha256":"c".repeat(64)});
    let log = b"FORGE_FIXED_CHECK_START 0\nfixture\nFORGE_FIXED_CHECK_END 0 rc=0 elapsed_ms=1\n";
    let mut full = request.clone();
    full["expected_main"] = request["integration_base"].clone();
    full["version"] = json!(2);
    full["checks"] = json!([{"index":0,"returncode":0}]);
    full["coverage"] = json!({"scope":"main"});
    full["cleanup_confirmed"] = json!(true);
    full["log_sha256"] = json!(format!("{:x}", Sha256::digest(log)));
    let raw = serde_json::to_vec(&full).unwrap();
    let mut receipt = full.clone();
    receipt["result_version"] = json!(2);
    receipt["check_receipt_sha256"] = json!(format!("{:x}", Sha256::digest(&raw)));
    assert!(validate_main_evidence(&raw, log, &request, &receipt).is_ok());
    assert!(validate_main_evidence(b"{}", log, &request, &receipt).is_err());
    assert!(validate_main_evidence(&raw, b"changed", &request, &receipt).is_err());
    for (field, value) in [
        ("action_id", json!(Uuid::new_v4().to_string())),
        ("candidate_commit", json!("d".repeat(40))),
        ("candidate_tree", json!("d".repeat(40))),
        ("expected_main", json!("d".repeat(40))),
        ("branch_scope", json!("branch/other")),
        ("version", json!(99)),
        ("coverage", json!({})),
        ("checks", json!([])),
        ("cleanup_confirmed", json!(false)),
    ] {
        let mut changed = full.clone();
        changed[field] = value;
        let bytes = serde_json::to_vec(&changed).unwrap();
        let mut witness = receipt.clone();
        witness["check_receipt_sha256"] = json!(format!("{:x}", Sha256::digest(&bytes)));
        assert!(
            validate_main_evidence(&bytes, log, &request, &witness).is_err(),
            "{field}"
        );
    }
}
