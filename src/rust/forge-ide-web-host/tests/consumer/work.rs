//! External-consumer proof of opt-in work admission, without a database/agent.
use super::*;
use forge_ide_web_host::{WorkConfig, WorkMode};
use std::{
    io::{BufRead, BufReader},
    net::TcpListener,
    os::unix::fs::PermissionsExt,
};

const TASK: &str = "00000000-0000-0000-0000-000000000001";
const OTHER: &str = "00000000-0000-0000-0000-000000000002";
const PROJECT: &str = "coding-only";
const STAMP: &str = "2026-11-19T17:00:00Z";

#[test]
fn negotiated_delivery_is_typed_and_invalid_source_evidence_fails_closed() {
    let files = Files::new();
    let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
    let host = configured(&files, &upstream);
    let auth = authenticate(&host);
    let mut response = opened();
    response["event"]["task"]["delivery"] = json!({
        "verification_id":OTHER,"source_kind":"native","commit_sha":"c".repeat(40),
        "tree_sha":"d".repeat(40),"updated_at":STAMP,
        "verification":{"status":"passed","updated_at":STAMP,"summary":"Exact main passed.","build_number":7},
        "repair":{"status":"none","round":0,"max_rounds":20,"task_id":null,"updated_at":STAMP,"summary":"No repair."},
        "release":{"status":"deployed","release_id":OTHER,"receipt_sha256":"e".repeat(64),"updated_at":STAMP,"summary":"Activation confirmed."}
    });
    let mut invalid = response.clone();
    invalid["event"]["task"]["delivery"]["release"]["receipt_sha256"] = json!("not-proof");
    let worker = backend(upstream, vec![(200, response.clone()), (200, invalid)]);
    assert_reply(
        &host,
        "ide.work.get",
        task_request("work_get"),
        &auth,
        200,
        response,
    );
    assert_reply(
        &host,
        "ide.work.get",
        task_request("work_get"),
        &auth,
        503,
        failure("unavailable"),
    );
    worker.join().unwrap();
}

fn settings(files: &Files, upstream: &TcpListener) -> WorkConfig {
    let token_file = files.0.join("work.token");
    fs::write(&token_file, "a".repeat(64)).unwrap();
    fs::set_permissions(&token_file, fs::Permissions::from_mode(0o600)).unwrap();
    WorkConfig {
        endpoint: upstream.local_addr().unwrap(),
        token_file,
        // This grant is intentionally absent from the node's inspection grants.
        allowed_projects: vec![PROJECT.into()],
        allowed_profiles: vec!["agy-plan-v1".into()],
        allowed_modes: vec![WorkMode::Plan],
        mcp_presence: None,
    }
}

fn configured(files: &Files, upstream: &TcpListener) -> Host {
    let mut value = config(files);
    value.work = Some(settings(files, upstream));
    assert!(value.chat.is_none());
    host_with_config(value).0
}

#[test]
fn organized_work_list_keeps_authenticated_scope_and_validates_current_workflow_projection() {
    let files = Files::new();
    let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
    let host = configured(&files, &upstream);
    let auth = authenticate(&host);
    let mut root = summary();
    root["status"] = json!("awaiting_review");
    root["workflow_required"] = json!(true);
    root["workflow_phase"] = json!("code_review");
    root["workflow_enabled"] = json!(true);
    root["updated_at"] = json!("2026-11-19T19:00:00Z");
    let response = reply(
        "work_listed",
        json!({"project_alias":PROJECT,"tasks":[root],"next_cursor":null,
        "scope_counts":{"active":1,"attention":2,"history":3,"all":6}}),
    );
    let worker = backend(
        upstream,
        vec![(200, response.clone()), (200, response.clone())],
    );
    let mut request = common("work_list");
    request["status_filter"] = json!("active");
    assert_reply(
        &host,
        "ide.work.list",
        request.clone(),
        &auth,
        200,
        response,
    );
    request["status_filter"] = json!("attention");
    assert_reply(
        &host,
        "ide.work.list",
        request,
        &auth,
        503,
        failure("unavailable"),
    );
    worker.join().unwrap();
}

fn common(kind: &str) -> Value {
    json!({"kind":kind,"request_id":"request-1","project_alias":PROJECT})
}

fn submit() -> Value {
    let mut value = common("work_submit");
    for (key, field) in json!({"base_revision":"a".repeat(40),"mode":"plan",
        "agent_profile":"agy-plan-v1","prompt":"Review the code 🛠","acceptance_criteria":"Document findings",
        "idempotency_key":"assignment-1"}).as_object().unwrap() {
        value[key] = field.clone();
    }
    value
}

fn task_request(kind: &str) -> Value {
    let mut value = common(kind);
    value["task_id"] = json!(TASK);
    value
}

fn summary() -> Value {
    json!({"task_id":TASK,"correlation_id":OTHER,"project_alias":PROJECT,"base_revision":"a".repeat(40),"mode":"plan",
        "agent_profile":"agy-plan-v1","recipe_digest":"b".repeat(64),"status":"queued",
        "revision":1,"created_at":STAMP})
}

fn opened() -> Value {
    let mut task = summary();
    task["prompt"] = submit()["prompt"].clone();
    task["acceptance_criteria"] = submit()["acceptance_criteria"].clone();
    reply("work_opened", json!({"task":task}))
}

fn reply(kind: &str, fields: Value) -> Value {
    let mut event = fields;
    event["kind"] = json!(kind);
    event["request_id"] = json!("request-1");
    json!({"protocol_version":1,"event":event})
}

fn receipt() -> Value {
    reply(
        "work_submitted",
        json!({"task_id":TASK,"project_alias":PROJECT,
        "revision":1,"status":"queued","recipe_digest":"b".repeat(64)}),
    )
}

fn failure(code: &str) -> Value {
    reply("work_failed", json!({"code":code}))
}

fn assert_reply(
    host: &Host,
    destination: &str,
    body: Value,
    auth: &str,
    status: u16,
    expected: Value,
) {
    let response = exchange(host, post(host, destination, body, auth));
    assert!(
        response.starts_with(&format!("HTTP/1.1 {status} ")),
        "{response}"
    );
    assert!(response.contains("Cache-Control: no-store"));
    let (_, bytes) = response.split_once("\r\n\r\n").unwrap();
    assert_eq!(serde_json::from_str::<Value>(bytes).unwrap(), expected);
}

fn authenticate(host: &Host) -> String {
    let (cookie, csrf, _) = paired(host);
    format!("Cookie: {cookie}\r\nForge-CSRF: {csrf}\r\n")
}

