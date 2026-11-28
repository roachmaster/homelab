//! work_external_workflow.py's phase machine (_step and
//! everything it drives): trusted delivery actions, source
//! selection, review queueing/consumption, author queueing,
//! verification and merge consumption, escalation and the
//! cancel cascade. The S4-bound pieces sit behind ports:
//! ExternalVerificationPort is the work_ci_contract +
//! work_verification observation (its absence mirrors the
//! unbound composition failing the observation), and
//! validate_gate_reference is the one pure function of
//! work_ci_contract.py consumed here (S4 reuses it).

use serde_json::{json, Value};
use uuid::Uuid;

use crate::branch_scope::{derive_branch_scope, validate_branch_name, validate_branch_scope};
use crate::canonical::{canonical_json, sha256_hex};
use crate::execution_policy::sha256_canonical;
use crate::external_identity::validate_content_identity;
use crate::external_review::{parse_verdict, validate_target, ExternalSourceIntegration};
use crate::external_tx::{ExternalTx, LockMode};
use crate::external_workflow::{is_hex, opt_uuid, ExternalWorkflowStore, TERMINAL};
use crate::model_route::format_iso;
use crate::retry_policy::AGENT_RETRY_LIMIT;
use crate::strict_json::parse_strict;
use crate::validation::is_int;
use crate::work_actors::{Actor, ActorKind};
use crate::work_error::WorkError;
use crate::workflow::{coordinator_actor, int_of, prepare_title, text, uuid_of};
use crate::workflow_ingress::parse_ts;

/// _verified_stage_observation's port: the exact stopped
/// stage evidence for a verify receipt, or an error the
/// caller turns into the no-repair escalation.
pub trait ExternalVerificationPort: Send + Sync {
    fn verified_stage_observation(
        &self,
        w: &Value,
        action: &Value,
        receipt: &Value,
    ) -> Result<Value, String>;
}

/// work_ci_contract.validate_gate_reference: a 64-hex
/// digest, or forge-sdlc-jenkins-v[1-4]:(main|detached|
/// `branch/<slug>`) as a full match.
pub fn validate_gate_reference(value: &Value) -> Result<String, WorkError> {
    let invalid = || WorkError::store("Invalid delivery gate");
    let raw = value.as_str().ok_or_else(invalid)?;
    if raw.len() == 64
        && raw
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Ok(raw.to_string());
    }
    let rest = raw
        .strip_prefix("forge-sdlc-jenkins-v")
        .ok_or_else(invalid)?;
    let mut chars = rest.chars();
    match chars.next() {
        Some('1' | '2' | '3' | '4') => {}
        _ => return Err(invalid()),
    }
    if chars.next() != Some(':') {
        return Err(invalid());
    }
    let scope = chars.as_str();
    if scope == "main" || scope == "detached" {
        return Ok(raw.to_string());
    }
    let slug = scope.strip_prefix("branch/").ok_or_else(invalid)?;
    if branch_slug_valid(slug) {
        return Ok(raw.to_string());
    }
    Err(invalid())
}

/// BRANCH_SLUG_RE: [a-z0-9](?:[a-z0-9._-]{0,56}[a-z0-9])?
fn branch_slug_valid(slug: &str) -> bool {
    let bytes = slug.as_bytes();
    if bytes.is_empty() || bytes.len() > 58 {
        return false;
    }
    let lower = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit();
    if !lower(bytes[0]) {
        return false;
    }
    if bytes.len() == 1 {
        return true;
    }
    lower(bytes[bytes.len() - 1])
        && bytes[1..bytes.len() - 1]
            .iter()
            .all(|b| lower(*b) || matches!(b, b'.' | b'_' | b'-'))
}

/// Python str() of a JSON value, for the few places the
/// source interpolates one (None -> 'None', True -> 'True').
pub(crate) fn py_str(value: &Value) -> String {
    match value {
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::String(raw) => raw.clone(),
        other => other.to_string(),
    }
}

fn state_pop(w: &mut Value, key: &str) -> Value {
    w["state"]
        .as_object_mut()
        .and_then(|state| state.remove(key))
        .unwrap_or(Value::Null)
}

fn truncate_bytes(value: &str, limit: usize) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        if out.len() + ch.len_utf8() > limit {
            break;
        }
        out.push(ch);
    }
    out
}

/// _revise's keyword options.
#[derive(Default)]
pub(crate) struct ReviseOptions {
    pub retry_kind: Option<String>,
    pub source_id: Option<String>,
    pub evidence_digest: Option<String>,
    pub passed_kinds: Vec<String>,
    pub phase_id: Option<String>,
    pub passed_phase_ids: Vec<String>,
}

impl ExternalWorkflowStore {
    fn source_key(&self, w: &Value) -> String {
        let mut key = format!(
            "{}:{}:{}",
            text(w, "active_task_id"),
            int_of(w, "plan_revisions"),
            int_of(w, "code_revisions")
        );
        if let Some(epoch) = w["state"]["operator_source_epoch"].as_str() {
            key += &format!(":{epoch}");
        }
        if let Some(epoch) = w["state"]["prepare_refresh_epoch"].as_str() {
            key += &format!(":{epoch}");
        }
        key
    }

    /// _action: the durable delivery action row for (kind,
    /// key) - expiring a stale queued/leased row first -
    /// or a fresh insert plus the queued event.
    pub(crate) fn action(
        &self,
        tx: &mut dyn ExternalTx,
        w: &Value,
        kind: &str,
        key: &str,
        request: &Value,
    ) -> Result<Value, WorkError> {
        if let Some(action) = tx.action_by_key(uuid_of(w, "workflow_id"), kind, key)? {
            let expired = tx.expire_action(uuid_of(&action, "action_id"))?;
            if expired.is_some() && kind == "verify" {
                self.cleanup_action(tx, w, &action)?;
            }
            return Ok(expired.unwrap_or(action));
        }
        let root = self.task(tx, w, Some(uuid_of(w, "root_task_id")))?;
        let action_id = Uuid::new_v4();
        let mut intent = request.clone();
        intent["workflow_id"] = json!(uuid_of(w, "workflow_id").to_string());
        intent["project_alias"] = w["project_alias"].clone();
        intent["correlation_id"] = json!(py_str(&root["correlation_id"]));
        intent["action_id"] = json!(action_id.to_string());
        let row = json!({
            "action_id": action_id.to_string(),
            "workflow_id": uuid_of(w, "workflow_id").to_string(),
            "owner_id": self.owner,
            "project_alias": text(w, "project_alias"),
            "workflow_revision": w["revision"],
            "kind": kind,
            "source_key": key,
            "intent": intent,
        });
        let result = tx.insert_action_row(&row)?;
        *self.new_actions.borrow_mut() = true;
        self.event(tx, w, &format!("Trusted {kind} action queued."), None, None)?;
        tx.notify_delivery()?;
        Ok(result)
    }

    /// _cleanup_action: queue the cancel action targeting a
    /// delivery action.
    pub(crate) fn cleanup_action(
        &self,
        tx: &mut dyn ExternalTx,
        w: &Value,
        action: &Value,
    ) -> Result<Value, WorkError> {
        let target_receipt = if action["result"].is_null() || action["result"]["receipt"].is_null()
        {
            json!({})
        } else {
            action["result"]["receipt"].clone()
        };
        self.action(
            tx,
            w,
            "cancel",
            &uuid_of(action, "action_id").to_string(),
            &json!({
                "target_action_id": uuid_of(action, "action_id").to_string(),
                "target_kind": action["kind"],
                "target_request": action["intent"],
                "target_receipt": target_receipt,
            }),
        )
    }

