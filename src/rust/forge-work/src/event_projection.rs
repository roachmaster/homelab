//! Pure public event projection; stored audit payloads never cross the wire.
use crate::{ActorKind, Cause, EventKind};
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;
use uuid::Uuid;

pub struct EventProjectionContext<'a> {
    pub owned_attempts: &'a BTreeSet<String>,
    pub external_binding: Option<&'a Value>,
    pub workflow_id: Option<Uuid>,
    pub related_allowed: bool,
}
fn canonical_id(value: &Value) -> Option<&str> {
    let text = value.as_str()?;
    (Uuid::parse_str(text).ok()?.to_string() == text).then_some(text)
}
fn keys(value: &Value, required: &[&str], optional: &[&str]) -> bool {
    value.as_object().is_some_and(|map| {
        required.iter().all(|key| map.contains_key(*key))
            && map
                .keys()
                .all(|key| required.contains(&key.as_str()) || optional.contains(&key.as_str()))
    })
}
fn display(value: &Value, maximum: usize, empty: bool, multiline: bool) -> bool {
    value.as_str().is_some_and(|text| {
        text.len() <= maximum
            && (empty || !text.trim().is_empty())
            && text
                .chars()
                .all(|c| !c.is_control() || multiline && matches!(c, '\n' | '\t'))
    })
}
fn projection_actor(payload: &Value, expected: &str) -> bool {
    payload.get("actor").is_none_or(|actor| {
        keys(actor, &["kind", "id"], &[])
            && actor["kind"].as_str() == Some(expected)
            && actor["id"]
                .as_str()
                .is_some_and(|id| !id.is_empty() && id.chars().count() <= 128)
    })
}
fn copy(payload: &Value, names: &[&str]) -> Value {
    Value::Object(
        names
            .iter()
            .filter_map(|key| payload.get(*key).map(|v| ((*key).to_string(), v.clone())))
            .collect(),
    )
}
fn attribution(kind: EventKind, payload: &Value) -> Option<Value> {
    if !kind.requires_cause()
        && !matches!(
            kind,
            EventKind::QuotaBlocked | EventKind::BlockedPermission | EventKind::BlockedAuth
        )
    {
        return None;
    }
    let mut out = Map::new();
    if let Some(actor) = payload.get("actor").filter(|actor| {
        keys(actor, &["kind", "id"], &[])
            && actor["kind"].as_str().and_then(ActorKind::parse).is_some()
            && actor["id"].as_str().is_some_and(|id| {
                !id.is_empty()
                    && id.len() <= 128
                    && id
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"_.:@/-".contains(&c))
            })
    }) {
        out.insert("actor".into(), actor.clone());
    }
    if payload["cause"]
        .as_str()
        .and_then(Cause::parse)
        .is_some_and(|cause| cause.allowed_for(kind))
    {
        out.insert("cause".into(), payload["cause"].clone());
        if display(&payload["cause_detail"], 512, false, false) {
            out.insert("detail".into(), payload["cause_detail"].clone());
        }
    }
    (!out.is_empty()).then_some(Value::Object(out))
}
fn timestamp(value: &Value) -> bool {
    let Some(text) = value.as_str() else {
        return false;
    };
    let b = text.as_bytes();
    b.len() == 20
        && b.iter().enumerate().all(|(i, c)| match i {
            4 | 7 => *c == b'-',
            10 => *c == b'T',
            13 | 16 => *c == b':',
            19 => *c == b'Z',
            _ => c.is_ascii_digit(),
        })
}
/// Admission-independent projection: the adapter supplies separately verified
/// ownership/binding facts, never facts inferred from event payloads themselves.
pub fn public_fields(
    kind: EventKind,
    payload: &Value,
    context: EventProjectionContext<'_>,
) -> Value {
    let mut out = Map::new();
    if let Some(attribution) = attribution(kind, payload) {
        out.insert("attribution".into(), attribution);
    }
    if kind == EventKind::Checkpoint {
        let native = keys(
            payload,
            &["attempt_id", "progress_sequence", "stage", "summary"],
            &["actor"],
        ) && projection_actor(payload, "worker")
            && canonical_id(&payload["attempt_id"])
                .is_some_and(|id| context.owned_attempts.contains(id))
            && payload["progress_sequence"].as_i64().is_some_and(|n| {
                (1..=128).contains(&n) && (payload["stage"] == "plan") == (n == 1)
            })
            && matches!(payload["stage"].as_str(), Some("plan" | "progress"))
            && display(&payload["summary"], 4096, false, true);
        let external = keys(
            payload,
            &["provider", "assignment_id", "generation", "summary"],
            &["actor"],
        ) && projection_actor(payload, "agent")
            && payload["provider"] == "muse-db"
            && canonical_id(&payload["assignment_id"]).is_some()
            && context.external_binding.is_some_and(|binding| {
                payload["assignment_id"] == binding["assignment_id"]
                    && payload["generation"].as_i64().is_some_and(|generation| {
                        (1..=3).contains(&generation)
                            && binding["generation"]
                                .as_i64()
                                .is_some_and(|current| generation <= current)
                    })
            })
            && display(&payload["summary"], 4096, false, true);
        let workflow = keys(
            payload,
            &["workflow_id", "phase", "summary"],
            &["actor", "related_task_id", "escalation_cause"],
        ) && projection_actor(payload, "coordinator")
            && canonical_id(&payload["workflow_id"]).is_some_and(|id| {
                context
                    .workflow_id
                    .is_some_and(|expected| expected.to_string() == id)
            })
            && matches!(
                payload["phase"].as_str(),
                Some(
                    "plan"
                        | "plan_review"
                        | "plan_revision"
                        | "implement"
                        | "checks"
                        | "code_review"
                        | "repair"
                        | "publishing"
                        | "hosted"
                        | "merging"
                        | "completed"
                        | "cancelled"
                        | "escalated"
                )
            )
            && payload.get("related_task_id").is_none_or(|id| {
                id.is_null() || context.related_allowed && canonical_id(id).is_some()
            })
            && payload.get("escalation_cause").is_none_or(|cause| {
                cause.is_null()
                    || payload["phase"] == "escalated"
                        && cause.as_str().is_some_and(|cause| {
                            crate::work_events::ESCALATION_CAUSES.contains(&cause)
                        })
            })
            && display(&payload["summary"], 4096, false, true);
        if native {
            out.insert("progress".into(),json!({"attempt_id":payload["attempt_id"],"sequence":payload["progress_sequence"],"stage":payload["stage"],"summary":payload["summary"]}));
            out.insert("progress_verified".into(), json!(true));
        } else if external {
            out.insert(
                "external_progress".into(),
                copy(
                    payload,
                    &["provider", "assignment_id", "generation", "summary"],
                ),
            );
        } else if workflow {
            let mut projected = copy(payload, &["workflow_id", "phase", "summary"]);
            for key in ["related_task_id", "escalation_cause"] {
                if let Some(value) = payload.get(key).filter(|value| !value.is_null()) {
                    projected[key] = value.clone();
                }
            }
            out.insert("workflow_progress".into(), projected);
        } else if keys(
            payload,
            &[
                "stage",
                "attempt_id",
                "extension_no",
                "previous_deadline",
                "new_deadline",
                "signal",
                "actor",
            ],
            &[],
        ) && payload["stage"] == "deadline_extended"
            && projection_actor(payload, "worker")
            && canonical_id(&payload["attempt_id"])
                .is_some_and(|id| context.owned_attempts.contains(id))
            && payload["extension_no"]
                .as_i64()
                .is_some_and(|number| (1..=64).contains(&number))
            && matches!(
                payload["signal"].as_str(),
                Some("progress_report" | "checkpoint" | "output")
            )
            && timestamp(&payload["previous_deadline"])
            && timestamp(&payload["new_deadline"])
            && payload["previous_deadline"].as_str() < payload["new_deadline"].as_str()
        {
            out.insert(
                "deadline_extension".into(),
                copy(
                    payload,
                    &[
                        "attempt_id",
                        "extension_no",
                        "previous_deadline",
                        "new_deadline",
                        "signal",
                    ],
                ),
            );
        }
    } else if kind == EventKind::Failed
        && matches!(
            payload["cause"].as_str(),
            Some(
                "malformed_output"
                    | "policy_unavailable"
                    | "context_unavailable"
                    | "resource_bound"
                    | "agent_input"
                    | "agent_failed"
            )
        )
        && payload["cause_summary"].as_str().is_some_and(|summary| {
            summary.len() <= 512
                && summary
                    .chars()
                    .all(|c| c as u32 >= 32 || matches!(c, '\t' | '\n'))
        })
    {
        out.insert(
            "failure".into(),
            json!({"cause":payload["cause"],"summary":payload["cause_summary"]}),
        );
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn raw_payload_never_becomes_public_and_native_progress_requires_ownership() {
        let attempt = Uuid::new_v4().to_string();
        let mut owned = BTreeSet::new();
        let payload = json!({"attempt_id":attempt,"progress_sequence":1,"stage":"plan","summary":"Bounded progress","actor":{"kind":"worker","id":"fixture"}});
        let render = |payload: &Value, owned: &BTreeSet<String>| {
            public_fields(
                EventKind::Checkpoint,
                payload,
                EventProjectionContext {
                    owned_attempts: owned,
                    external_binding: None,
                    workflow_id: None,
                    related_allowed: false,
                },
            )
        };
        assert_eq!(render(&payload, &owned), json!({}));
        owned.insert(attempt);
        assert_eq!(render(&payload, &owned)["progress_verified"], true);
        let mut bad = payload.clone();
        bad["command"] = json!("private fixture");
        assert_eq!(render(&bad, &owned), json!({}));
        for (key, value) in [
            ("progress_sequence", json!(true)),
            ("summary", json!("bad\0")),
            ("actor", json!({"kind":"agent","id":"fixture"})),
        ] {
            let mut bad = payload.clone();
            bad[key] = value;
            assert_eq!(render(&bad, &owned), json!({}));
        }
    }
    #[test]
    fn stop_attribution_is_bounded_and_legacy_reason_is_not_invented() {
        let owned = BTreeSet::new();
        let project = |p: &Value| {
            public_fields(
                EventKind::Cancelled,
                p,
                EventProjectionContext {
                    owned_attempts: &owned,
                    external_binding: None,
                    workflow_id: None,
                    related_allowed: false,
                },
            )
        };
        assert_eq!(
            project(&json!({"summary":"do not expose raw legacy"})),
            json!({})
        );
        let payload = json!({"actor":{"kind":"operator","id":"fixture"},"cause":"operator_request","cause_detail":"Obsolete diagnostic","secret":"private"});
        assert_eq!(
            project(&payload),
            json!({"attribution":{"actor":{"kind":"operator","id":"fixture"},"cause":"operator_request","detail":"Obsolete diagnostic"}})
        );
        let mut bad = payload;
        bad["cause"] = json!("agent_failed");
        assert_eq!(
            project(&bad),
            json!({"attribution":{"actor":{"kind":"operator","id":"fixture"}}})
        );
    }
    #[test]
    fn external_and_related_workflow_progress_require_separate_current_binding() {
        let owned = BTreeSet::new();
        let assignment = Uuid::new_v4().to_string();
        let workflow = Uuid::new_v4();
        let related = Uuid::new_v4().to_string();
        let binding = json!({"assignment_id":assignment,"generation":2});
        let external = json!({"provider":"muse-db","assignment_id":assignment,"generation":1,"summary":"External report","actor":{"kind":"agent","id":"fixture"}});
        let render = |payload: &Value, binding: Option<&Value>, workflow, related_allowed| {
            public_fields(
                EventKind::Checkpoint,
                payload,
                EventProjectionContext {
                    owned_attempts: &owned,
                    external_binding: binding,
                    workflow_id: workflow,
                    related_allowed,
                },
            )
        };
        assert!(render(&external, Some(&binding), None, false)
            .get("external_progress")
            .is_some());
        assert_eq!(render(&external, None, None, false), json!({}));
        let mut future = external;
        future["generation"] = json!(3);
        assert_eq!(render(&future, Some(&binding), None, false), json!({}));
        let flow = json!({"workflow_id":workflow.to_string(),"phase":"escalated","summary":"Recorded workflow checkpoint","related_task_id":related,"escalation_cause":"verification_failed","actor":{"kind":"coordinator","id":"fixture"}});
        assert!(render(&flow, None, Some(workflow), true)
            .get("workflow_progress")
            .is_some());
        assert_eq!(render(&flow, None, Some(workflow), false), json!({}));
        assert_eq!(render(&flow, None, Some(Uuid::new_v4()), true), json!({}));
        let mut bad = flow;
        bad["escalation_cause"] = json!("invented");
        assert_eq!(render(&bad, None, Some(workflow), true), json!({}));
    }
    #[test]
    fn failed_and_deadline_fields_are_bounded_not_raw_payloads() {
        let attempt = Uuid::new_v4().to_string();
        let owned = BTreeSet::from([attempt.clone()]);
        let project = |kind, payload: &Value, owned: &BTreeSet<String>| {
            public_fields(
                kind,
                payload,
                EventProjectionContext {
                    owned_attempts: owned,
                    external_binding: None,
                    workflow_id: None,
                    related_allowed: false,
                },
            )
        };
        let failure =
            json!({"cause":"agent_failed","cause_summary":"Recorded failure","stderr":"private"});
        assert_eq!(
            project(EventKind::Failed, &failure, &owned)["failure"],
            json!({"cause":"agent_failed","summary":"Recorded failure"})
        );
        let mut oversized = failure;
        oversized["cause_summary"] = json!("é".repeat(257));
        assert!(project(EventKind::Failed, &oversized, &owned)
            .get("failure")
            .is_none());
        let deadline = json!({"stage":"deadline_extended","attempt_id":attempt,"extension_no":1,"previous_deadline":"2026-11-28T00:00:00Z","new_deadline":"2026-11-28T00:10:00Z","signal":"progress_report","actor":{"kind":"worker","id":"fixture"}});
        assert!(project(EventKind::Checkpoint, &deadline, &owned)
            .get("deadline_extension")
            .is_some());
        assert_eq!(
            project(EventKind::Checkpoint, &deadline, &BTreeSet::new()),
            json!({})
        );
        let mut backwards = deadline.clone();
        backwards["new_deadline"] = backwards["previous_deadline"].clone();
        assert_eq!(
            project(EventKind::Checkpoint, &backwards, &owned),
            json!({})
        );
        let mut bad = deadline;
        bad["extension_no"] = json!(true);
        assert_eq!(project(EventKind::Checkpoint, &bad, &owned), json!({}));
    }
}