fn no_calls(upstream: &TcpListener) {
    upstream.set_nonblocking(true).unwrap();
    assert_eq!(
        upstream.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    upstream.set_nonblocking(false).unwrap();
}

/// The selected injected loopback instance receives only the native credential.
fn backend(upstream: TcpListener, responses: Vec<(u16, Value)>) -> thread::JoinHandle<Vec<Value>> {
    thread::spawn(move || {
        let mut received = Vec::new();
        for (status, response) in responses {
            let (mut socket, _) = upstream.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut reader = BufReader::new(&mut socket);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert_eq!(line, "POST /v1/dispatch HTTP/1.1\r\n");
            let mut headers = BTreeMap::new();
            loop {
                line.clear();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                let (name, value) = line.trim_end().split_once(": ").unwrap();
                headers.insert(name.to_ascii_lowercase(), value.to_owned());
            }
            assert_eq!(
                headers["authorization"],
                format!("Bearer {}", "a".repeat(64))
            );
            assert_eq!(headers["forge-envelope-version"], "1");
            assert_eq!(headers["forge-contract-version"], "1");
            if headers["forge-destination"].starts_with("ide.work.") {
                assert_eq!(headers["forge-delivery-version"], "1");
                assert_eq!(headers["forge-retry-events-version"], "1");
                assert_eq!(headers["forge-event-attribution-version"], "1");
            } else {
                assert!(!headers.contains_key("forge-delivery-version"));
                assert!(!headers.contains_key("forge-retry-events-version"));
                assert!(!headers.contains_key("forge-event-attribution-version"));
            }
            for key in ["cookie", "origin", "forge-csrf", "forge-target-node"] {
                assert!(!headers.contains_key(key));
            }
            let mut bytes = vec![0; headers["content-length"].parse().unwrap()];
            reader.read_exact(&mut bytes).unwrap();
            let request: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(
                headers["forge-request-id"],
                request["request_id"].as_str().unwrap()
            );
            let kind = request["kind"].as_str().unwrap();
            let expected = if kind == "work_cicd_stage_log" {
                "ide.work.cicd.stage_log".into()
            } else if matches!(
                kind,
                "work_plan_decide" | "work_review_decide" | "work_publication_retry"
            ) {
                format!("ide.{}", kind.replace('_', "."))
            } else if let Some(suffix) = kind.strip_prefix("work_") {
                format!("ide.work.{suffix}")
            } else {
                format!("ide.usage.{}", kind.strip_prefix("usage_").unwrap())
            };
            assert_eq!(headers["forge-destination"], expected);
            received.push(request);
            let bytes = response.to_string();
            write!(
                socket,
                "HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\n\r\n{bytes}",
                bytes.len()
            )
            .unwrap();
        }
        received
    })
}

#[test]
fn work_is_default_off_and_unknown_destinations_are_explicitly_unsupported() {
    let files = Files::new();
    let (host, _) = host(&files);
    let auth = authenticate(&host);
    assert_reply(
        &host,
        "ide.work.submit",
        submit(),
        &auth,
        503,
        failure("unavailable"),
    );
    for destination in ["ide.work.unknown", "ide.work.execute_shell"] {
        assert!(
            exchange(&host, post(&host, destination, common("work_unknown"), ""))
                .starts_with("HTTP/1.1 401 ")
        );
        assert_reply(
            &host,
            destination,
            common("work_unknown"),
            &auth,
            404,
            failure("not_found"),
        );
    }
}

#[test]
fn work_configuration_is_typed_effect_free_and_independent_of_node_read_grants() {
    let files = Files::new();
    let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
    let value = settings(&files, &upstream);
    let mut host_config = config(&files);
    for invalid in [
        WorkConfig {
            allowed_projects: vec![],
            ..value.clone()
        },
        WorkConfig {
            allowed_projects: vec!["bad/path".into()],
            ..value.clone()
        },
        WorkConfig {
            allowed_profiles: vec!["same".into(), "same".into()],
            ..value.clone()
        },
        WorkConfig {
            allowed_modes: vec![WorkMode::Plan, WorkMode::Plan],
            ..value.clone()
        },
        WorkConfig {
            endpoint: "192.0.2.1:18081".parse().unwrap(),
            ..value.clone()
        },
        WorkConfig {
            token_file: "relative.token".into(),
            ..value.clone()
        },
    ] {
        host_config.work = Some(invalid);
        assert!(register_host("ui", host_config.clone()).is_err());
    }
    host_config.work = Some(value.clone());
    fs::remove_file(&value.token_file).unwrap();
    let group = register_host("ui", host_config).unwrap();
    let (_, factory) = Registry::new()
        .register(group)
        .unwrap()
        .freeze()
        .unwrap()
        .into_factories();
    // Creating the configured dependency, rather than registration/freeze, reads the token.
    let ports = BTreeMap::from([(
        "lab".into(),
        Box::new(Fake(Rc::new(Cell::new(0)))) as Box<dyn InspectionPort>,
    )]);
    assert!(matches!(
        factory.create_with_ports(ports),
        Err(HostError::Configuration("invalid chat token file"))
    ));
    no_calls(&upstream);
}

#[test]
fn work_denies_grants_and_malformed_dtos_before_upstream_effects() {
    let files = Files::new();
    let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
    let host = configured(&files, &upstream);
    let auth = authenticate(&host);
    for (field, value) in [
        ("project_alias", json!("workspace")),
        ("mode", json!("review")),
        ("agent_profile", json!("other-profile")),
    ] {
        let mut body = submit();
        body[field] = value;
        assert_reply(
            &host,
            "ide.work.submit",
            body,
            &auth,
            403,
            failure("denied"),
        );
    }
    let mut cancel = task_request("work_cancel");
    cancel["expected_revision"] = json!(1);
    cancel["idempotency_key"] = json!("cancel-foreign");
    for (destination, mut body) in [
        ("ide.work.get", task_request("work_get")),
        ("ide.work.list", common("work_list")),
        ("ide.work.events", task_request("work_events")),
        ("ide.work.cancel", cancel),
    ] {
        body["project_alias"] = json!("workspace");
        assert_reply(&host, destination, body, &auth, 403, failure("denied"));
    }
    for (field, value) in [
        ("owner", json!("someone")),
        ("base_revision", json!("A".repeat(40))),
        ("mode", json!("execute")),
        ("prompt", json!("x".repeat(8193))),
        ("prompt", json!("🛠".repeat(2049))),
        ("prompt", json!("bad\u{0080}")),
        ("acceptance_criteria", json!("x".repeat(2049))),
        ("acceptance_criteria", json!(" \n\t")),
        ("idempotency_key", json!("bad/key")),
        ("request_id", json!("mismatch")),
    ] {
        let mut body = submit();
        body[field] = value;
        assert_reply(
            &host,
            "ide.work.submit",
            body,
            &auth,
            400,
            failure("invalid_request"),
        );
    }
    for (destination, mut body, field, value) in [
        (
            "ide.work.get",
            task_request("work_get"),
            "task_id",
            json!(TASK.to_uppercase().replace('0', "A")),
        ),
        ("ide.work.list", common("work_list"), "limit", json!(true)),
        ("ide.work.list", common("work_list"), "limit", json!(51)),
        (
            "ide.work.list",
            common("work_list"),
            "cursor",
            json!("opaque"),
        ),
        (
            "ide.work.events",
            task_request("work_events"),
            "after_sequence",
            json!(9_007_199_254_740_992_u64),
        ),
        (
            "ide.work.events",
            task_request("work_events"),
            "limit",
            json!(0),
        ),
        (
            "ide.work.cancel",
            task_request("work_cancel"),
            "expected_revision",
            json!(true),
        ),
    ] {
        body[field] = value;
        assert_reply(
            &host,
            destination,
            body,
            &auth,
            400,
            failure("invalid_request"),
        );
    }
    let duplicate = format!("{{\"kind\":\"work_get\",\"request_id\":\"request-1\",\"project_alias\":\"{PROJECT}\",\"task_id\":\"{TASK}\",\"task_id\":\"{OTHER}\"}}");
    let mut raw = post(&host, "ide.work.get", json!({}), &auth);
    raw.truncate(raw.find("\r\n\r\n").unwrap() + 4);
    raw = raw.replace(
        "Content-Length: 2",
        &format!("Content-Length: {}", duplicate.len()),
    );
    raw.push_str(&duplicate);
    assert!(exchange(&host, raw).starts_with("HTTP/1.1 400 "));
    for headers in [
        format!("{auth}Forge-Request-Id: request-1\r\n"),
        format!("{auth}Forge-Destination: ide.work.submit\r\n"),
        format!("{auth}Forge-Owner: caller\r\n"),
    ] {
        assert!(
            exchange(&host, post(&host, "ide.work.submit", submit(), &headers))
                .starts_with("HTTP/1.1 400 ")
        );
    }
    let bad_csrf = auth.replace("Forge-CSRF: ", "Forge-CSRF: invalid");
    assert!(
        exchange(&host, post(&host, "ide.work.submit", submit(), &bad_csrf))
            .starts_with("HTTP/1.1 401 ")
    );
    let missing_csrf = format!("{}\r\n", auth.split_once("\r\n").unwrap().0);
    assert!(exchange(
        &host,
        post(&host, "ide.work.submit", submit(), &missing_csrf)
    )
    .starts_with("HTTP/1.1 401 "));
    let mut escaped = submit();
    escaped["prompt"] = json!("\"".repeat(8192));
    let mut oversized = post(&host, "ide.work.submit", escaped, &auth);
    // Admission must reject the declared framing before waiting for its body.
    let declared = oversized.split_once("\r\n\r\n").unwrap().1.len();
    oversized = oversized.replace(
        &format!("Content-Length: {declared}"),
        &format!(
            "Content-Length: {}",
            HostLimits::default().request_bytes + 1
        ),
    );
    oversized.truncate(oversized.find("\r\n\r\n").unwrap() + 4);
    assert!(exchange(&host, oversized).starts_with("HTTP/1.1 413 "));
    for destination in ["ide.work.result", "ide.work.resume", "ide.work.accept"] {
        assert_reply(
            &host,
            destination,
            common("work_mismatched"),
            &auth,
            400,
            failure("invalid_request"),
        );
    }
    assert!(
        exchange(&host, post(&host, "ide.work.submit", submit(), "")).starts_with("HTTP/1.1 401 ")
    );
    no_calls(&upstream);
}

#[test]
fn work_selected_singleton_relays_all_five_operations_and_immutable_retry_receipts() {
    let files = Files::new();
    let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
    let host = configured(&files, &upstream);
    let auth = authenticate(&host);
    // A ready Singleton retains the chosen native credential after construction.
    fs::remove_file(files.0.join("work.token")).unwrap();
    let list = reply(
        "work_listed",
        json!({"project_alias":PROJECT,"tasks":[summary()],"next_cursor":TASK}),
    );
    let events = reply(
        "work_events",
        json!({"project_alias":PROJECT,"task_id":TASK,"status":"queued",
        "revision":1,"events":[{"event_id":"00000000-0000-4000-8000-000000000001","correlation_id":OTHER,"sequence":1,"kind":"queued","created_at":STAMP}],"next_sequence":1}),
    );
    let cancel = reply(
        "work_cancel_recorded",
        json!({"project_alias":PROJECT,"task_id":TASK,
        "base_revision":1,"revision":2,"status":"cancelled"}),
    );
    let mut cancelled = opened();
    cancelled["event"]["task"]["status"] = json!("cancelled");
    cancelled["event"]["task"]["revision"] = json!(2);
    let mut retry = receipt();
    retry["event"]["request_id"] = json!("retry-2");
    let mut cancel_request = task_request("work_cancel");
    cancel_request["expected_revision"] = json!(1);
    cancel_request["idempotency_key"] = json!("cancel-1");
    let cases = vec![
        ("ide.work.submit", submit(), 202, receipt()),
        ("ide.work.get", task_request("work_get"), 200, opened()),
        ("ide.work.list", common("work_list"), 200, list),
        ("ide.work.events", task_request("work_events"), 200, events),
        ("ide.work.cancel", cancel_request, 200, cancel),
        ("ide.work.get", task_request("work_get"), 200, cancelled),
    ];
    let worker = backend(
        upstream,
        cases
            .iter()
            .map(|(_, _, status, reply)| (*status, reply.clone()))
            .chain(std::iter::once((202, retry.clone())))
            .collect(),
    );
    for (destination, body, status, response) in &cases {
        assert_reply(
            &host,
            destination,
            body.clone(),
            &auth,
            *status,
            response.clone(),
        );
    }
    let mut body = submit();
    body["request_id"] = json!("retry-2");
    let request = post(&host, "ide.work.submit", body.clone(), &auth)
        .replace("Forge-Request-Id: request-1", "Forge-Request-Id: retry-2");
    let response = exchange(&host, request);
    assert!(response.starts_with("HTTP/1.1 202 "));
    assert_eq!(
        serde_json::from_str::<Value>(response.split_once("\r\n\r\n").unwrap().1).unwrap(),
        retry
    );
    let received = worker.join().unwrap();
    let expected: Vec<_> = cases
        .into_iter()
        .map(|(_, body, _, _)| body)
        .chain(std::iter::once(body))
        .collect();
    assert_eq!(received, expected);
}

#[test]
fn work_validates_typed_failure_codes_against_the_upstream_http_status() {
    let files = Files::new();
    let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
    let host = configured(&files, &upstream);
    let auth = authenticate(&host);
    let failures = [
        (400, "invalid_request"),
        (403, "denied"),
        (404, "not_found"),
        (409, "idempotency_conflict"),
        (409, "revision_conflict"),
        (409, "not_cancellable"),
        (503, "queue_full"),
        (503, "unavailable"),
    ];
    let worker = backend(
        upstream,
        failures
            .iter()
            .map(|(status, code)| (*status, failure(code)))
            .chain([
                (200, failure("not_found")),
                (418, failure("unavailable")),
                (503, reply("work_failed", json!({"code":"internal_secret"}))),
            ])
            .collect(),
    );
    for (status, code) in failures {
        assert_reply(
            &host,
            "ide.work.get",
            task_request("work_get"),
            &auth,
            status,
            failure(code),
        );
    }
    for _ in 0..3 {
        assert_reply(
            &host,
            "ide.work.get",
            task_request("work_get"),
            &auth,
            503,
            failure("unavailable"),
        );
    }
    worker.join().unwrap();
}

#[test]
fn work_rejects_wrong_identity_scope_digest_status_and_unrecognized_response_fields() {
    let files = Files::new();
    let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
    let host = configured(&files, &upstream);
    let auth = authenticate(&host);
    let mut invalid = Vec::new();
    for (key, value) in [
        ("task_id", json!(OTHER)),
        ("project_alias", json!("foreign")),
        ("recipe_digest", json!("b".repeat(63))),
        ("base_revision", json!("A".repeat(40))),
        ("status", json!("unrecognized")),
        ("revision", json!(0)),
        ("revision", json!(9_007_199_254_740_992_u64)),
        ("mode", json!("execute")),
        ("created_at", json!("2026-02-30T12:00:00Z")),
        ("created_at", json!("2026-11-19T12:00:00+02:00")),
        ("owner_id", json!("private")),
    ] {
        let mut value_reply = opened();
        value_reply["event"]["task"][key] = value;
        invalid.push(value_reply);
    }
    for (key, value) in [
        ("request_id", json!("other")),
        ("kind", json!("work_submitted")),
        ("debug", json!("private")),
    ] {
        let mut value_reply = opened();
        value_reply["event"][key] = value;
        invalid.push(value_reply);
    }
    let mut version = opened();
    version["protocol_version"] = json!(2);
    invalid.push(version);
    let mut extra = opened();
    extra["credential"] = json!("private");
    invalid.push(extra);
    let count = invalid.len();
    let worker = backend(
        upstream,
        invalid.into_iter().map(|value| (200, value)).collect(),
    );
    for _ in 0..count {
        assert_reply(
            &host,
            "ide.work.get",
            task_request("work_get"),
            &auth,
            503,
            failure("unavailable"),
        );
    }
    worker.join().unwrap();
}

#[test]
fn work_checks_event_order_cursors_filters_and_receipt_revisions() {
    let files = Files::new();
    let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
    let host = configured(&files, &upstream);
    let auth = authenticate(&host);
    let mut events = reply(
        "work_events",
        json!({"project_alias":PROJECT,"task_id":TASK,"revision":2,
        "status":"cancelled","events":[{"event_id":"00000000-0000-4000-8000-000000000001","correlation_id":OTHER,"sequence":1,"kind":"queued","created_at":STAMP},
        {"event_id":"00000000-0000-4000-8000-000000000002","correlation_id":OTHER,"sequence":2,"kind":"cancelled","created_at":STAMP}],"next_sequence":2}),
    );
    let mut cases = Vec::new();
    for kind in [
        "reverse",
        "duplicate",
        "duplicate_event_id",
        "invalid_event_id",
        "missing_correlation",
        "zero",
        "cursor",
        "scope",
        "timestamp",
        "kind",
    ] {
        let mut bad = events.clone();
        match kind {
            "reverse" => bad["event"]["events"].as_array_mut().unwrap().reverse(),
            "duplicate" => bad["event"]["events"][1]["sequence"] = json!(1),
            "duplicate_event_id" => {
                bad["event"]["events"][1]["event_id"] =
                    bad["event"]["events"][0]["event_id"].clone()
            }
            "invalid_event_id" => bad["event"]["events"][0]["event_id"] = json!("not-a-uuid"),
            "missing_correlation" => {
                bad["event"]["events"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("correlation_id");
            }
            "zero" => bad["event"]["events"][0]["sequence"] = json!(0),
            "cursor" => bad["event"]["next_sequence"] = json!(1),
            "scope" => bad["event"]["project_alias"] = json!("foreign"),
            "timestamp" => bad["event"]["events"][0]["created_at"] = json!("2026-11-19"),
            _ => bad["event"]["events"][0]["kind"] = json!("tool_output"),
        }
        cases.push(("ide.work.events", task_request("work_events"), bad));
    }
    let mut after = task_request("work_events");
    after["after_sequence"] = json!(1);
    cases.push(("ide.work.events", after.clone(), events.clone()));
    let mut limited = task_request("work_events");
    limited["limit"] = json!(1);
    cases.push(("ide.work.events", limited, events.clone()));
    let listed = reply(
        "work_listed",
        json!({"project_alias":PROJECT,"tasks":[summary()],"next_cursor":null}),
    );
    for kind in [
        "cursor",
        "empty",
        "duplicate",
        "missing",
        "scope",
        "secret",
        "reverse",
    ] {
        let mut bad = listed.clone();
        match kind {
            "cursor" => bad["event"]["next_cursor"] = json!(OTHER),
            "empty" => {
                bad["event"]["tasks"] = json!([]);
                bad["event"]["next_cursor"] = json!(TASK);
            }
            "duplicate" => bad["event"]["tasks"] = json!([summary(), summary()]),
            "missing" => {
                bad["event"].as_object_mut().unwrap().remove("next_cursor");
            }
            "scope" => bad["event"]["tasks"][0]["project_alias"] = json!("foreign"),
            "secret" => bad["event"]["tasks"][0]["prompt"] = json!("private"),
            _ => {
                let mut second = summary();
                second["task_id"] = json!(OTHER);
                bad["event"]["tasks"] = json!([second, summary()]);
            }
        }
        cases.push(("ide.work.list", common("work_list"), bad));
    }
    let mut filtered = common("work_list");
    filtered["status_filter"] = json!("cancelled");
    cases.push(("ide.work.list", filtered, listed.clone()));
    let mut limited = common("work_list");
    limited["limit"] = json!(1);
    let mut two = listed;
    let mut second = summary();
    second["task_id"] = json!(OTHER);
    two["event"]["tasks"] = json!([summary(), second]);
    cases.push(("ide.work.list", limited, two));
    let worker = backend(
        upstream,
        cases
            .iter()
            .map(|(_, _, bad)| (200, bad.clone()))
            .chain(std::iter::once((200, {
                events["event"]["events"].as_array_mut().unwrap().remove(0);
                events
            })))
            .collect(),
    );
    for (destination, body, _) in cases {
        assert_reply(&host, destination, body, &auth, 503, failure("unavailable"));
    }
    let accepted = reply(
        "work_events",
        json!({"project_alias":PROJECT,"task_id":TASK,"revision":2,
        "status":"cancelled","events":[{"event_id":"00000000-0000-4000-8000-000000000002","correlation_id":OTHER,"sequence":2,"kind":"cancelled","created_at":STAMP}],"next_sequence":2}),
    );
    assert_reply(&host, "ide.work.events", after, &auth, 200, accepted);
    worker.join().unwrap();
}

#[test]
fn work_checks_submit_cancel_receipts_and_boundary_pages() {
    let files = Files::new();
    let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
    let host = configured(&files, &upstream);
    let auth = authenticate(&host);
    let mut cases = Vec::new();
    for (key, value) in [
        ("revision", json!(2)),
        ("status", json!("cancelled")),
        ("project_alias", json!("foreign")),
        ("task_id", json!("BAD")),
        ("recipe_digest", json!(format!("sha256:{}", "b".repeat(64)))),
    ] {
        let mut bad = receipt();
        bad["event"][key] = value;
        cases.push((
            "ide.work.submit",
            submit(),
            202,
            bad,
            503,
            failure("unavailable"),
        ));
    }
    let mut cancel_request = task_request("work_cancel");
    cancel_request["expected_revision"] = json!(1);
    cancel_request["idempotency_key"] = json!("cancel-1");
    let cancelled = reply(
        "work_cancel_recorded",
        json!({"project_alias":PROJECT,"task_id":TASK,
        "base_revision":1,"revision":2,"status":"cancelled"}),
    );
    for (key, value) in [
        ("base_revision", json!(2)),
        ("revision", json!(1)),
        ("revision", json!(9_007_199_254_740_992_u64)),
        ("status", json!("queued")),
        ("project_alias", json!("foreign")),
        ("task_id", json!(OTHER)),
    ] {
        let mut bad = cancelled.clone();
        bad["event"][key] = value;
        cases.push((
            "ide.work.cancel",
            cancel_request.clone(),
            200,
            bad,
            503,
            failure("unavailable"),
        ));
    }
    let mut maximum = submit();
    maximum["prompt"] = json!("🛠".repeat(2048));
    maximum["acceptance_criteria"] = json!("é".repeat(1024));
    cases.push(("ide.work.submit", maximum, 202, receipt(), 202, receipt()));
    cases.push((
        "ide.work.cancel",
        cancel_request,
        200,
        cancelled.clone(),
        200,
        cancelled,
    ));
    let mut list_request = common("work_list");
    list_request["status_filter"] = Value::Null;
    list_request["cursor"] = Value::Null;
    let empty = reply(
        "work_listed",
        json!({"project_alias":PROJECT,"tasks":[],"next_cursor":null}),
    );
    cases.push((
        "ide.work.list",
        list_request,
        200,
        empty.clone(),
        200,
        empty,
    ));
    let mut after = task_request("work_events");
    after["after_sequence"] = json!(10);
    let empty = reply(
        "work_events",
        json!({"project_alias":PROJECT,"task_id":TASK,"status":"queued",
        "revision":1,"events":[],"next_sequence":10}),
    );
    cases.push(("ide.work.events", after, 200, empty.clone(), 200, empty));
    let mut first = summary();
    first["created_at"] = json!("2026-11-19T17:00:00+00:00");
    let mut second = summary();
    second["task_id"] = json!(OTHER);
    second["created_at"] = json!("2026-11-19T17:00:00.100000Z");
    let ordered = reply(
        "work_listed",
        json!({"project_alias":PROJECT,"tasks":[first,second],"next_cursor":null}),
    );
    cases.push((
        "ide.work.list",
        common("work_list"),
        200,
        ordered.clone(),
        200,
        ordered,
    ));
    let worker = backend(
        upstream,
        cases
            .iter()
            .map(|(_, _, status, body, _, _)| (*status, body.clone()))
            .collect(),
    );
    for (destination, body, _, _, status, expected) in cases {
        assert_reply(&host, destination, body, &auth, status, expected);
    }
    worker.join().unwrap();
}

#[test]
fn work_catalog_intersects_independent_grants_and_accepts_execution_event_sequences() {
    let files = Files::new();
    let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
    let host = configured(&files, &upstream);
    let auth = authenticate(&host);
    let catalog = reply(
        "work_catalog",
        json!({
            "projects":[{"alias":PROJECT,"label":"Coding","base_revision":"a".repeat(40)},
                        {"alias":"foreign","label":"Other","base_revision":null}],
            "profiles":[{"alias":"agy-plan-v1","label":"Gemini","backend":"antigravity","model":"fixture-model","modes":["plan","review","implement"]},
                        {"alias":"foreign","label":"Hidden","backend":"codex","model":"other","modes":["plan"]}],
            "worker_status":"ready","observed_at":STAMP
        }),
    );
    let mut expected = catalog.clone();
    expected["event"]["projects"]
        .as_array_mut()
        .unwrap()
        .truncate(1);
    expected["event"]["profiles"]
        .as_array_mut()
        .unwrap()
        .truncate(1);
    expected["event"]["profiles"][0]["modes"] = json!(["plan"]);
    let mut running = opened();
    running["event"]["task"]["status"] = json!("running");
    running["event"]["task"]["revision"] = json!(8);
    let events = reply(
        "work_events",
        json!({"project_alias":PROJECT,"task_id":TASK,"revision":8,"status":"running",
        "events":[{"event_id":"00000000-0000-4000-8000-000000000012","correlation_id":OTHER,"sequence":12,"kind":"checkpoint","created_at":STAMP}],"next_sequence":12}),
    );
    let worker = backend(
        upstream,
        vec![
            (200, catalog),
            (200, running.clone()),
            (200, events.clone()),
        ],
    );
    assert_reply(
        &host,
        "ide.work.catalog",
        json!({"kind":"work_catalog","request_id":"request-1"}),
        &auth,
        200,
        expected,
    );
    assert_reply(
        &host,
        "ide.work.get",
        task_request("work_get"),
        &auth,
        200,
        running,
    );
    assert_reply(
        &host,
        "ide.work.events",
        task_request("work_events"),
        &auth,
        200,
        events,
    );
    assert_eq!(worker.join().unwrap().len(), 3);
}

#[test]
fn work_resume_rechecks_saved_mode_before_any_mutation() {
    let files = Files::new();
    let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
    let host = configured(&files, &upstream);
    let auth = authenticate(&host);
    let mut saved = opened();
    saved["event"]["task"]["mode"] = json!("implement");
    saved["event"]["task"]["status"] = json!("failed");
    saved["event"]["task"]["revision"] = json!(4);
    let worker = backend(upstream, vec![(200, saved)]);
    let mut request = task_request("work_resume");
    request["expected_revision"] = json!(4);
    request["idempotency_key"] = json!("resume-1");
    request["agent_profile"] = json!("agy-plan-v1");
    request["reconciliation_receipt_id"] = json!(OTHER);
    assert_reply(
        &host,
        "ide.work.resume",
        request,
        &auth,
        403,
        failure("denied"),
    );
    let calls = worker.join().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["kind"], "work_get");
}

fn usage_settings(files: &Files, upstream: &TcpListener) -> forge_ide_web_host::UsageConfig {
    let work = settings(files, upstream);
    forge_ide_web_host::UsageConfig {
        endpoint: work.endpoint,
        token_file: work.token_file,
        allowed_projects: work.allowed_projects,
        allowed_profiles: work.allowed_profiles,
    }
}
fn configured_usage(files: &Files, upstream: &TcpListener) -> Host {
    let mut value = config(files);
    value.usage = Some(usage_settings(files, upstream));
    assert!(value.work.is_none() && value.chat.is_none());
    host_with_config(value).0
}
fn usage_provider(alias: &str) -> Value {
    json!({"alias":alias,"label":"Codex","provider":"codex","model":"fixture-model",
        "worker_state":"idle","active_task_id":null,"snapshot_state":"unknown","observed_at":null,
        "source":"fixture-metadata","version":"1","last_error":null,"refresh_pending":false,
        "allowance":{"state":"unknown","reason":null,"ordinary_usage_allowed":null,"buckets":[]},
        "jobs":{"scope":"forge_project","input_tokens":"9007199254740993","output_tokens":"7","cached_input_tokens":null,
                "total_tokens":"9007199254741000","known_attempts":1,"unknown_attempts":0,"elapsed_ms":"1000"},
        "account_activity":{"state":"unknown","lifetime_tokens":null,"daily":[]}})
}
fn usage_failure(code: &str) -> Value {
    reply("usage_failed", json!({"code":code}))
}

#[test]
fn usage_selected_singleton_is_independent_and_filters_metadata_profiles() {
    let files = Files::new();
    let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
    let host = configured_usage(&files, &upstream);
    let auth = authenticate(&host);
    let overview = reply(
        "usage_overview",
        json!({"project_alias":PROJECT,"providers":[usage_provider("agy-plan-v1"),usage_provider("foreign")]}),
    );
    let expected = reply(
        "usage_overview",
        json!({"project_alias":PROJECT,"providers":[usage_provider("agy-plan-v1")]}),
    );
    let refresh = reply(
        "usage_refresh_queued",
        json!({"project_alias":PROJECT,"refresh_id":TASK,"profiles":["agy-plan-v1"],"coalesced":true}),
    );
    let worker = backend(upstream, vec![(200, overview), (202, refresh.clone())]);
    assert_reply(
        &host,
        "ide.usage.overview",
        common("usage_overview"),
        &auth,
        200,
        expected,
    );
    let mut request = common("usage_refresh");
    request["agent_profile"] = json!("agy-plan-v1");
    assert_reply(&host, "ide.usage.refresh", request, &auth, 202, refresh);
    let calls = worker.join().unwrap();
    assert_eq!(calls.len(), 2);
    assert!(calls
        .iter()
        .all(|c| c["kind"].as_str().unwrap().starts_with("usage_")));
}

#[test]
fn usage_grants_and_refresh_scope_are_checked_before_io() {
    let files = Files::new();
    let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
    let host = configured_usage(&files, &upstream);
    let auth = authenticate(&host);
    for kind in ["usage_overview", "usage_detail", "usage_refresh"] {
        let mut request = common(kind);
        request["project_alias"] = json!("foreign");
        if kind != "usage_overview" {
            request["agent_profile"] = json!("agy-plan-v1");
        }
        let dest = format!("ide.usage.{}", kind.strip_prefix("usage_").unwrap());
        assert_reply(&host, &dest, request, &auth, 403, usage_failure("denied"));
    }
    for alias in [json!(null), json!("foreign")] {
        let mut request = common("usage_refresh");
        request["agent_profile"] = alias;
        assert_reply(
            &host,
            "ide.usage.refresh",
            request,
            &auth,
            403,
            usage_failure("denied"),
        );
    }
    assert!(exchange(
        &host,
        post(&host, "ide.usage.overview", common("usage_overview"), "")
    )
    .starts_with("HTTP/1.1 401 "));
    no_calls(&upstream);
}

#[test]
fn agents_and_legacy_mail_entries_redirect_in_the_same_document_and_usage_is_default_off() {
    let files = Files::new();
    let (host, _) = host(&files);
    let auth = authenticate(&host);
    for path in ["/agents", "/agents/", "/mail", "/mail/"] {
        for method in ["GET", "HEAD"] {
            let response = exchange(
                &host,
                format!(
                    "{method} {path} HTTP/1.1\r\nHost: {}\r\n\r\n",
                    host.local_addr().unwrap()
                ),
            );
            assert!(response.starts_with("HTTP/1.1 302 "));
            assert!(response.contains("Location: /#agents\r\n"));
            assert!(response.ends_with("\r\n\r\n"));
        }
        let denied = exchange(
            &host,
            format!(
                "POST {path} HTTP/1.1\r\nHost: {}\r\nContent-Length: 0\r\n\r\n",
                host.local_addr().unwrap()
            ),
        );
        assert!(denied.starts_with("HTTP/1.1 405 "));
        assert!(denied.contains("Allow: GET, HEAD\r\n"));
        assert!(!denied.contains("Location:"));
    }
    assert_reply(
        &host,
        "ide.usage.overview",
        common("usage_overview"),
        &auth,
        503,
        usage_failure("unavailable"),
    );
    assert_reply(
        &host,
        "ide.usage.unknown",
        common("usage_unknown"),
        &auth,
        404,
        usage_failure("not_found"),
    );
}

fn plan_decision(decision: &str) -> Value {
    let mut request = task_request("work_plan_decide");
    for (key,value) in json!({"expected_revision":4,"plan_attempt_id":OTHER,"plan_sha256":"c".repeat(64),
        "decision":decision,"agent_profile":"agy-plan-v1","idempotency_key":format!("plan-{decision}"),"note":"Include failure-path tests."}).as_object().unwrap() {
        request[key]=value.clone();
    }
    request
}
fn plan_decided(decision: &str) -> Value {
    reply(
        "work_plan_decided",
        json!({"project_alias":PROJECT,"task_id":TASK,"base_revision":4,"revision":5,
        "status":if decision=="cancel" {"cancelled"} else {"queued"},
        "mode":if decision=="resume" {"implement"} else {"plan"}}),
    )
}
#[test]
fn plan_decision_admission_requires_target_mode_but_cancel_needs_no_profile_execution_grant() {
    let files = Files::new();
    let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
    let host = configured(&files, &upstream);
    let auth = authenticate(&host);
    assert_reply(
        &host,
        "ide.work.plan.decide",
        plan_decision("resume"),
        &auth,
        403,
        failure("denied"),
    );
    no_calls(&upstream);
    let mut invalid = plan_decision("revise");
    invalid["note"] = json!(" ");
    assert_reply(
        &host,
        "ide.work.plan.decide",
        invalid,
        &auth,
        400,
        failure("invalid_request"),
    );
    no_calls(&upstream);
    let mut cancel = plan_decision("cancel");
    cancel["agent_profile"] = json!("removed-profile");
    let worker = backend(upstream, vec![(200, plan_decided("cancel"))]);
    assert_reply(
        &host,
        "ide.work.plan.decide",
        cancel,
        &auth,
        200,
        plan_decided("cancel"),
    );
    let calls = worker.join().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["agent_profile"], "removed-profile");
}
#[test]
fn plan_decisions_keep_the_exact_intent_and_replay_without_a_stale_saved_plan_precheck() {
    let files = Files::new();
    let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut value = config(&files);
    let mut grants = settings(&files, &upstream);
    grants.allowed_modes.push(WorkMode::Implement);
    value.work = Some(grants);
    let host = host_with_config(value).0;
    let auth = authenticate(&host);
    let responses = vec![
        (202, plan_decided("resume")),
        (202, plan_decided("resume")),
        (202, plan_decided("revise")),
    ];
    let worker = backend(upstream, responses);
    for decision in ["resume", "resume", "revise"] {
        assert_reply(
            &host,
            "ide.work.plan.decide",
            plan_decision(decision),
            &auth,
            202,
            plan_decided(decision),
        );
    }
    let calls = worker.join().unwrap();
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0], calls[1]);
    assert!(calls.iter().all(|r| r["kind"] == "work_plan_decide"));
    assert_eq!(calls[0]["plan_sha256"], "c".repeat(64));
}
#[test]
fn work_publication_projection_rejects_an_arbitrary_href_before_returning_it_to_the_browser() {
    let files = Files::new();
    let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
    let host = configured(&files, &upstream);
    let auth = authenticate(&host);
    let mut safe = opened();
    safe["event"]["task"]["publication"] = json!({"status":"published",
        "pr_url":"https://github.com/roachmaster/homelab/pull/42","commit_sha":"e".repeat(40),"branch":"forge/task-fixture","summary":"Checks passed."});
    let mut unsafe_reply = safe.clone();
    unsafe_reply["event"]["task"]["publication"]["pr_url"] = json!("javascript:alert(1)");
    let worker = backend(upstream, vec![(200, unsafe_reply), (200, safe.clone())]);
    assert_reply(
        &host,
        "ide.work.get",
        task_request("work_get"),
        &auth,
        503,
        failure("unavailable"),
    );
    assert_reply(
        &host,
        "ide.work.get",
        task_request("work_get"),
        &auth,
        200,
        safe,
    );
    assert_eq!(worker.join().unwrap().len(), 2);
}