    /// _source: the trusted source-selection action pins
    /// state.base_revision once.
    fn source(
        &self,
        tx: &mut dyn ExternalTx,
        w: &mut Value,
        artifact: &Value,
        task: &Value,
    ) -> Result<bool, WorkError> {
        if !w["state"]["base_revision"].is_null()
            || w["state"]
                .as_object()
                .map(|state| state.contains_key("base_revision"))
                .unwrap_or(false)
        {
            return Ok(true);
        }
        let request = json!({
            "source_task_id": uuid_of(task, "id").to_string(),
            "source_assignment_id": artifact["assignment_id"],
            "source_sha256": artifact["sha256"],
            "base_revision": task["base_revision"],
        });
        let key = self.source_key(w);
        let a = self.action(tx, w, "source", &key, &request)?;
        if text(&a, "status") == "failed" {
            self.escalate(
                tx,
                w,
                &format!(
                    "Trusted source selection failed: {}",
                    text(&a["result"], "summary")
                ),
                true,
                None,
            )?;
            return Ok(false);
        }
        if text(&a, "status") != "completed" {
            return Ok(false);
        }
        let head = &a["result"]["receipt"]["base_revision"];
        if !is_hex(head, 40) {
            return Err(WorkError::store(
                "Delivery source receipt has no exact revision",
            ));
        }
        w["state"]["base_revision"] = head.clone();
        self.save(
            tx,
            w,
            "Configured source revision selected for independent review.",
            None,
            None,
            None,
        )?;
        Ok(true)
    }
}

impl ExternalWorkflowStore {
    /// _validate_prepared_branch_scope.
    fn validate_prepared_branch_scope(candidate: &Value) -> Result<String, String> {
        let scope_raw = candidate["branch_scope"]
            .as_str()
            .ok_or_else(|| "Invalid workflow branch scope".to_string())?;
        let scope = validate_branch_scope(
            scope_raw,
            text(candidate, "candidate_commit"),
            text(candidate, "integration_base"),
        )?;
        let source_branch = candidate["source_branch"].as_str();
        if scope == "detached" {
            if !candidate["source_branch"].is_null() {
                return Err("Detached candidate must not carry source_branch".to_string());
            }
        } else if scope == "main" {
            if candidate["candidate_commit"] != candidate["integration_base"] {
                return Err("Non-main candidate cannot claim main branch scope".to_string());
            }
            if !matches!(source_branch, None | Some("main")) {
                return Err("Main branch scope conflicts with source_branch".to_string());
            }
        } else if let Some(branch) = source_branch {
            let expected = derive_branch_scope(
                text(candidate, "candidate_commit"),
                text(candidate, "integration_base"),
                Some(branch),
            )?;
            if expected != scope {
                return Err("Candidate branch_scope conflicts with source_branch".to_string());
            }
        }
        Ok(scope)
    }

    /// _candidate_manifest: the strict version-1 author
    /// manifest.
    fn candidate_manifest(content: &str) -> Result<Value, String> {
        let value = parse_strict(content).map_err(|_| "Invalid candidate manifest".to_string())?;
        let object = value
            .as_object()
            .ok_or_else(|| "Invalid candidate manifest".to_string())?;
        let required = [
            "version",
            "candidate_commit",
            "candidate_tree",
            "base_revision",
            "summary",
        ];
        let keys_ok = required.iter().all(|key| object.contains_key(*key))
            && object
                .keys()
                .all(|key| required.contains(&key.as_str()) || key == "source_branch");
        if !keys_ok || !is_int(&object["version"]) || object["version"].as_i64() != Some(1) {
            return Err("Invalid candidate manifest".to_string());
        }
        for key in ["candidate_commit", "candidate_tree", "base_revision"] {
            if !is_hex(&object[key], 40) {
                return Err("Invalid candidate source identity".to_string());
            }
        }
        let summary = object["summary"]
            .as_str()
            .ok_or_else(|| "Invalid candidate source identity".to_string())?;
        if summary.len() > 4096 {
            return Err("Invalid candidate source identity".to_string());
        }
        if !object["source_branch"].is_null() {
            let branch = object["source_branch"]
                .as_str()
                .ok_or_else(|| "Invalid Git branch name".to_string())?;
            validate_branch_name(branch)?;
            if branch == "main" && object["candidate_commit"] != object["base_revision"] {
                return Err("Non-main candidate cannot claim main source branch".to_string());
            }
        }
        Ok(value)
    }

