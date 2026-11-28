//! Native gate compatibility through the real, socket-only SQL fixture.
#![cfg(feature = "test-support")]
use forge_postgres::pg_fixture::{pg16_available, sql_dir, DisposablePostgresCluster};
use forge_work_publication::ci_contract::CiGateContractFactory;
use serde_json::{json, Value};

fn evidence(profile: &str) -> (Value, Value) {
    let plan = CiGateContractFactory::new(profile)
        .unwrap()
        .create()
        .plan("main")
        .unwrap();
    let request = json!({"action_id":"123e4567-e89b-42d3-a456-426614174000",
        "prepare_action_id":"123e4567-e89b-52d3-a456-426614174004",
        "workflow_id":"123e4567-e89b-42d3-a456-426614174001",
        "project_alias":"homelab","candidate_commit":"a".repeat(40),"candidate_tree":"b".repeat(40),
        "integration_base":"a".repeat(40),"gate_manifest_sha256":format!("{profile}:main"),
        "branch_scope":"main","source_branch":"main","verification_profile":profile,
        "correlation_id":"123e4567-e89b-42d3-a456-426614174002"});
    let mut receipt = request.clone();
    for key in ["prepare_action_id", "integration_base", "source_branch"] {
        receipt.as_object_mut().unwrap().remove(key);
    }
    let checks: Vec<Value> = plan
        .checks
        .iter()
        .enumerate()
        .map(|(index, argv)| json!({"index":index,"argv":argv,"returncode":0}))
        .collect();
    receipt["version"] = json!(plan.result_version());
    receipt["result_version"] = json!(plan.result_version());
    receipt["expected_main"] = json!("a".repeat(40));
    receipt["provider"] = json!("jenkins");
    receipt["result"] = json!("SUCCESS");
    receipt["cleanup_confirmed"] = json!(true);
    receipt["coverage"] = plan.coverage().unwrap();
    receipt["checks"] = json!(checks);
    for key in ["bundle_sha256", "check_receipt_sha256", "log_sha256"] {
        receipt[key] = json!("c".repeat(64));
    }
    if profile.ends_with("v3") || profile.ends_with("v4") {
        let witness = forge_work_publication::ci_phases::event_stage_evidence_for_profile(
            profile,
            "main",
            &receipt["checks"],
            None,
        )
        .unwrap();
        for key in ["phase_results", "passed_kinds", "failure_kind"] {
            receipt[key] = witness[key].clone();
        }
    }
    (request, receipt)
}

#[test]
fn native_v4_and_historical_main_receipts_require_their_actual_closed_commands() {
    if !pg16_available() {
        assert!(
            std::env::var("FORGE_REQUIRE_DISPOSABLE_POSTGRES").as_deref() != Ok("1"),
            "Main requires actual PostgreSQL"
        );
        return;
    }
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let mut cluster = DisposablePostgresCluster::start().unwrap();
            let client = cluster.connect("postgres").await.unwrap();
            client
                .batch_execute("SET ROLE forge_agent_owner")
                .await
                .unwrap();
            for profile in [
                "forge-sdlc-jenkins-v2",
                "forge-sdlc-jenkins-v3",
                "forge-sdlc-jenkins-v4",
            ] {
                let (request, receipt) = evidence(profile);
            let request_valid=client.query_one("SELECT forge_agent.delivery_request_valid($1::jsonb,($1::jsonb->>'action_id')::uuid,($1::jsonb->>'workflow_id')::uuid,'homelab',$2,$3,($1::jsonb->>'correlation_id')::uuid)", &[&request,&"a".repeat(40),&"b".repeat(40)]).await.unwrap();
            assert!(request_valid.get::<_,bool>(0),"exact request refused: {profile}");
                let row = client
                    .query_one(
                        "SELECT forge_agent.delivery_main_receipt_valid($1,$2,$3)",
                        &[&request, &receipt, &"a".repeat(40)],
                    )
                    .await
                    .unwrap();
                assert!(row.get::<_, bool>(0), "exact {profile} receipt refused");
            }
            let (request, receipt) = evidence("forge-sdlc-jenkins-v4");
            for mutation in [
                "legacy_argv",
                "wrong_version",
                "legacy_coverage",
                "partial",
                "failure_kind",
            ] {
                let mut changed = receipt.clone();
                match mutation {
                    "legacy_argv" => {
                        changed["checks"][45]["argv"] = json!([
                            "/usr/bin/python3",
                            "-B",
                            "deploy/mission-control/backend-suites.py",
                            "--scope",
                            "main"
                        ])
                    }
                    "wrong_version" => changed["result_version"] = json!(3),
                    "legacy_coverage" => changed["coverage"]["backend_suites"] = json!("all_four"),
                    "partial" => {
                        changed["checks"].as_array_mut().unwrap().pop();
                    }
                    _ => changed["failure_kind"] = json!("tests"),
                }
                let row = client
                    .query_one(
                        "SELECT forge_agent.delivery_main_receipt_valid($1,$2,$3)",
                        &[&request, &changed, &"a".repeat(40)],
                    )
                    .await
                    .unwrap();
                assert!(!row.get::<_, bool>(0), "accepted {mutation}");
            }
            let row = client
                .query_one(
                    "SELECT EXISTS(SELECT FROM forge_agent.schema_migrations WHERE version=55)",
                    &[],
                )
                .await
                .unwrap();
            assert!(!row.get::<_, bool>(0));
            cluster.apply_and_verify().unwrap();
            // The strict M56 verifier must reject even a semantically harmless body change.
            let signature = "forge_agent._native_ci_v4_main_receipt_valid(jsonb,jsonb,text)";
            let row = client
                .query_one("SELECT pg_get_functiondef($1::text::regprocedure)", &[&signature])
                .await
                .unwrap();
            let definition: String = row.get(0);
            let changed = definition.replacen(" SELECT", " -- drift\n SELECT", 1);
            client.batch_execute(&changed).await.unwrap();
            let (status, _out, err) = cluster
                .run_sql_script(
                    &sql_dir().join("management/verify-native-ci-v4.sql"),
                    "postgres",
                )
                .unwrap();
            assert_ne!(status, 0);
            assert!(err.contains("Native v4 source/security drift"));
            client.batch_execute(&definition).await.unwrap();
            cluster.apply_and_verify().unwrap();
            cluster.stop();
        });
}