fn review_decision(decision: &str) -> Value {
    let mut value = task_request("work_review_decide");
    value["expected_revision"] = json!(4);
    value["review_id"] = json!(OTHER);
    value["decision"] = json!(decision);
    value["agent_profile"] = json!("agy-plan-v1");
    value["note"] = json!("Owner-reviewed compiler evidence");
    value["idempotency_key"] = json!("review-intent");
    value
}

fn verification_retry_receipt() -> Value {
    reply(
        "work_review_decided",
        json!({"project_alias":PROJECT,"task_id":TASK,"base_revision":4,"revision":5,
            "status":"awaiting_review","review_id":"90b8e250-6824-4ef9-af75-078389e71960"}),
    )
}

#[test]
fn review_infrastructure_retry_relays_new_review_once_through_authenticated_gateway() {
    let files = Files::new();
    let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
    let host = configured(&files, &upstream);
    let auth = authenticate(&host);
    let mut request = review_decision("retry_infrastructure");
    request["findings"] = json!([]);
    request["verified_finding_ids"] = json!([]);
    let receipt = verification_retry_receipt();
    let worker = backend(upstream, vec![(202, receipt.clone())]);
    assert_reply(
        &host,
        "ide.work.review.decide",
        request.clone(),
        &auth,
        202,
        receipt,
    );
    assert_eq!(worker.join().unwrap(), vec![request]);
}