    /// _step: one phase-machine advance for a leased
    /// workflow.
    #[allow(clippy::too_many_lines)]
    pub(crate) fn step(&self, tx: &mut dyn ExternalTx, w: &mut Value) -> Result<bool, WorkError> {
        let root = self.task(tx, w, Some(uuid_of(w, "root_task_id")))?;
        let task = if uuid_of(w, "active_task_id") == uuid_of(w, "root_task_id") {
            root.clone()
        } else {
            self.task(tx, w, None)?
        };
        let cancelled =
            |row: &Value| matches!(text(row, "status"), "cancelled" | "cancel_requested");
        if text(w, "phase") != "merging" && (cancelled(&root) || cancelled(&task)) {
            let note = if cancelled(&root) {
                "Root workflow task cancellation observed."
            } else {
                "Author task cancellation observed."
            };
            return self.cancel_locked(tx, w, note, &coordinator_actor(), "workflow_cancelled");
        }
        if matches!(
            text(w, "phase"),
            "plan" | "plan_revision" | "implement" | "repair"
        ) {
            let Some(artifact) = self.artifact(tx, w, &task)? else {
                if matches!(
                    text(&task, "status"),
                    "failed"
                        | "blocked_auth"
                        | "blocked_permission"
                        | "quota_blocked"
                        | "needs_reconcile"
                ) {
                    self.escalate(
                        tx,
                        w,
                        &format!("External author cannot continue: {}", text(&task, "status")),
                        true,
                        None,
                    )?;
                    return Ok(true);
                }
                return Ok(false);
            };
            if !self.source(tx, w, &artifact, &task)? {
                return Ok(false);
            }
            if !w["state"]["pending_rebase"].is_null() {
                let summary = state_pop(w, "pending_rebase");
                let cause = state_pop(w, "pending_rebase_cause");
                return self.revise(
                    tx,
                    w,
                    "code",
                    &json!({"summary": summary, "findings": []}),
                    &artifact,
                    ReviseOptions {
                        retry_kind: Some("merge".to_string()),
                        source_id: cause["source_id"].as_str().map(str::to_string),
                        evidence_digest: cause["digest"].as_str().map(str::to_string),
                        ..Default::default()
                    },
                );
            }
            if text(&task, "mode") == "plan" {
                let retry = if !w["state"]["operator_rescue_pending"].is_null()
                    && w["state"]["operator_rescue_pending"].as_bool() == Some(true)
                {
                    int_of(&w["state"], "review_retries")
                } else {
                    0
                };
                state_pop(w, "operator_rescue_pending");
                self.queue_review(tx, w, &task, &artifact, "plan", retry, None)?;
                return Ok(true);
            }
            let manifest = match Self::candidate_manifest(text(&artifact, "content")) {
                Ok(manifest) => manifest,
                Err(_) => {
                    return self.revise(
                        tx,
                        w,
                        "code",
                        &json!({
                            "summary": "Return the exact candidate JSON manifest with version,candidate_commit,candidate_tree,base_revision,summary (and optional source_branch). Preserve your existing branch and completed work.",
                            "findings": [],
                        }),
                        &artifact,
                        ReviseOptions {
                            retry_kind: Some("feedback_delivery".to_string()),
                            source_id: Some(format!(
                                "artifact:{}",
                                text(&artifact, "assignment_id")
                            )),
                            evidence_digest: Some(
                                text(&artifact, "sha256").to_string(),
                            ),
                            ..Default::default()
                        },
                    );
                }
            };
            if manifest["base_revision"] != task["base_revision"] {
                let returned = json!({
                    "candidate_commit": manifest["candidate_commit"],
                    "candidate_tree": manifest["candidate_tree"],
                    "base_revision": manifest["base_revision"],
                });
                let history = if w["state"]["manifest_returns"].is_object() {
                    w["state"]["manifest_returns"].clone()
                } else {
                    json!({"rounds": 0, "last": null})
                };
                let recorded = if w["state"]["candidate"].is_object() {
                    w["state"]["candidate"].clone()
                } else {
                    json!({})
                };
                let recorded_commit = if recorded
                    .as_object()
                    .map(|object| object.contains_key("author_candidate_commit"))
                    .unwrap_or(false)
                {
                    recorded["author_candidate_commit"].clone()
                } else {
                    recorded["candidate_commit"].clone()
                };
                let repeated = history["last"] == returned
                    || (recorded_commit == manifest["candidate_commit"]
                        && recorded["integration_base"] == manifest["base_revision"]);
                let rounds = int_of(&history, "rounds");
                if repeated || rounds >= crate::external_workflow::MANIFEST_RETURN_LIMIT {
                    w["state"]["manifest_returns"] = json!({
                        "rounds": rounds,
                        "last": returned,
                        "stopped": true,
                    });
                    let lead = if repeated {
                        format!(
                            "The author returned the identical candidate manifest at base {}",
                            text(&manifest, "base_revision")
                        )
                    } else {
                        format!(
                            "The bounded manifest-return budget is exhausted at base {}",
                            text(&manifest, "base_revision")
                        )
                    };
                    self.escalate(
                        tx,
                        w,
                        &format!(
                            "{lead} although base {} was requested. Manifest-return loop stopped; the candidate and all tasks are retained.",
                            text(&task, "base_revision")
                        ),
                        false,
                        None,
                    )?;
                    return Ok(true);
                }
                w["state"]["manifest_returns"] = json!({
                    "rounds": rounds + 1,
                    "last": returned,
                });
                return self.revise(
                    tx,
                    w,
                    "code",
                    &json!({
                        "summary": format!(
                            "Return the exact candidate manifest at the originally supplied base {}. Preserve the existing implementation.",
                            text(&task, "base_revision")
                        ),
                        "findings": [],
                    }),
                    &artifact,
                    ReviseOptions {
                        retry_kind: Some("feedback_delivery".to_string()),
                        source_id: Some(format!(
                            "artifact:{}",
                            text(&artifact, "assignment_id")
                        )),
                        evidence_digest: Some(text(&artifact, "sha256").to_string()),
                        ..Default::default()
                    },
                );
            }
            let mut request = json!({
                "source_task_id": uuid_of(&task, "id").to_string(),
                "source_assignment_id": artifact["assignment_id"],
                "source_sha256": artifact["sha256"],
                "base_revision": manifest["base_revision"],
                "candidate_commit": manifest["candidate_commit"],
                "candidate_tree": manifest["candidate_tree"],
            });
            let identity_protocol = w["state"]["identity_protocol"]
                .as_str()
                .unwrap_or("legacy-v1")
                .to_string();
            if !matches!(identity_protocol.as_str(), "legacy-v1" | "content-v2") {
                return Err(WorkError::store("Invalid frozen identity protocol"));
            }
            if identity_protocol == "content-v2" {
                request["identity_protocol"] = json!("content-v2");
            }
            request["agent_profile"] = w["author_profile"].clone();
            request["root_task_id"] = json!(uuid_of(w, "root_task_id").to_string());
            if let Some(branch) = self
                .profiles
                .get(text(w, "author_profile"))
                .and_then(|profile| profile.branch.clone())
            {
                request["expected_agent_branch"] = json!(branch);
            }
            if !manifest["source_branch"].is_null() {
                request["source_branch"] = manifest["source_branch"].clone();
            }
            let mut prepare_key = text(&artifact, "sha256").to_string();
            if w["state"]["operator_rescue_pending"].as_bool() == Some(true) {
                if let Some(epoch) = w["state"]["operator_source_epoch"].as_str() {
                    prepare_key += &format!(":{epoch}");
                }
            }
            if let Some(epoch) = w["state"]["prepare_refresh_epoch"].as_str() {
                prepare_key += &format!(":{epoch}");
            }
            let a = self.action(tx, w, "prepare", &prepare_key, &request)?;
            if text(&a, "status") == "failed" {
                if text(&a["result"], "status") == "main_changed" {
                    if text(&a["result"]["receipt"], "reason") == "source_conflict" {
                        return self.request_rebase(
                            tx,
                            w,
                            "Trusted preparation found an actual source conflict. Rebase the preserved implementation onto the new trusted base and return its exact manifest.",
                            Some(&a),
                        );
                    }
                    return self.request_integration_refresh(tx, w, &a);
                }
                return self.revise(
                    tx,
                    w,
                    "code",
                    &json!({
                        "summary": a["result"]["summary"],
                        "findings": [],
                    }),
                    &artifact,
                    ReviseOptions {
                        retry_kind: Some("verification".to_string()),
                        source_id: Some(format!("delivery:{}", uuid_of(&a, "action_id"))),
                        evidence_digest: Some(sha256_canonical(&a["result"])),
                        ..Default::default()
                    },
                );
            }
            if text(&a, "status") != "completed" {
                return Ok(false);
            }
            let candidate = a["result"]["receipt"].clone();
            for key in ["candidate_commit", "candidate_tree", "integration_base"] {
                if !is_hex(&candidate[key], 40) {
                    return Err(WorkError::store("Unverified delivery candidate"));
                }
            }
            if candidate
                .as_object()
                .map(|object| object.contains_key("source_integration"))
                .unwrap_or(false)
            {
                let integration =
                    ExternalSourceIntegration::from_value(&candidate["source_integration"])
                        .map_err(WorkError::store)?;
                let action_id = uuid_of(&a, "action_id").to_string();
                if integration.prepare_action_id != action_id
                    || py_str(&candidate["prepare_action_id"]) != action_id
                    || integration.source_base_revision != text(&manifest, "base_revision")
                    || integration.source_candidate_commit != text(&manifest, "candidate_commit")
                    || integration.source_candidate_tree != text(&manifest, "candidate_tree")
                    || py_str(&candidate["author_candidate_commit"])
                        != text(&manifest, "candidate_commit")
                    || integration.integration_base != text(&candidate, "integration_base")
                    || integration.candidate_tree != text(&candidate, "candidate_tree")
                    || integration.integration_base == integration.source_base_revision
                {
                    return Err(WorkError::store(
                        "Prepared integration differs from its immutable author source",
                    ));
                }
            } else if candidate["candidate_tree"] != manifest["candidate_tree"]
                || candidate["integration_base"] != manifest["base_revision"]
            {
                return Err(WorkError::store(
                    "Prepared source differs from author candidate or trusted base",
                ));
            }
            for key in ["diff_sha256", "gate_source_sha256"] {
                if !is_hex(&candidate[key], 64) {
                    return Err(WorkError::store("Candidate lacks exact gate/diff digests"));
                }
            }
            let gate = validate_gate_reference(&candidate["gate_manifest_sha256"])?;
            if gate.contains(':')
                && gate
                    != format!(
                        "{}:{}",
                        py_str(&candidate["verification_profile"]),
                        py_str(&candidate["branch_scope"])
                    )
            {
                return Err(WorkError::store(
                    "Prepared gate contract differs from its verification profile or branch scope",
                ));
            }
            if identity_protocol == "content-v2" {
                if candidate["identity_protocol"].as_str() != Some("content-v2") {
                    return Err(WorkError::store(
                        "Prepared candidate lost its content protocol",
                    ));
                }
                let content = validate_content_identity(&candidate["content_identity"])
                    .map_err(WorkError::store)?;
                let action_id = uuid_of(&a, "action_id").to_string();
                if content["verification"]["generation"].as_str() != Some(action_id.as_str())
                    || py_str(&candidate["prepare_action_id"]) != action_id
                {
                    return Err(WorkError::store(
                        "Content identity is bound to a different preparation generation",
                    ));
                }
            } else if candidate
                .as_object()
                .map(|object| object.contains_key("content_identity"))
                .unwrap_or(false)
            {
                return Err(WorkError::store(
                    "Legacy generation cannot change identity protocol",
                ));
            }
            Self::validate_prepared_branch_scope(&candidate).map_err(WorkError::store)?;
            let changes = &candidate["protected_gate_changes"];
            let changes_ok = changes
                .as_array()
                .map(|list| {
                    list.len() <= 256
                        && list.iter().all(|change| {
                            change.is_object() && change["finding_id"].as_str().is_some()
                        })
                })
                .unwrap_or(false);
            if !changes_ok {
                return Err(WorkError::store("Invalid protected gate change manifest"));
            }
            self.resolve_retry(
                tx,
                w,
                "feedback_delivery",
                &format!("artifact:{}", text(&artifact, "assignment_id")),
                text(&artifact, "sha256"),
            )?;
            w["state"]["base_revision"] = candidate["integration_base"].clone();
            w["state"]["candidate_source"] = json!({
                "source_assignment_id": artifact["assignment_id"],
                "source_sha256": artifact["sha256"],
            });
            w["state"]["candidate"] = candidate;
            let retry = if w["state"]["operator_rescue_pending"].as_bool() == Some(true) {
                int_of(&w["state"], "review_retries")
            } else {
                0
            };
            state_pop(w, "operator_rescue_pending");
            self.queue_review(tx, w, &task, &artifact, "code", retry, None)?;
            return Ok(true);
        }
        if matches!(text(w, "phase"), "plan_review" | "code_review") {
            return self.consume_review(tx, w);
        }
        if text(w, "phase") == "checks" {
            return self.consume_verification(tx, w);
        }
        if text(w, "phase") == "merging" {
            return self.consume_merge(tx, w);
        }
        Ok(false)
    }
}