#[test]
fn review_infrastructure_retry_rejects_wrong_scope_revision_identity_and_repair_receipts() {
    let files = Files::new();
    let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
    let host = configured(&files, &upstream);
    let auth = authenticate(&host);
    let request = review_decision("retry_infrastructure");
    let invalid: Vec<_> = [
        ("project_alias", json!("foreign")),
        ("task_id", json!(OTHER)),
        ("base_revision", json!(3)),
        ("revision", json!(4)),
        ("revision", json!(6)),
        ("review_id", json!(OTHER)),
        ("review_id", json!("not-a-uuid")),
        ("review_id", json!("90B8E250-6824-4EF9-AF75-078389E71960")),
        ("review_id", Value::Null),
        (
            "repair_task_id",
            json!("90b8e250-6824-4ef9-af75-078389e71960"),
        ),
        ("status", json!("running")),
        ("status", json!("completed")),
    ]
    .into_iter()
    .map(|(key, value)| {
        let mut receipt = verification_retry_receipt();
        receipt["event"][key] = value;
        (202, receipt)
    })
    .collect();
    let count = invalid.len();
    let worker = backend(upstream, invalid);
    for _ in 0..count {
        assert_reply(
            &host,
            "ide.work.review.decide",
            request.clone(),
            &auth,
            503,
            failure("unavailable"),
        );
    }
    assert_eq!(worker.join().unwrap(), vec![request; count]);
}

#[test]
fn nonexecution_review_controls_survive_removed_profile_and_implementation_grant() {
    let files = Files::new();
    let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
    let host = configured(&files, &upstream);
    let auth = authenticate(&host);
    let mut repair = review_decision("repair");
    repair["agent_profile"] = json!("removed-profile");
    assert_reply(
        &host,
        "ide.work.review.decide",
        repair,
        &auth,
        403,
        failure("denied"),
    );
    no_calls(&upstream);
    let mut foreign = review_decision("approve");
    foreign["project_alias"] = json!("ungranted-project");
    assert_reply(
        &host,
        "ide.work.review.decide",
        foreign,
        &auth,
        403,
        failure("denied"),
    );
    no_calls(&upstream);
    let receipt = reply(
        "work_review_decided",
        json!({"project_alias":PROJECT,"task_id":TASK,"base_revision":4,"revision":5,"status":"awaiting_review","review_id":OTHER}),
    );
    let retried = verification_retry_receipt();
    let publication = reply(
        "work_publication_retry_queued",
        json!({"project_alias":PROJECT,"task_id":TASK,"base_revision":4,"revision":5,"status":"awaiting_review","publication_id":OTHER}),
    );
    let worker = backend(
        upstream,
        vec![
            (200, receipt.clone()),
            (200, retried.clone()),
            (200, receipt.clone()),
            (202, publication.clone()),
        ],
    );
    for decision in ["approve", "retry_infrastructure", "escalate"] {
        let mut request = review_decision(decision);
        request["agent_profile"] = json!("removed-profile");
        assert_reply(
            &host,
            "ide.work.review.decide",
            request,
            &auth,
            200,
            if decision == "retry_infrastructure" {
                retried.clone()
            } else {
                receipt.clone()
            },
        );
    }
    let mut retry = task_request("work_publication_retry");
    for (key,value) in json!({"expected_revision":4,"publication_id":OTHER,"candidate_commit":"e".repeat(40),"note":"Retry the same artifact","idempotency_key":"publication-retry"}).as_object().unwrap() { retry[key]=value.clone(); }
    assert_reply(
        &host,
        "ide.work.publication.retry",
        retry,
        &auth,
        202,
        publication,
    );
    let calls = worker.join().unwrap();
    assert_eq!(calls.len(), 4);
    assert!(
        calls.iter().all(|call| call["kind"] != "work_get"),
        "nonexecution controls do not perform a model-grant precheck"
    );
}
#[test]
fn review_repair_rechecks_the_saved_implementation_mode_before_mutation() {
    let files = Files::new();
    let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut value = config(&files);
    let mut grants = settings(&files, &upstream);
    grants.allowed_modes.push(WorkMode::Implement);
    value.work = Some(grants);
    let host = host_with_config(value).0;
    let auth = authenticate(&host);
    let worker = backend(upstream, vec![(200, opened())]);
    assert_reply(
        &host,
        "ide.work.review.decide",
        review_decision("repair"),
        &auth,
        403,
        failure("denied"),
    );
    let calls = worker.join().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["kind"], "work_get");
}