impl ExternalWorkflowStore {
    /// _insert_task: the coding task row + workflow role +
    /// the first 'queued' event.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn insert_task(
        &self,
        tx: &mut dyn ExternalTx,
        w: &Value,
        task_id: Uuid,
        mode: &str,
        profile: &str,
        recipe: &Value,
        prompt: &str,
        criteria: &str,
        base: &str,
        role: &str,
    ) -> Result<(), WorkError> {
        let intent = json!({
            "workflow_id": uuid_of(w, "workflow_id").to_string(),
            "task_id": task_id.to_string(),
            "mode": mode,
            "profile": profile,
            "recipe_digest": recipe["digest"],
            "prompt": prompt,
            "criteria": criteria,
            "base": base,
        });
        let project = text(w, "project_alias");
        tx.insert_task_row(&json!({
            "owner_id": self.owner,
            "project_alias": project,
            "id": task_id.to_string(),
            "idempotency_key": task_id.to_string(),
            "intent_digest": sha256_canonical(&intent),
            "base_revision": base,
            "mode": mode,
            "agent_profile": profile,
            "title": prepare_title(prompt)?,
            "prompt": prompt,
            "acceptance_criteria": criteria,
            "pinned_recipe": recipe["recipe"],
            "recipe_digest": recipe["digest"],
            "status": "queued",
            "revision": 1,
            "auto_repair": false,
            "review_required": false,
        }))?;
        tx.insert_workflow_task(project, task_id, uuid_of(w, "workflow_id"), role)?;
        tx.insert_event(
            project,
            task_id,
            "queued",
            json!({
                "workflow_id": uuid_of(w, "workflow_id").to_string(),
                "status": "queued",
                "revision": 1,
                "task_id": task_id.to_string(),
                "project_alias": project,
                "recipe_digest": recipe["digest"],
            }),
            &Actor::new(ActorKind::Agent, profile),
            None,
            None,
            true,
        )?;
        tx.notify_jobs()
    }

    /// _queue_review: bind the immutable target, insert the
    /// reviewer task and the review row, advance to the
    /// review phase.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn queue_review(
        &self,
        tx: &mut dyn ExternalTx,
        w: &mut Value,
        task: &Value,
        artifact: &Value,
        kind: &str,
        retry: i64,
        correction: Option<&str>,
    ) -> Result<(), WorkError> {
        let task_id = Uuid::new_v4();
        let review_id = Uuid::new_v4();
        let recipe = w["snapshot"]["recipes"]["reviewer_review"].clone();
        let root = self.task(tx, w, Some(uuid_of(w, "root_task_id")))?;
        let candidate = if kind == "code" && !w["state"]["candidate"].is_null() {
            Some(w["state"]["candidate"].clone())
        } else {
            None
        };
        let review_base_revision = match &candidate {
            Some(candidate) => candidate["candidate_commit"].clone(),
            None => w["state"]["base_revision"].clone(),
        };
        let attestations: Vec<Value> = match &candidate {
            Some(candidate) => candidate["protected_gate_changes"]
                .as_array()
                .map(|changes| {
                    changes
                        .iter()
                        .map(|change| change["finding_id"].clone())
                        .collect()
                })
                .unwrap_or_default(),
            None => Vec::new(),
        };
        let mut target = json!({
            "version": if candidate.is_some()
                && w["state"]["identity_protocol"].as_str() == Some("content-v2") {
                2
            } else {
                1
            },
            "kind": "external_database_review",
            "workflow_id": uuid_of(w, "workflow_id").to_string(),
            "review_id": review_id.to_string(),
            "reviewer_task_id": task_id.to_string(),
            "reviewer_profile": text(w, "reviewer_profile"),
            "target_kind": kind,
            "source_task_id": uuid_of(task, "id").to_string(),
            "source_assignment_id": artifact["assignment_id"],
            "source_sha256": artifact["sha256"],
            "source_revision": task["revision"],
            "source_base_revision": task["base_revision"],
            "review_base_revision": review_base_revision,
            "request_prompt": root["prompt"],
            "acceptance_criteria": root["acceptance_criteria"],
            "source_content": artifact["content"],
            "approved_plan": if kind == "code" {
                w["state"]["approved_plan"].clone()
            } else {
                Value::Null
            },
            "candidate": candidate,
            "required_attestations": attestations,
        });
        crate::defect::merge_defect_context(&mut target, &root);
        let raw = canonical_json(&target);
        let digest = sha256_hex(raw.as_bytes());
        validate_target(&raw, &digest).map_err(WorkError::store)?;
        let mut prompt = format!(
            "Independently review the complete external {kind} artifact supplied in external-review.json. Return only the bound structured verdict; do not edit or publish source."
        );
        if let Some(correction) = correction {
            prompt += &format!(
                " Previous review could not be consumed: {} Correct the JSON contract against the complete unchanged source; do not echo Markdown or manufacture approval.",
                correction.chars().take(1024).collect::<String>()
            );
        }
        self.insert_task(
            tx,
            w,
            task_id,
            "review",
            text(w, "reviewer_profile"),
            &recipe,
            &prompt,
            "Review the full supplied artifact and current source; verify every protected gate change.",
            text(&target, "review_base_revision"),
            "reviewer",
        )?;
        if root["task_kind"].as_str() == Some("defect") {
            tx.update_task_row(
                text(w, "project_alias"),
                task_id,
                &json!({"task_kind": "defect", "defect": root["defect"]}),
            )?;
        }
        tx.insert_review_row(&json!({
            "review_id": review_id.to_string(),
            "workflow_id": uuid_of(w, "workflow_id").to_string(),
            "owner_id": self.owner,
            "project_alias": text(w, "project_alias"),
            "source_task_id": uuid_of(task, "id").to_string(),
            "source_assignment_id": artifact["assignment_id"],
            "source_sha256": artifact["sha256"],
            "target_kind": kind,
            "reviewer_task_id": task_id.to_string(),
            "target_json": raw,
            "target_sha256": digest,
        }))?;
        w["state"]["active_review_id"] = json!(review_id.to_string());
        w["state"]["review_retries"] = json!(retry);
        self.save(
            tx,
            w,
            &format!(
                "Independent Codex {kind} review queued with the complete immutable artifact."
            ),
            Some(if kind == "plan" {
                "plan_review"
            } else {
                "code_review"
            }),
            Some(task_id),
            None,
        )
    }

    /// _native_artifact: the reviewer's published result,
    /// chunk-verified against its reconciliation receipt.
    pub(crate) fn native_artifact(
        &self,
        tx: &mut dyn ExternalTx,
        w: &Value,
        task: &Value,
        accepted_revision: Option<i64>,
    ) -> Result<Option<Value>, WorkError> {
        let revision = match accepted_revision {
            None => {
                if text(task, "status") != "awaiting_review" {
                    return Ok(None);
                }
                int_of(task, "revision")
            }
            Some(accepted) => {
                if text(task, "status") != "completed" || int_of(task, "revision") != accepted + 1 {
                    return Ok(None);
                }
                accepted
            }
        };
        let Some(attempt_id) = opt_uuid(task, "latest_attempt_id") else {
            return Ok(None);
        };
        let Some(a) = tx.native_attempt(
            text(w, "project_alias"),
            uuid_of(task, "id"),
            attempt_id,
            revision,
        )?
        else {
            return Ok(None);
        };
        let byte_count = a["result_byte_count"].as_i64().unwrap_or(0);
        if byte_count == 0 || byte_count > crate::external_workflow::MAX_NATIVE_VERDICT as i64 {
            return Err(WorkError::store("Native reviewer verdict exceeds contract"));
        }
        let chunks = tx.result_chunks(
            text(w, "project_alias"),
            uuid_of(task, "id"),
            uuid_of(&a, "attempt_id"),
        )?;
        let content: String = chunks.concat();
        if chunks.len() as i64 != a["result_chunk_count"].as_i64().unwrap_or(-1)
            || content.len() as i64 != byte_count
            || sha256_hex(content.as_bytes()) != text(&a, "result_sha256")
        {
            return Err(WorkError::store(
                "Native reviewer result does not match its receipt",
            ));
        }
        Ok(Some(json!({
            "content": content,
            "attempt_id": uuid_of(&a, "attempt_id").to_string(),
            "sha256": a["result_sha256"],
            "receipt_id": uuid_of(&a, "receipt_id").to_string(),
        })))
    }

    /// _accept_task: terminal acceptance of a non-root
    /// task (the root envelope completes only at merge).
    pub(crate) fn accept_task(
        &self,
        tx: &mut dyn ExternalTx,
        w: &Value,
        task: &Value,
    ) -> Result<(), WorkError> {
        if uuid_of(task, "id") == uuid_of(w, "root_task_id") {
            return Ok(());
        }
        if text(task, "status") == "completed" {
            return Ok(());
        }
        if text(task, "status") != "awaiting_review" {
            return Err(WorkError::store("Task is not reviewable"));
        }
        tx.update_task_row(
            text(w, "project_alias"),
            uuid_of(task, "id"),
            &json!({
                "status": "completed",
                "accepted_at": {"$now": true},
                "revision": {"$incr": 1},
            }),
        )?;
        Ok(())
    }

    /// _consume_review: consume the active review's native
    /// verdict exactly once.
    pub(crate) fn consume_review(
        &self,
        tx: &mut dyn ExternalTx,
        w: &mut Value,
    ) -> Result<bool, WorkError> {
        let review_id = opt_uuid(&w["state"], "active_review_id")
            .ok_or_else(|| WorkError::store("External workflow review binding changed"))?;
        let review = tx
            .review_row(review_id, LockMode::Update)?
            .filter(|review| text(review, "status") == "reviewing")
            .ok_or_else(|| WorkError::store("External workflow review binding changed"))?;
        let critic = self.task(tx, w, Some(uuid_of(&review, "reviewer_task_id")))?;
        let task = self.task(tx, w, Some(uuid_of(&review, "source_task_id")))?;
        let source = self
            .artifact(tx, w, &task)?
            .filter(|source| {
                source["assignment_id"] == review["source_assignment_id"]
                    && source["sha256"] == review["source_sha256"]
            })
            .ok_or_else(|| WorkError::store("External source changed while reviewing"))?;
        let result = match self.native_artifact(tx, w, &critic, None) {
            Ok(result) => result,
            Err(WorkError::Store { .. }) => {
                return self.retry_review(
                    tx,
                    w,
                    &review,
                    (&task, &source),
                    "Native reviewer result was oversized or did not match its immutable publication receipt.",
                    false,
                );
            }
            Err(error) => return Err(error),
        };
        let Some(result) = result else {
            if matches!(
                text(&critic, "status"),
                "failed"
                    | "needs_reconcile"
                    | "quota_blocked"
                    | "blocked_auth"
                    | "blocked_permission"
                    | "cancelled"
            ) {
                return self.retry_review(
                    tx,
                    w,
                    &review,
                    (&task, &source),
                    &format!(
                        "Native reviewer could not finish: {}",
                        text(&critic, "status")
                    ),
                    text(&critic, "status") != "cancelled",
                );
            }
            return Ok(false);
        };
        let target = validate_target(text(&review, "target_json"), text(&review, "target_sha256"))
            .map_err(WorkError::store)?;
        let verdict = match parse_verdict(
            text(&result, "content"),
            &target,
            text(&review, "target_sha256"),
        ) {
            Ok(verdict) => verdict,
            Err(_) => {
                return self.retry_review(
                    tx,
                    w,
                    &review,
                    (&task, &source),
                    "Native reviewer returned an invalid bound verdict.",
                    false,
                );
            }
        };
        let receipt = json!({
            "review_id": review_id.to_string(),
            "reviewer_task_id": uuid_of(&critic, "id").to_string(),
            "reviewer_attempt_id": result["attempt_id"],
            "reviewer_result_sha256": result["sha256"],
            "reconciliation_receipt_id": result["receipt_id"],
            "context_sha256": review["target_sha256"],
            "source_sha256": source["sha256"],
            "decision": verdict["decision"],
        });
        let status = match text(&verdict, "decision") {
            "approve" => "approved",
            "revise" => "revise",
            _ => "escalated",
        };
        tx.update_review_row(
            review_id,
            &json!({
                "status": status,
                "reviewer_attempt_id": result["attempt_id"],
                "reviewer_result_sha256": result["sha256"],
                "verdict": verdict,
                "receipt": receipt,
            }),
        )?;
        self.accept_task(tx, w, &critic)?;
        self.resolve_retry(
            tx,
            w,
            "reviewer_correction",
            &format!("review:{review_id}"),
            text(&result, "sha256"),
        )?;
        match text(&verdict, "decision") {
            "escalate" => {
                self.escalate(
                    tx,
                    w,
                    text(&verdict, "summary"),
                    true,
                    Some("review_verdict"),
                )?;
                Ok(true)
            }
            "revise" => self.revise(
                tx,
                w,
                text(&review, "target_kind"),
                &verdict,
                &source,
                ReviseOptions::default(),
            ),
            _ => {
                self.accept_task(tx, w, &task)?;
                self.resolve_retry(
                    tx,
                    w,
                    if text(&review, "target_kind") == "plan" {
                        "planning"
                    } else {
                        "review"
                    },
                    &format!("review:{review_id}"),
                    text(&result, "sha256"),
                )?;
                if text(&review, "target_kind") == "plan" {
                    w["state"]["approved_plan"] = json!({
                        "assignment_id": source["assignment_id"],
                        "sha256": source["sha256"],
                        "content": source["content"],
                    });
                    w["state"]["plan_review"] = receipt;
                    self.queue_author(tx, w, "implement", &source, None, None, None, None)?;
                } else {
                    w["state"]["code_review"] = receipt;
                    self.save(
                        tx,
                        w,
                        "Independent code review approved; trusted Jenkins verification is next.",
                        Some("checks"),
                        None,
                        None,
                    )?;
                }
                Ok(true)
            }
        }
    }

    /// _retry_review: requeue the same review with the
    /// failure as its correction, under the retry budget.
    /// Only the trusted terminal-state path in consume_review
    /// supplies infrastructure=true; malformed or
    /// receipt-mismatched output keeps reviewer-correction
    /// semantics regardless of its text.
    pub(crate) fn retry_review(
        &self,
        tx: &mut dyn ExternalTx,
        w: &mut Value,
        review: &Value,
        source_binding: (&Value, &Value),
        summary: &str,
        infrastructure: bool,
    ) -> Result<bool, WorkError> {
        let (task, source) = source_binding;
        let review_id = uuid_of(review, "review_id");
        if tx.task_attempt_active(uuid_of(review, "reviewer_task_id"))? {
            return Ok(false);
        }
        let critic = self.task(tx, w, Some(uuid_of(review, "reviewer_task_id")))?;
        if text(&critic, "status") == "cancelled" {
            self.escalate(tx, w, summary, true, None)?;
            return Ok(true);
        }
        let mut retry = int_of(&w["state"], "review_retries") + 1;
        if infrastructure {
            // Persist the wait before reserving an admission.
            // Coordinator restarts and repeated ticks cannot
            // spend retries or launch reviewers early.
            let now = tx.clock_now()?;
            let pending = w["state"]["review_retry_wait"].clone();
            if pending.is_null() {
                let retries = int_of(&w["state"], "review_retries").clamp(0, 6);
                let delay = std::cmp::min(300, 5 * 2i64.pow(retries as u32));
                w["state"]["review_retry_wait"] = json!({
                    "review_id": review_id.to_string(),
                    "not_before": format_iso(now + i128::from(delay) * 1_000_000_000),
                });
                self.save(
                    tx,
                    w,
                    "Review infrastructure unavailable; waiting before retry.",
                    None,
                    None,
                    None,
                )?;
                return Ok(true);
            }
            if pending["review_id"].as_str() != Some(review_id.to_string().as_str()) {
                return Err(WorkError::store("Review retry wait binding changed"));
            }
            let not_before = parse_ts(text(&pending, "not_before"))
                .ok_or_else(|| WorkError::store("Review retry wait timestamp is invalid"))?;
            if now < not_before {
                return Ok(false);
            }
            if let Some(state) = w["state"].as_object_mut() {
                state.remove("review_retry_wait");
            }
        }
        if !self.retry_events {
            if retry > AGENT_RETRY_LIMIT {
                self.escalate(tx, w, summary, !infrastructure, None)?;
                return Ok(true);
            }
        } else {
            let source_id = format!("reviewer:{review_id}");
            let evidence = sha256_canonical(&json!({
                "target": review["target_sha256"],
                "task_id": uuid_of(&critic, "id").to_string(),
                "attempt_id": critic["latest_attempt_id"]
                    .as_str()
                    .unwrap_or("None"),
                "summary": summary,
            }));
            let event = self.reserve_retry(
                tx,
                w,
                "reviewer_correction",
                &source_id,
                &evidence,
                &source_id,
                &[],
                None,
                &[],
                infrastructure,
            )?;
            let Some(event) = event else {
                return Ok(true);
            };
            retry = int_of(&event, "used");
        }
        tx.update_review_row(review_id, &json!({"status": "retry"}))?;
        self.queue_review(
            tx,
            w,
            task,
            source,
            text(review, "target_kind"),
            retry,
            Some(summary),
        )?;
        Ok(true)
    }
}