#[test]
fn autonomous_submission_forwards_exact_policy_without_receipt_stranding_precheck() {
    let files = Files::new();
    let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
    let host = configured(&files, &upstream);
    let auth = authenticate(&host);
    let mut request = submit();
    request["workflow_policy_digest"] = json!("c".repeat(64));
    let worker = backend(upstream, vec![(202, receipt()), (202, receipt())]);
    for _ in 0..2 {
        assert_reply(
            &host,
            "ide.work.submit",
            request.clone(),
            &auth,
            202,
            receipt(),
        );
    }
    let calls = worker.join().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0], calls[1]);
    assert_eq!(calls[0]["workflow_policy_digest"], "c".repeat(64));
    assert!(calls.iter().all(|call| call["kind"] == "work_submit"));
}

#[test]
fn cicd_stage_log_admission_bounds_and_catalog_branch_filtering() {
    let files = Files::new();
    let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
    let host = configured(&files, &upstream);
    let auth = authenticate(&host);

    let corr = "99999999-9999-4999-8999-999999999999";
    let action = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let valid_req = json!({
        "kind": "work_cicd_stage_log",
        "request_id": "request-1",
        "project_alias": PROJECT,
        "task_id": TASK,
        "correlation_id": corr,
        "action_id": action,
        "stage_id": "unit-tests",
        "view": "console",
        "limit_bytes": 4096
    });
    let log_text = "running 3 tests\ntest result: ok. 3 passed; 0 failed; 0 ignored;";
    let valid_page = json!({
        "kind": "work_cicd_stage_log_page",
        "request_id": "request-1",
        "project_alias": PROJECT,
        "task_id": TASK,
        "correlation_id": corr,
        "action_id": action,
        "stage_id": "unit-tests",
        "view": "console",
        "content": log_text,
        "truncated": false,
        "total_bytes": log_text.len(),
        "byte_offset": 0,
        "next_cursor": null,
        "prev_cursor": null,
        "tests_passed": 3,
        "tests_failed": 0,
        "tests_skipped": 0,
        "test_cases": [],
        "error_excerpt": null
    });
    let catalog_raw = json!({
        "kind": "work_catalog",
        "request_id": "request-1",
        "projects": [
            {"alias": PROJECT, "label": "Workspace", "base_revision": "a".repeat(40)},
            {"alias": "other-project", "label": "Other", "base_revision": "a".repeat(40)}
        ],
        "profiles": [
            {"alias": "agy-plan-v1", "label": "AGY", "backend": "antigravity", "model": "gemini", "modes": ["plan", "review"], "branch": "forge/agent/agy-plan-v1"},
            {"alias": "other-profile", "label": "Other", "backend": "codex", "model": "gpt", "modes": ["plan"], "branch": "forge/agent/other-profile"}
        ],
        "worker_status": "ready",
        "observed_at": "2026-11-19T18:00:00Z",
        "agent_branches": [
            {
                "project_alias": PROJECT,
                "agent_profile": "agy-plan-v1",
                "branch_name": "forge/agent/agy-plan-v1",
                "branch_scope": "branch/forge-agent-agy-plan-v1-0123456789abcdef",
                "sync_status": "synced_with_main",
                "cache_status": "hit_branch",
                "reservation_status": "idle",
                "reservation_epoch": 1
            },
            {
                "project_alias": "other-project",
                "agent_profile": "agy-plan-v1",
                "branch_name": "forge/agent/agy-plan-v1",
                "branch_scope": "branch/forge-agent-agy-plan-v1-0123456789abcdef",
                "sync_status": "unknown",
                "cache_status": "unknown",
                "reservation_status": "idle",
                "reservation_epoch": 0
            },
            {
                "project_alias": PROJECT,
                "agent_profile": "other-profile",
                "branch_name": "forge/agent/other-profile",
                "branch_scope": "branch/forge-agent-other-profile-0123456789abcdef",
                "sync_status": "unknown",
                "cache_status": "unknown",
                "reservation_status": "idle",
                "reservation_epoch": 0
            }
        ]
    });
    let valid_page_env = json!({"protocol_version": 1, "event": valid_page});
    let catalog_env = json!({"protocol_version": 1, "event": catalog_raw});
    let worker = backend(
        upstream,
        vec![(200, valid_page_env.clone()), (200, catalog_env)],
    );

    // Oversized limit_bytes -> rejected before runner I/O
    let mut oversized = valid_req.clone();
    oversized["limit_bytes"] = json!(65_536);
    assert_reply(
        &host,
        "ide.work.cicd.stage_log",
        oversized,
        &auth,
        400,
        failure("invalid_request"),
    );
    // Unauthorized project -> rejected before runner I/O
    let mut denied_proj = valid_req.clone();
    denied_proj["project_alias"] = json!("other-project");
    assert_reply(
        &host,
        "ide.work.cicd.stage_log",
        denied_proj,
        &auth,
        403,
        failure("denied"),
    );

    // Valid stage log request -> forwarded and validated
    assert_reply(
        &host,
        "ide.work.cicd.stage_log",
        valid_req,
        &auth,
        200,
        valid_page_env,
    );

    // Catalog filters agent_branches to allowed_projects & allowed_profiles
    let cat_req = json!({"kind": "work_catalog", "request_id": "request-1"});
    let reply = exchange(&host, post(&host, "ide.work.catalog", cat_req, &auth));
    assert!(reply.starts_with("HTTP/1.1 200 "));
    let body: Value = serde_json::from_str(reply.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    let branches = body["event"]["agent_branches"].as_array().unwrap();
    assert_eq!(branches.len(), 1);
    assert_eq!(branches[0]["project_alias"], PROJECT);
    assert_eq!(branches[0]["agent_profile"], "agy-plan-v1");

    let calls = worker.join().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0]["kind"], "work_cicd_stage_log");
    assert_eq!(calls[1]["kind"], "work_catalog");
}