impl ExternalWorkflowStore {
    /// _revise: one bounded author revision from feedback.
    pub(crate) fn revise(
        &self,
        tx: &mut dyn ExternalTx,
        w: &mut Value,
        kind: &str,
        feedback: &Value,
        source: &Value,
        options: ReviseOptions,
    ) -> Result<bool, WorkError> {
        let key = if kind == "plan" {
            "plan_revisions"
        } else {
            "code_revisions"
        };
        let mut event = None;
        if !self.retry_events {
            if int_of(w, key) >= self.retry_limit(w, &format!("max_{key}"))? {
                self.escalate(
                    tx,
                    w,
                    &format!(
                        "The {kind} revision budget is exhausted. {}",
                        text(feedback, "summary")
                    ),
                    true,
                    None,
                )?;
                return Ok(true);
            }
        } else {
            self.retry_limit(w, &format!("max_{key}"))?;
            let retry_kind = options.retry_kind.unwrap_or_else(|| {
                if kind == "plan" {
                    "planning".to_string()
                } else {
                    "review".to_string()
                }
            });
            let source_id = options.source_id.unwrap_or_else(|| {
                let anchor = w["state"]["active_review_id"]
                    .as_str()
                    .unwrap_or_else(|| text(source, "assignment_id"));
                format!("review:{anchor}")
            });
            let evidence_digest = options.evidence_digest.unwrap_or_else(|| {
                sha256_canonical(&json!({
                    "source_sha256": source["sha256"],
                    "feedback": feedback,
                }))
            });
            let action_key = format!(
                "author:{}",
                sha256_canonical(&json!({
                    "source_id": source_id.clone(),
                    "source_sha256": source["sha256"],
                    "kind": retry_kind,
                }))
            );
            let reserved = self.reserve_retry(
                tx,
                w,
                &retry_kind,
                &source_id,
                &evidence_digest,
                &action_key,
                &options.passed_kinds,
                options.phase_id.as_deref(),
                &options.passed_phase_ids,
                false,
            )?;
            let Some(reserved) = reserved else {
                return Ok(true);
            };
            event = Some(reserved);
        }
        let next = int_of(w, key) + 1;
        w[key] = json!(next);
        self.queue_author(
            tx,
            w,
            if kind == "plan" { "plan" } else { "implement" },
            source,
            Some(feedback),
            event.as_ref(),
            None,
            None,
        )?;
        Ok(true)
    }