#[test]
fn mcp_presence_is_host_owned_optional_and_reader_failure_does_not_hide_catalog() {
    for enabled in [false, true] {
        let files = Files::new();
        let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
        let presence = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut value = config(&files);
        let mut work = settings(&files, &upstream);
        if enabled {
            work.mcp_presence = Some(forge_ide_web_host::McpPresenceConfig {
                endpoint: presence.local_addr().unwrap(),
                token_file: work.token_file.clone(),
            });
        }
        value.work = Some(work);
        let host = host_with_config(value).0;
        let auth = authenticate(&host);
        let catalog = reply(
            "work_catalog",
            json!({"projects":[],"profiles":[],"worker_status":"ready","observed_at":STAMP,
            "mcp_presence":{"status":"available","report":{"version":1,"agents":[{"agent_id":"forged","label":"Forged client","state":"online","last_seen_at":STAMP}]}}}),
        );
        let mut expected = catalog.clone();
        expected["event"]
            .as_object_mut()
            .unwrap()
            .remove("mcp_presence");
        if enabled {
            expected["event"]["mcp_presence"] = json!({"status":"unavailable"});
        }
        // A stopped selected MCP reader is an unavailable observation, not offline.
        drop(presence);
        let worker = backend(upstream, vec![(200, catalog)]);
        assert_reply(
            &host,
            "ide.work.catalog",
            json!({"kind":"work_catalog","request_id":"request-1"}),
            &auth,
            200,
            expected,
        );
        worker.join().unwrap();
    }
}