    /// _author_prompt: the exact claim-context instructions.
    fn author_prompt(mode: &str) -> String {
        let common = "Verify the SHA-256 of the exact UTF-8 claim.external_context_json against claim.external_context_sha256, then parse it once. Follow its instructions, original request, approved plan and feedback at the supplied base_revision. Preserve existing work. Never merge main or manufacture process proofs, approvals, CI results or receipts.";
        if mode == "plan" {
            format!("{common} Return the full revised plan.")
        } else {
            format!("{common} Implement, verify and publish the authorized feature candidate. Return only JSON matching result_contract, with the actual commit, tree and supplied base_revision.")
        }
    }

    /// _queue_author: insert the author task, persist the
    /// phase, enqueue its v2 context, then clear the review
    /// bindings from state.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn queue_author(
        &self,
        tx: &mut dyn ExternalTx,
        w: &mut Value,
        mode: &str,
        source: &Value,
        feedback: Option<&Value>,
        retry_event: Option<&Value>,
        task_id: Option<Uuid>,
        summary: Option<&str>,
    ) -> Result<Uuid, WorkError> {
        let task_id = task_id.unwrap_or_else(Uuid::new_v4);
        let root = self.task(tx, w, Some(uuid_of(w, "root_task_id")))?;
        let recipe = w["snapshot"]["recipes"][if mode == "plan" {
            "author_plan"
        } else {
            "author_implement"
        }]
        .clone();
        let phase = if mode == "plan" {
            if feedback.is_some() {
                "plan_revision"
            } else {
                "plan"
            }
        } else if feedback.is_some() {
            "repair"
        } else {
            "implement"
        };
        w["active_task_id"] = json!(task_id.to_string());
        let mut prompt = Self::author_prompt(mode);
        if let Some(event) = retry_event {
            prompt += &format!(
                " The current {} failure event is {}; this is retry {} of {}. Retain that event identity across revisions.",
                text(event, "kind").replace('_', " "),
                text(event, "event_id"),
                int_of(event, "used"),
                int_of(event, "limit"),
            );
        }
        let base = w["state"]["base_revision"]
            .as_str()
            .unwrap_or_else(|| text(&root, "base_revision"))
            .to_string();
        self.insert_task(
            tx,
            w,
            task_id,
            mode,
            text(w, "author_profile"),
            &recipe,
            &prompt,
            text(&root, "acceptance_criteria"),
            &base,
            "author",
        )?;
        if root["task_kind"].as_str() == Some("defect") {
            tx.update_task_row(
                text(w, "project_alias"),
                task_id,
                &json!({"task_kind": "defect", "defect": root["defect"]}),
            )?;
        }
        let default_summary = format!(
            "{} {} queued from the reviewed artifact.",
            text(w, "author_profile"),
            if feedback.is_some() {
                "revision"
            } else {
                "implementation"
            }
        );
        self.save(
            tx,
            w,
            summary.unwrap_or(&default_summary),
            Some(phase),
            Some(task_id),
            None,
        )?;
        let mut context = json!({
            "version": 2,
            "workflow_id": uuid_of(w, "workflow_id").to_string(),
            "stage": mode,
            "request_prompt": root["prompt"],
            "acceptance_criteria": root["acceptance_criteria"],
            "approved_plan": w["state"]["approved_plan"],
            "previous_artifact": {
                "assignment_id": source["assignment_id"],
                "sha256": source["sha256"],
                "content": source["content"],
            },
            "feedback": feedback,
            "base_revision": base,
            "result_contract": if mode == "implement" {
                json!({
                    "version": 1,
                    "candidate_commit": "40 lowercase hex",
                    "candidate_tree": "40 lowercase hex",
                    "base_revision": "the supplied base_revision",
                    "summary": "bounded implementation report",
                    "source_branch": "optional Git feature branch name",
                })
            } else {
                json!("Full revised plan text")
            },
        });
        crate::defect::merge_defect_context(&mut context, &root);
        let assignment = self.enqueue_ext(
            tx,
            text(w, "project_alias"),
            task_id,
            text(w, "author_profile"),
            &context,
        )?;
        state_pop(w, "active_review_id");
        state_pop(w, "candidate");
        state_pop(w, "code_review");
        state_pop(w, "candidate_source");
        tx.update_workflow_state(uuid_of(w, "workflow_id"), &w["state"].clone(), None)?;
        Ok(assignment)
    }

    /// _delivery_request: the verify/merge action request
    /// bound to the reviewed candidate.
    fn delivery_request(&self, w: &Value) -> Value {
        let candidate = w["state"]["candidate"].clone();
        let mut request = candidate.clone();
        request["candidate"] = candidate.clone();
        request["base_revision"] = candidate["integration_base"].clone();
        request["source_task_id"] = json!(uuid_of(w, "active_task_id").to_string());
        if let Some(source) = w["state"]["candidate_source"].as_object() {
            for (key, value) in source {
                request[key] = value.clone();
            }
        }
        request["approved_plan_sha256"] = w["state"]["approved_plan"]["sha256"].clone();
        request["plan_review"] = w["state"]["plan_review"].clone();
        request["code_review"] = w["state"]["code_review"].clone();
        request
    }

    /// _consume_verification: the verify action's terminal
    /// result, with the protected stage observation closing
    /// passed retry kinds.
    pub(crate) fn consume_verification(
        &self,
        tx: &mut dyn ExternalTx,
        w: &mut Value,
    ) -> Result<bool, WorkError> {
        let request = self.delivery_request(w);
        let a = self.action(
            tx,
            w,
            "verify",
            text(&w["state"]["candidate"], "candidate_commit"),
            &request,
        )?;
        if !matches!(text(&a, "status"), "completed" | "failed") {
            return Ok(false);
        }
        let result = a["result"].clone();
        let receipt = result["receipt"].clone();
        let candidate = w["state"]["candidate"].clone();
        let mut observation = None;
        if self.retry_events {
            let observed = match &self.verification {
                Some(port) => port.verified_stage_observation(w, &a, &receipt),
                None => Err("verification port unavailable".to_string()),
            };
            match observed {
                Ok(value) => observation = Some(value),
                Err(_) => {
                    self.escalate(
                        tx,
                        w,
                        "Trusted verification lacks exact source-bound stopped stage evidence; no repair attempt was admitted.",
                        true,
                        None,
                    )?;
                    return Ok(true);
                }
            }
        }
        if text(&a, "status") == "failed" {
            if let Some(observation) = &observation {
                if observation["failure_kind"].is_null() {
                    let scope = self.retry_scope(w)?;
                    for kind in string_list(&observation["passed_kinds"]) {
                        tx.resolve_verified(
                            &scope,
                            &kind,
                            &format!("delivery:{}", uuid_of(&a, "action_id")),
                            text(observation, "stage_evidence_digest"),
                            None,
                        )?;
                    }
                    self.escalate(
                        tx,
                        w,
                        "All protected phases passed but Jenkins did not report SUCCESS. The terminal delivery mismatch requires reconciliation; no source repair was admitted.",
                        true,
                        None,
                    )?;
                    return Ok(true);
                }
            }
            self.cleanup_action(tx, w, &a)?;
            let source_task = self.task(tx, w, None)?;
            let source = self
                .artifact(tx, w, &source_task)?
                .ok_or_else(|| WorkError::store("External author artifact disappeared"))?;
            let options = match &observation {
                None => ReviseOptions::default(),
                Some(observation) => ReviseOptions {
                    retry_kind: Some(
                        observation["failure_kind"]
                            .as_str()
                            .unwrap_or("verification")
                            .to_string(),
                    ),
                    source_id: Some(format!("delivery:{}", uuid_of(&a, "action_id"))),
                    evidence_digest: Some(text(observation, "stage_evidence_digest").to_string()),
                    passed_kinds: string_list(&observation["passed_kinds"]),
                    phase_id: Some(
                        observation["phase_id"]
                            .as_str()
                            .unwrap_or("verification")
                            .to_string(),
                    ),
                    passed_phase_ids: string_list(&observation["passed_phase_ids"]),
                },
            };
            return self.revise(
                tx,
                w,
                "code",
                &json!({
                    "summary": format!(
                        "Trusted verification failed: {}",
                        text(&result, "summary")
                    ),
                    "findings": [],
                }),
                &source,
                options,
            );
        }
        if [
            "candidate_commit",
            "candidate_tree",
            "gate_manifest_sha256",
            "branch_scope",
        ]
        .iter()
        .any(|key| receipt[*key] != candidate[*key])
            || receipt["result"].as_str() != Some("SUCCESS")
        {
            return Err(WorkError::store(
                "Jenkins receipt is not bound to the approved exact source",
            ));
        }
        if let Some(observation) = &observation {
            let scope = self.retry_scope(w)?;
            for kind in string_list(&observation["passed_kinds"]) {
                tx.resolve_verified(
                    &scope,
                    &kind,
                    &format!("delivery:{}", uuid_of(&a, "action_id")),
                    text(observation, "stage_evidence_digest"),
                    None,
                )?;
            }
        }
        w["state"]["verification"] = receipt;
        self.save(
            tx,
            w,
            "Independent review and exact-source Jenkins checks passed; trusted merge is starting.",
            Some("merging"),
            None,
            None,
        )?;
        Ok(true)
    }

    /// _consume_merge: the merge action's verified receipt
    /// completes the workflow and the root envelope.
    pub(crate) fn consume_merge(
        &self,
        tx: &mut dyn ExternalTx,
        w: &mut Value,
    ) -> Result<bool, WorkError> {
        let mut request = self.delivery_request(w);
        request["verification"] = w["state"]["verification"].clone();
        let a = self.action(
            tx,
            w,
            "merge",
            text(&w["state"]["candidate"], "candidate_commit"),
            &request,
        )?;
        if !matches!(text(&a, "status"), "completed" | "failed") {
            return Ok(false);
        }
        let result = a["result"].clone();
        let receipt = result["receipt"].clone();
        if text(&result, "status") == "main_changed" {
            return self.request_integration_refresh(tx, w, &a);
        }
        let candidate = w["state"]["candidate"].clone();
        if text(&result, "status") != "merged"
            || receipt["merge_commit"] != candidate["candidate_commit"]
            || receipt["candidate_tree"] != candidate["candidate_tree"]
            || receipt["expected_main"] != candidate["integration_base"]
        {
            self.escalate(
                tx,
                w,
                &format!(
                    "Trusted merge could not be verified: {}",
                    text(&result, "summary")
                ),
                true,
                None,
            )?;
            return Ok(true);
        }
        self.resolve_retry(
            tx,
            w,
            "merge",
            &format!("delivery:{}", uuid_of(&a, "action_id")),
            &sha256_canonical(&receipt),
        )?;
        w["state"]["merge_receipt"] = receipt.clone();
        w["enabled"] = json!(false);
        self.save(
            tx,
            w,
            "The exact reviewed and Jenkins-verified candidate is on main.",
            Some("completed"),
            None,
            None,
        )?;
        let root = self.task(tx, w, Some(uuid_of(w, "root_task_id")))?;
        if !root["latest_attempt_id"].is_null()
            || text(&root, "agent_profile") != text(w, "author_profile")
        {
            return Err(WorkError::store("Root external envelope identity changed"));
        }
        let mut set = json!({
            "status": "completed",
            "cancelled_at": null,
            "revision": {"$incr": 1},
        });
        if root["accepted_at"].is_null() {
            set["accepted_at"] = json!({"$now": true});
        }
        tx.update_task_row(text(w, "project_alias"), uuid_of(&root, "id"), &set)?;
        let evidence = json!({
            "workflow_id": uuid_of(w, "workflow_id").to_string(),
            "phase": "completed",
            "summary": "Trusted exact-source merge completed the external assignment.",
            "status": "completed",
            "base_revision": root["revision"],
            "revision": int_of(&root, "revision") + 1,
            "delivery_action_id": uuid_of(&a, "action_id").to_string(),
            "candidate_commit": candidate["candidate_commit"],
            "candidate_tree": candidate["candidate_tree"],
            "gate_manifest_sha256": candidate["gate_manifest_sha256"],
            "merge_commit": receipt["merge_commit"],
        });
        tx.insert_event(
            text(w, "project_alias"),
            uuid_of(&root, "id"),
            "completed",
            evidence,
            &coordinator_actor(),
            None,
            None,
            false,
        )?;
        Ok(true)
    }

    /// _request_integration_refresh: main drift invalidates
    /// checked source, not the author's completed work.
    pub(crate) fn request_integration_refresh(
        &self,
        tx: &mut dyn ExternalTx,
        w: &mut Value,
        action: &Value,
    ) -> Result<bool, WorkError> {
        if self.retry_events {
            let event = self.reserve_retry(
                tx,
                w,
                "merge",
                &format!("delivery:{}", uuid_of(action, "action_id")),
                &sha256_canonical(&action["result"]),
                &format!("prepare-refresh:{}", uuid_of(action, "action_id")),
                &[],
                None,
                &[],
                false,
            )?;
            if event.is_none() {
                return Ok(true);
            }
        } else {
            let used = int_of(&w["state"], "trusted_main_refreshes");
            if used + int_of(w, "code_revisions") >= self.retry_limit(w, "max_code_revisions")? {
                self.escalate(
                    tx,
                    w,
                    "The bounded main integration budget is exhausted.",
                    true,
                    None,
                )?;
                return Ok(true);
            }
            w["state"]["trusted_main_refreshes"] = json!(used + 1);
        }
        w["state"]["prepare_refresh_epoch"] = json!(uuid_of(action, "action_id").to_string());
        for key in [
            "candidate",
            "candidate_source",
            "code_review",
            "verification",
            "active_review_id",
        ] {
            state_pop(w, key);
        }
        self.save(
            tx,
            w,
            "Main advanced; trusted preparation will integrate the preserved author artifact and repeat independent review and exact-source CI.",
            Some("repair"),
            None,
            None,
        )?;
        Ok(true)
    }

    /// _request_rebase: park the preserved implementation
    /// for a bounded rebase onto the new trusted base.
    pub(crate) fn request_rebase(
        &self,
        tx: &mut dyn ExternalTx,
        w: &mut Value,
        summary: &str,
        action: Option<&Value>,
    ) -> Result<bool, WorkError> {
        if !self.retry_events
            && int_of(w, "code_revisions") >= self.retry_limit(w, "max_code_revisions")?
        {
            self.escalate(
                tx,
                w,
                &format!("The bounded source/revision budget is exhausted. {summary}"),
                true,
                None,
            )?;
            return Ok(true);
        }
        if let Some(action) = action {
            w["state"]["pending_rebase_cause"] = json!({
                "source_id": format!("delivery:{}", uuid_of(action, "action_id")),
                "digest": sha256_canonical(&action["result"]),
            });
            w["state"]["prepare_refresh_epoch"] = json!(uuid_of(action, "action_id").to_string());
        }
        state_pop(w, "base_revision");
        w["state"]["pending_rebase"] = json!(summary.chars().take(2048).collect::<String>());
        self.save(
            tx,
            w,
            "Main changed; selecting the new trusted source before bounded rebase.",
            Some("repair"),
            None,
            None,
        )?;
        Ok(true)
    }

    /// _escalate: retain the evidence, bound the public
    /// reason, close the retry scope.
    pub(crate) fn escalate(
        &self,
        tx: &mut dyn ExternalTx,
        w: &mut Value,
        reason: &str,
        cancel_children: bool,
        cause: Option<&str>,
    ) -> Result<(), WorkError> {
        let root = self.task(tx, w, Some(uuid_of(w, "root_task_id")))?;
        let phase = text(w, "phase");
        if !TERMINAL.contains(&phase)
            && phase != "merging"
            && matches!(text(&root, "status"), "cancelled" | "cancel_requested")
        {
            self.cancel_locked(
                tx,
                w,
                "Root workflow task was cancelled.",
                &coordinator_actor(),
                "workflow_cancelled",
            )?;
            return Ok(());
        }
        if cancel_children {
            self.cancel_children(tx, w, &coordinator_actor(), "workflow_cancelled")?;
        }
        let safe: String = reason
            .chars()
            .filter(|ch| !ch.is_control() || matches!(ch, '\r' | '\n' | '\t'))
            .collect();
        let bounded = truncate_bytes(&safe, 2048);
        let code = crate::work_events::escalation_cause(reason, cause).map_err(WorkError::store)?;
        w["enabled"] = json!(false);
        w["state"]["escalation_reason"] = json!(bounded);
        self.save(
            tx,
            w,
            &bounded,
            Some("escalated"),
            None,
            Some(&json!({"escalation_cause": code})),
        )?;
        self.resolve_open_retries(tx, w)?;
        Ok(())
    }

    /// _cancel_children: cancel every live child task and
    /// fail open delivery actions, queueing verify cleanups.
    pub(crate) fn cancel_children(
        &self,
        tx: &mut dyn ExternalTx,
        w: &Value,
        actor: &Actor,
        cause: &str,
    ) -> Result<(), WorkError> {
        let workflow_id = uuid_of(w, "workflow_id");
        let project = text(w, "project_alias");
        for task in tx.workflow_tasks_locked(workflow_id)? {
            let task_id = uuid_of(&task, "id");
            let status = if tx.task_attempt_active(task_id)? {
                "cancel_requested"
            } else {
                "cancelled"
            };
            tx.update_task_row(
                project,
                task_id,
                &json!({
                    "status": status,
                    "cancelled_at": if status == "cancelled" {
                        json!({"$now": true})
                    } else {
                        Value::Null
                    },
                    "revision": {"$incr": 1},
                }),
            )?;
            tx.insert_event(
                project,
                task_id,
                status,
                json!({
                    "base_revision": task["revision"],
                    "revision": int_of(&task, "revision") + 1,
                    "workflow_id": workflow_id.to_string(),
                }),
                actor,
                Some(cause),
                None,
                false,
            )?;
        }
        let cleanup = tx.verify_actions(workflow_id)?;
        tx.fail_open_actions(
            workflow_id,
            &json!({
                "status": "failed",
                "receipt": {},
                "summary": "Workflow admission stopped before delivery completed.",
            }),
        )?;
        for action in &cleanup {
            self.cleanup_action(tx, w, action)?;
        }
        Ok(())
    }

    /// _cancel_locked: the shared cancel body (public
    /// cancel, step observation, escalation reroute).
    pub(crate) fn cancel_locked(
        &self,
        tx: &mut dyn ExternalTx,
        w: &mut Value,
        note: &str,
        actor: &Actor,
        cause: &str,
    ) -> Result<bool, WorkError> {
        if text(w, "phase") == "merging" {
            return Err(WorkError::rejected(409, "not_cancellable"));
        }
        if TERMINAL.contains(&text(w, "phase")) {
            return Ok(false);
        }
        self.cancel_children(tx, w, actor, cause)?;
        w["enabled"] = json!(false);
        self.save(tx, w, note, Some("cancelled"), None, None)?;
        self.resolve_open_retries(tx, w)?;
        Ok(true)
    }
}

fn string_list(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}