#[test]
fn mcp_presence_selected_reader_gets_only_dedicated_bearer_and_projects_typed_data() {
    let files = Files::new();
    let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
    let presence = TcpListener::bind("127.0.0.1:0").unwrap();
    let token_file = files.0.join("presence.token");
    fs::write(&token_file, "b".repeat(64)).unwrap();
    fs::set_permissions(&token_file, fs::Permissions::from_mode(0o600)).unwrap();
    let mut value = config(&files);
    let mut work = settings(&files, &upstream);
    work.mcp_presence = Some(forge_ide_web_host::McpPresenceConfig {
        endpoint: presence.local_addr().unwrap(),
        token_file,
    });
    value.work = Some(work);
    let host = host_with_config(value).0;
    let auth = authenticate(&host);
    let report = json!({"version":1,"agents":[{"agent_id":"dot","label":"Dot","state":"never","last_seen_at":null}]});
    let data = report.clone();
    let reader = thread::spawn(move || {
        let (mut socket, _) = presence.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut reader = BufReader::new(&mut socket);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert_eq!(line, "GET /internal/agent-presence HTTP/1.1\r\n");
        let mut headers = BTreeMap::new();
        loop {
            line.clear();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            let (name, value) = line.trim_end().split_once(": ").unwrap();
            headers.insert(name.to_ascii_lowercase(), value.to_owned());
        }
        assert_eq!(
            headers["authorization"],
            format!("Bearer {}", "b".repeat(64))
        );
        for forbidden in [
            "cookie",
            "origin",
            "cf-connecting-ip",
            "forge-target-node",
            "forge-destination",
        ] {
            assert!(!headers.contains_key(forbidden));
        }
        let body = serde_json::to_vec(&data).unwrap();
        write!(
            socket,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .unwrap();
        socket.write_all(&body).unwrap();
    });
    let catalog = reply(
        "work_catalog",
        json!({"projects":[],"profiles":[],"worker_status":"ready","observed_at":STAMP}),
    );
    let mut expected = catalog.clone();
    expected["event"]["mcp_presence"] = json!({"status":"available","report":report});
    let worker = backend(upstream, vec![(200, catalog)]);
    assert_reply(
        &host,
        "ide.work.catalog",
        json!({"kind":"work_catalog","request_id":"request-1"}),
        &auth,
        200,
        expected,
    );
    reader.join().unwrap();
    worker.join().unwrap();
}

#[test]
fn recorded_cancellation_attribution_is_typed_and_rejects_invented_causes() {
    let files = Files::new();
    let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
    let host = configured(&files, &upstream);
    let auth = authenticate(&host);
    let events = reply(
        "work_events",
        json!({
            "project_alias":PROJECT,"task_id":TASK,"revision":2,"status":"cancelled",
            "events":[{"event_id":TASK,"correlation_id":OTHER,"sequence":1,
                "kind":"cancelled","created_at":STAMP,
                "attribution":{"actor":{"kind":"worker","id":"grok-primary"},
                    "cause":"workflow_cancelled","detail":"Recorded stop <safe text>."}}],
            "next_sequence":1
        }),
    );
    let mut invalid = events.clone();
    invalid["event"]["events"][0]["attribution"]["cause"] = json!("invented");
    let worker = backend(upstream, vec![(200, events.clone()), (200, invalid)]);
    assert_reply(
        &host,
        "ide.work.events",
        task_request("work_events"),
        &auth,
        200,
        events,
    );
    assert_reply(
        &host,
        "ide.work.events",
        task_request("work_events"),
        &auth,
        503,
        failure("unavailable"),
    );
    worker.join().unwrap();
}

#[test]
fn defect_submission_and_filtered_portal_projection_use_the_normal_relay() {
    let files = Files::new();
    let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
    let host = configured(&files, &upstream);
    let auth = authenticate(&host);
    let mut metadata = json!({"summary":"Opening fails", "location":{"repo_path":"src/editor.rs","function":"open","commit":"a".repeat(40)},
        "observed_behavior":"Document stays closed","expected_behavior":"Document opens", "severity":"high", "evidence":"Failing regression"});
    metadata["evidence"] = json!("e".repeat(8192));
    let mut task = summary();
    task["task_kind"] = json!("defect");
    task["defect"] = metadata.clone();
    let response = reply(
        "work_listed",
        json!({"project_alias":PROJECT,"tasks":[task],"next_cursor":null}),
    );
    let worker = backend(
        upstream,
        vec![
            (202, receipt()),
            (200, response.clone()),
            (200, response.clone()),
        ],
    );
    let mut request = submit();
    request["task_kind"] = json!("defect");
    request["defect"] = metadata;
    request["prompt"] = json!("p".repeat(8192));
    assert!(serde_json::to_vec(&request).unwrap().len() > 16_384);
    let mut malformed = request.clone();
    malformed["defect"]["severity"] = json!("urgent");
    assert_reply(
        &host,
        "ide.work.submit",
        malformed,
        &auth,
        400,
        reply(
            "work_failed",
            json!({"code":"invalid_request", "reason":"defect.fields"}),
        ),
    );
    assert_reply(&host, "ide.work.submit", request, &auth, 202, receipt());
    let mut list = common("work_list");
    list["task_kind_filter"] = json!("defect");
    assert_reply(&host, "ide.work.list", list.clone(), &auth, 200, response);
    list["task_kind_filter"] = json!("feature");
    assert_reply(
        &host,
        "ide.work.list",
        list,
        &auth,
        503,
        failure("unavailable"),
    );
    worker.join().unwrap();
}
