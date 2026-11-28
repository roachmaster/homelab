//! Actual private ready-port composition over an owned disposable database.
use super::*;
use crate::pg_fixture::{pg16_available, DisposablePostgresCluster};
use forge_work::work_api::{decode, WorkCommand, WorkDestination};
use forge_work_publication::delivery_repair::RepairSubmitPort;

fn config() -> WorkApiConfig {
    forge_work::work_config::parse_work_config(&json!({
        "owner":"repair-fixture","allowed_projects":["homelab"],"allowed_modes":["plan","review","implement"],
        "profiles": {
            "grok-primary":{"backend":"grok","model":"fixture","resource_profile":"forge-coding-v10","branch":"feature/grok","execution_enabled":true},
            "codex-author":{"backend":"codex","model":"gpt-5","resource_profile":"forge-coding-v10","branch":"feature/codex"},
            "codex-primary":{"backend":"codex","model":"gpt-5","resource_profile":"forge-coding-v10"}
        },
        "project_workflows":{"homelab":{"author_profile":"grok-primary","reviewer_profile":"codex-primary"}},
        "workflow_policy_compatibility":"forge-frozen-workflow-roles-v1",
        "delivery_enabled":true,"delivery_repair_projects":["homelab"],"delivery_repair_authors":{"homelab":"codex-author"}
    })).unwrap()
}

#[test]
fn actual_private_repair_uses_normal_admission_atomic_lease_and_queued_receipt() {
    if !pg16_available() {
        assert_ne!(
            std::env::var("FORGE_REQUIRE_DISPOSABLE_POSTGRES").as_deref(),
            Ok("1")
        );
        return;
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let mut cluster = DisposablePostgresCluster::start().unwrap();
    let config = config();
    let mut pg = PgConfig::new(&config.owner);
    pg.host = cluster.root.to_string_lossy().into_owned();
    pg.user = "forge_agent".into();
    let normal = create_work_service(&config, &pg).unwrap(); // no background thread/model is started
    let snapshot = config
        .project_workflow("homelab")
        .unwrap()
        .snapshot(
            &config,
            &config.repair_policy,
            &forge_work::work_config::native_policy_matches,
        )
        .unwrap();
    let public = json!({"kind":"work_submit","request_id":"fixture","project_alias":"homelab","base_revision":"a".repeat(40),"mode":"plan","agent_profile":"grok-primary","prompt":"first catalog recipe","acceptance_criteria":"retained","idempotency_key":"first-native","workflow_policy_digest":snapshot["policy_digest"]});
    let command = decode(WorkDestination::Submit, "fixture", &public).unwrap();
    let (_, response) = normal
        .service
        .dispatch(WorkDestination::Submit, "fixture", command, None)
        .unwrap();
    assert_eq!(response["event"]["status"], "queued");
    assert!(response["event"].get("actor").is_none());
    let mut forged = public;
    forged["delivery_admission"] = json!({"operation_id":Uuid::new_v4().to_string(),"generation":1,"lease_token":Uuid::new_v4().to_string()});
    assert!(decode(WorkDestination::Submit, "fixture", &forged).is_err());
    // A failed observed-main fixture (not a fabricated production CI receipt).
    let verification = Uuid::new_v4();
    let lineage = Uuid::new_v4();
    runtime.block_on(async {
        let admin = cluster.connect("postgres").await.unwrap();
        admin.execute("INSERT INTO forge_agent.delivery_projects VALUES('repair-fixture','homelab','fixture/repo')",&[]).await.unwrap();
        admin.execute("INSERT INTO forge_agent.delivery_capability_grants VALUES('forge_agent','repair-fixture','homelab','repair',true)",&[]).await.unwrap();
        admin.execute("INSERT INTO forge_agent.delivery_lineages(lineage_id,owner_id,project_alias) VALUES($1,'repair-fixture','homelab')",&[&lineage]).await.unwrap();
        admin.execute("INSERT INTO forge_agent.delivery_ledger(verification_id,owner_id,project_alias,source_kind,transport_scope_id,correlation_id,commit_sha,tree_sha,lineage_id,verification_status,verification_receipt) VALUES($1,'repair-fixture','homelab','observed',$1,$1,$2,$3,$4,'failed','{}')",&[&verification,&"a".repeat(40),&"b".repeat(40),&lineage]).await.unwrap();
        admin.execute("INSERT INTO forge_agent.delivery_project_main_heads(owner_id,project_alias,repository,commit_sha,tree_sha,verification_id) VALUES('repair-fixture','homelab','fixture/repo',$1,$2,$3)",&[&"a".repeat(40),&"b".repeat(40),&verification]).await.unwrap();
        admin.execute("INSERT INTO forge_agent.delivery_operations(verification_id,owner_id,project_alias,kind) VALUES($1,'repair-fixture','homelab','repair')",&[&verification]).await.unwrap();
    });
    let mut consumer = build_delivery_repair(&config, &pg, &normal.service).unwrap();
    assert!(consumer.run_once().unwrap());
    assert!(!consumer.run_once().unwrap());
    let (task, request, generation, lease_token, operation) = runtime.block_on(async {
        let admin = cluster.connect("postgres").await.unwrap();
        let row=admin.query_one("SELECT o.status,o.task_id,o.request,o.receipt,o.finish_generation,o.finish_token,o.operation_id,l.rounds,t.agent_profile,w.snapshot,e.payload FROM forge_agent.delivery_operations o JOIN forge_agent.delivery_lineages l USING(owner_id,project_alias) JOIN forge_agent.coding_tasks t ON t.id=o.task_id JOIN forge_agent.coding_workflows w ON w.root_task_id=t.id JOIN forge_agent.coding_events e ON e.task_id=t.id AND e.sequence=1 WHERE o.verification_id=$1",&[&verification]).await.unwrap();
        assert_eq!(row.get::<_,String>("status"),"admitted");
        assert_eq!(row.get::<_,i32>("rounds"),1);
        assert_eq!(row.get::<_,String>("agent_profile"),"codex-author");
        let snapshot:Value=row.get("snapshot"); assert_eq!(snapshot["author_profile"],"codex-author"); assert_eq!(snapshot["reviewer_profile"],"codex-primary");
        let raw:Value=row.get("payload"); let receipt:Value=row.get("receipt"); assert_eq!(raw,receipt); assert!(raw.get("actor").is_some());
        let count:i64=admin.query_one("SELECT count(*) FROM forge_agent.delivery_task_lineages WHERE task_id=$1",&[&row.get::<_,Uuid>("task_id")]).await.unwrap().get(0); assert_eq!(count,1);
        (row.get::<_,Uuid>("task_id"),row.get::<_,Value>("request"),row.get::<_,i32>("finish_generation"),row.get::<_,Uuid>("finish_token"),row.get::<_,Uuid>("operation_id"))
    });
    // Exact committed replay keeps the original private receipt and consumes no round.
    let command = &request["command"];
    let mut ready = RepairSubmit {
        service: Arc::clone(&normal.service),
        pg: pg.clone(),
    };
    let submit = forge_work_publication::delivery_repair::RepairSubmit {
        project_alias: "homelab".into(),
        base_revision: "a".repeat(40),
        mode: "plan".into(),
        agent_profile: "codex-author".into(),
        prompt: command["prompt"].as_str().unwrap().into(),
        acceptance_criteria: command["acceptance_criteria"].as_str().unwrap().into(),
        idempotency_key: command["idempotency_key"].as_str().unwrap().into(),
        auto_repair: false,
        workflow_policy_digest: command["workflow_policy_digest"]
            .as_str()
            .map(str::to_string),
        workflow_policy_id: None,
        delivery_admission: Some(
            json!({"operation_id":operation.to_string(),"generation":generation,"lease_token":lease_token.to_string()}),
        ),
    };
    assert_eq!(ready.submit(&submit).unwrap()["task_id"], task.to_string());
    // A different task cannot borrow a finished lease; the transaction creates no row.
    let mut rejected = submit;
    rejected.idempotency_key = "forged-private".into();
    assert!(ready.submit(&rejected).is_err());
    runtime.block_on(async {
        let admin=cluster.connect("postgres").await.unwrap();
        assert_eq!(admin.query_one("SELECT count(*) FROM forge_agent.coding_tasks WHERE idempotency_key='forged-private'",&[]).await.unwrap().get::<_,i64>(0),0);
        assert_eq!(admin.query_one("SELECT rounds FROM forge_agent.delivery_lineages WHERE lineage_id=$1",&[&lineage]).await.unwrap().get::<_,i32>(0),1);
    });
    let (_, opened) = normal
        .service
        .dispatch(
            WorkDestination::Get,
            "fixture",
            WorkCommand::Get {
                project_alias: "homelab".into(),
                task_id: task,
            },
            Some(1),
        )
        .unwrap();
    assert_eq!(opened["event"]["task"]["task_id"], task.to_string());
    let (_, catalog) = normal
        .service
        .dispatch(
            WorkDestination::Catalog,
            "fixture",
            WorkCommand::Catalog,
            Some(1),
        )
        .unwrap();
    assert_eq!(catalog["event"]["kind"], "work_catalog");
    let mut stale = forged;
    stale.as_object_mut().unwrap().remove("delivery_admission");
    stale["base_revision"] = json!("c".repeat(40));
    stale["idempotency_key"] = json!("stale-main");
    let command = decode(WorkDestination::Submit, "fixture", &stale).unwrap();
    assert!(
        matches!(normal.service.dispatch(WorkDestination::Submit,"fixture",command,None),Err(WorkError::Rejected{status:409,code}) if code=="result_mismatch:base_stale")
    );
    assert_eq!(
        config.project_workflow("homelab").unwrap().author_profile,
        "grok-primary"
    );
    let policy_id = catalog["event"]["projects"][0]["workflow"]["policy_id"].clone();
    assert!(policy_id.is_string());
    let mut id_only = stale.clone();
    id_only["base_revision"] = json!("a".repeat(40));
    id_only["idempotency_key"] = json!("first-native");
    id_only
        .as_object_mut()
        .unwrap()
        .remove("workflow_policy_digest");
    id_only["workflow_policy_id"] = policy_id.clone();
    let command = decode(WorkDestination::Submit, "fixture", &id_only).unwrap();
    let (_, replayed) = normal
        .service
        .dispatch(WorkDestination::Submit, "fixture", command, None)
        .unwrap();
    assert_eq!(replayed["event"]["task_id"], response["event"]["task_id"]);
    id_only["idempotency_key"] = json!("id-only-new");
    let command = decode(WorkDestination::Submit, "fixture", &id_only).unwrap();
    let (_, new) = normal
        .service
        .dispatch(WorkDestination::Submit, "fixture", command, None)
        .unwrap();
    let id_task = Uuid::parse_str(new["event"]["task_id"].as_str().unwrap()).unwrap();
    runtime.block_on(async {
        let admin=cluster.connect("postgres").await.unwrap();
        let row=admin.query_one("SELECT t.intent_digest,w.snapshot FROM forge_agent.coding_tasks t JOIN forge_agent.coding_workflows w ON w.root_task_id=t.id WHERE t.id=$1",&[&id_task]).await.unwrap();
        let intent=json!({"project_alias":"homelab","base_revision":"a".repeat(40),"mode":"plan","agent_profile":"grok-primary","prompt":"first catalog recipe","acceptance_criteria":"retained","workflow_policy_id":policy_id});
        assert_eq!(row.get::<_,String>("intent_digest"),forge_work::execution_policy::sha256_canonical(&intent));
        assert_eq!(row.get::<_,Value>("snapshot")["author_profile"],"grok-primary");
    });
    let mut digest_replay = id_only.clone();
    digest_replay
        .as_object_mut()
        .unwrap()
        .remove("workflow_policy_id");
    digest_replay["workflow_policy_digest"] = snapshot["policy_digest"].clone();
    let command = decode(WorkDestination::Submit, "fixture", &digest_replay).unwrap();
    assert_eq!(
        normal
            .service
            .dispatch(WorkDestination::Submit, "fixture", command, None)
            .unwrap()
            .1["event"]["task_id"],
        id_task.to_string()
    );
    digest_replay["prompt"] = json!("changed intent");
    let command = decode(WorkDestination::Submit, "fixture", &digest_replay).unwrap();
    assert!(
        matches!(normal.service.dispatch(WorkDestination::Submit,"fixture",command,None),Err(WorkError::Rejected{status:409,code}) if code=="idempotency_conflict")
    );
    // Claim contract failures roll back the *actual* SQL claim and its
    // reservation, rather than committing a task the executor cannot start.
    runtime
        .block_on(cluster.grant_worker(
            &config.owner,
            "homelab",
            "codex-author",
            &["plan"],
            "forge_coding_worker",
        ))
        .unwrap();
    let mut worker_pg = pg.clone();
    worker_pg.user = "forge_coding_worker".into();
    let mut worker = crate::worker::WorkerApi::new(PgSession::connect(worker_pg).unwrap());
    let incarnation = Uuid::new_v4();
    worker
        .advertise(
            "repair-fixture-worker",
            incarnation,
            &[crate::worker::AdvertisedProject {
                alias: "homelab".into(),
                base_revision: Some("a".repeat(40)),
            }],
            &[crate::worker::AdvertisedProfile {
                alias: "codex-author".into(),
                backend: "codex".into(),
                model: "gpt-5".into(),
                modes: vec!["plan".into()],
            }],
            &forge_work_worker::policy::policy_digest(forge_work_worker::policy::POLICY_ID)
                .unwrap(),
        )
        .unwrap();
    runtime.block_on(async {
        let admin=cluster.connect("postgres").await.unwrap();
        admin.batch_execute("ALTER FUNCTION forge_coding_worker_api.claim(text,uuid,text[]) RENAME TO fixture_original_claim; CREATE FUNCTION forge_coding_worker_api.claim(text,uuid,text[]) RETURNS jsonb LANGUAGE sql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fixture$ SELECT value || jsonb_build_object('parent_task_id',value->>'task_id') FROM (SELECT forge_coding_worker_api.fixture_original_claim($1,$2,$3) AS value) q $fixture$; GRANT EXECUTE ON FUNCTION forge_coding_worker_api.claim(text,uuid,text[]) TO forge_coding_worker;").await.unwrap();
    });
    assert!(worker
        .claim(
            "repair-fixture-worker",
            incarnation,
            &["codex-author".into()]
        )
        .is_err());
    runtime.block_on(async {
        let admin=cluster.connect("postgres").await.unwrap();
        assert_eq!(admin.query_one("SELECT status,latest_attempt_id FROM forge_agent.coding_tasks WHERE id=$1",&[&task]).await.unwrap().get::<_,String>("status"),"queued");
        assert_eq!(admin.query_one("SELECT count(*) FROM forge_agent.coding_attempts WHERE task_id=$1",&[&task]).await.unwrap().get::<_,i64>(0),0);
        admin.batch_execute("DROP FUNCTION forge_coding_worker_api.claim(text,uuid,text[]); ALTER FUNCTION forge_coding_worker_api.fixture_original_claim(text,uuid,text[]) RENAME TO claim;").await.unwrap();
    });
    let claimed = worker
        .claim(
            "repair-fixture-worker",
            incarnation,
            &["codex-author".into()],
        )
        .unwrap()
        .unwrap();
    assert_eq!(claimed.task_id, task.to_string());
    let (_, running) = normal
        .service
        .dispatch(
            WorkDestination::Get,
            "fixture",
            WorkCommand::Get {
                project_alias: "homelab".into(),
                task_id: task,
            },
            Some(1),
        )
        .unwrap();
    let detail = &running["event"]["task"];
    assert_eq!(detail["workflow"]["author_profile"], "codex-author");
    assert_eq!(detail["attempt"]["phase"], "starting");
    assert!(detail["attempt"]["finished_at"].is_null());
    assert!(detail.get("result").is_none());
    for forbidden in [
        "latest_attempt_id",
        "plan_decision_id",
        "workflow_id",
        "workflow_review_id",
    ] {
        assert!(detail.get(forbidden).is_none());
    }
    for forbidden in [
        "published_at",
        "result_sha256",
        "result_byte_count",
        "result_chunk_count",
    ] {
        assert!(detail["attempt"].get(forbidden).is_none());
    }

    let read_store = pg_work_store(&config, &pg).unwrap();
    assert_eq!(
        crate::work_common::run_read_tx(&read_store, |tx| tx
            .call_scalar::<String>("SELECT current_setting('transaction_isolation')", &[])
            .map_err(work_err))
        .unwrap()
        .as_deref(),
        Some("repeatable read")
    );
    assert_eq!(
        crate::work_common::run_tx(&read_store, |tx| tx
            .call_scalar::<String>("SELECT current_setting('transaction_isolation')", &[])
            .map_err(work_err))
        .unwrap()
        .as_deref(),
        Some("read committed")
    );
    // Public event projection is checked through the same ready API over
    // immutable fixture rows, including forged/malformed report payloads.
    runtime.block_on(async {
        let admin=cluster.connect("postgres").await.unwrap();
        let reports=[
            ("checkpoint",json!({"attempt_id":claimed.attempt_id.to_string(),"progress_sequence":1,"stage":"plan","summary":"Owned progress","actor":{"kind":"worker","id":"fixture"}})),
            ("checkpoint",json!({"attempt_id":Uuid::new_v4().to_string(),"progress_sequence":1,"stage":"plan","summary":"Foreign report","actor":{"kind":"worker","id":"fixture"}})),
            ("failed",json!({"cause":"agent_failed","cause_summary":"Bounded failure","private":"never expose"})),
            ("cancelled",json!({"actor":{"kind":"operator","id":"fixture"},"cause":"operator_request","cause_detail":"Recorded operator stop","private":"never expose"})),
            ("checkpoint",json!({"workflow_id":claimed.workflow_id.unwrap().to_string(),"phase":"plan","summary":"Bound workflow progress","related_task_id":task.to_string(),"actor":{"kind":"coordinator","id":"fixture"}})),
            ("checkpoint",json!({"summary":"private arbitrary checkpoint"})),
            ("blocked_auth",json!({"actor":{"kind":"worker","id":"fixture"}})),
        ];
        for (index,(kind,payload)) in reports.iter().enumerate() {
            admin.execute("INSERT INTO forge_agent.coding_events(owner_id,project_alias,task_id,sequence,event_kind,payload) VALUES('repair-fixture','homelab',$1,$2,$3,$4)",&[&task,&(index as i64+3),kind,payload]).await.unwrap();
        }
    });
    let (_, events) = normal
        .service
        .dispatch(
            WorkDestination::Events,
            "fixture",
            WorkCommand::Events {
                project_alias: "homelab".into(),
                task_id: task,
                after_sequence: 0,
                limit: 100,
            },
            None,
        )
        .unwrap();
    let events = events["event"]["events"].as_array().unwrap();
    assert_eq!(events.len(), 9);
    assert_eq!(events[2]["progress"]["summary"], "Owned progress");
    assert_eq!(events[2]["progress_verified"], true);
    assert!(events[3].get("progress").is_none());
    assert_eq!(events[4]["failure"]["summary"], "Bounded failure");
    assert_eq!(events[5]["attribution"]["detail"], "Recorded operator stop");
    assert_eq!(
        events[6]["workflow_progress"]["related_task_id"],
        task.to_string()
    );
    assert!(events[7].get("progress").is_none());
    assert_eq!(events[8]["kind"], "blocked_auth");
    for event in events {
        assert!(event["event_id"].as_str().is_some());
        assert!(event["correlation_id"].as_str().is_some());
        for forbidden in ["payload", "actor", "cause", "private", "summary"] {
            assert!(event.get(forbidden).is_none());
        }
    }
    cluster.stop();
}

#[test]
fn external_admission_and_context_share_task_transaction() {
    if !pg16_available() {
        assert_ne!(
            std::env::var("FORGE_REQUIRE_DISPOSABLE_POSTGRES").as_deref(),
            Ok("1")
        );
        return;
    }
    for managed in [false, true] {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let mut cluster = DisposablePostgresCluster::start().unwrap();
        let resource = if managed {
            "forge-external-db-v2"
        } else {
            "forge-external-db-v1"
        };
        let mut raw = json!({"owner":"external-fixture","allowed_projects":["homelab"],"allowed_modes":["plan","review","implement"],"identity_protocol":"content-v2","profiles":{
            "muse-db-primary":{"backend":"muse","model":"external-runner","resource_profile":resource,"branch":"feature/muse","execution_enabled":true},
            "codex-primary":{"backend":"codex","model":"gpt-5","resource_profile":"forge-coding-v10"}
        }});
        if managed {
            raw["external_workflows"] = json!({"homelab":{"author_profile":"muse-db-primary","reviewer_profile":"codex-primary"}});
        }
        let config = forge_work::work_config::parse_work_config(&raw).unwrap();
        let mut pg = PgConfig::new(&config.owner);
        pg.host = cluster.root.to_string_lossy().into_owned();
        pg.user = "forge_agent".into();
        let normal = create_work_service(&config, &pg).unwrap();
        let mut body = json!({"kind":"work_submit","request_id":"fixture","project_alias":"homelab","base_revision":"a".repeat(40),"mode":"plan","agent_profile":"muse-db-primary","prompt":"owned external admission","acceptance_criteria":"preserve scoped context","idempotency_key":"external-first"});
        if managed {
            body["workflow_policy_digest"] = config
                .external_workflow("homelab")
                .unwrap()
                .snapshot(&config, &config.native_policy_ref())
                .unwrap()["policy_digest"]
                .clone();
        }
        // Missing protected service scope rolls back task, policy/identity rows,
        // events and assignment together (no separate coordinator discovers it).
        let command = decode(WorkDestination::Submit, "fixture", &body).unwrap();
        assert!(normal
            .service
            .dispatch(WorkDestination::Submit, "fixture", command, None)
            .is_err());
        runtime.block_on(async {
            let admin=cluster.connect("postgres").await.unwrap();
            assert_eq!(admin.query_one("SELECT count(*) FROM forge_agent.coding_tasks",&[]).await.unwrap().get::<_,i64>(0),0);
            assert_eq!(admin.query_one("SELECT count(*) FROM forge_external_agent.assignments",&[]).await.unwrap().get::<_,i64>(0),0);
            admin.batch_execute("CREATE ROLE fixture_muse LOGIN; INSERT INTO forge_external_agent.scope_grants(external_role,service_role,owner_id,project_alias,profile_alias,enabled) VALUES('fixture_muse','forge_agent','external-fixture','homelab','muse-db-primary',true);").await.unwrap();
        });
        let command = decode(WorkDestination::Submit, "fixture", &body).unwrap();
        let (_, response) = normal
            .service
            .dispatch(WorkDestination::Submit, "fixture", command, None)
            .unwrap();
        let task = Uuid::parse_str(response["event"]["task_id"].as_str().unwrap()).unwrap();
        let command = decode(WorkDestination::Submit, "fixture", &body).unwrap();
        assert_eq!(
            normal
                .service
                .dispatch(WorkDestination::Submit, "fixture", command, None)
                .unwrap()
                .1,
            response
        );
        runtime.block_on(async {
            let admin=cluster.connect("postgres").await.unwrap();
            let row=admin.query_one("SELECT auto_repair,workflow_required,workflow_id FROM forge_agent.coding_tasks WHERE id=$1",&[&task]).await.unwrap();
            assert!(!row.get::<_,bool>("auto_repair")); assert!(!row.get::<_,bool>("workflow_required")); assert!(row.get::<_,Option<Uuid>>("workflow_id").is_none());
            assert_eq!(admin.query_one("SELECT count(*) FROM forge_agent.coding_workflows",&[]).await.unwrap().get::<_,i64>(0),0);
            assert_eq!(admin.query_one("SELECT count(*) FROM forge_external_agent.assignments WHERE task_id=$1",&[&task]).await.unwrap().get::<_,i64>(0),1);
            if managed {
                let row=admin.query_one("SELECT c.content,c.sha256,w.state FROM forge_external_agent.assignments a JOIN forge_external_workflow.contexts c USING(assignment_id) JOIN forge_external_workflow.tasks l ON l.task_id=a.task_id JOIN forge_external_workflow.workflows w USING(workflow_id) WHERE a.task_id=$1",&[&task]).await.unwrap();
                let content:String=row.get("content");
                let context:Value=serde_json::from_str(&content).unwrap(); assert_eq!(context["stage"],"plan"); assert_eq!(context["request_prompt"],"owned external admission");
                assert_eq!(row.get::<_,String>("sha256"),forge_work::canonical::sha256_hex(content.as_bytes()));
                assert_eq!(row.get::<_,Value>("state")["identity_protocol"],"content-v2");
            } else {
                assert_eq!(admin.query_one("SELECT count(*) FROM forge_external_workflow.tasks",&[]).await.unwrap().get::<_,i64>(0),0);
            }
        });
        let (_, opened) = normal
            .service
            .dispatch(
                WorkDestination::Get,
                "fixture",
                WorkCommand::Get {
                    project_alias: "homelab".into(),
                    task_id: task,
                },
                Some(1),
            )
            .unwrap();
        if managed {
            assert_eq!(
                opened["event"]["task"]["workflow"]["kind"],
                "external_database"
            );
        }
        runtime.block_on(async {
            let admin=cluster.connect("postgres").await.unwrap();
            admin.batch_execute("GRANT CONNECT ON DATABASE forge_agent TO fixture_muse; GRANT USAGE ON SCHEMA forge_external_agent_api TO fixture_muse; GRANT EXECUTE ON FUNCTION forge_external_agent_api.advertise(text,text,text),forge_external_agent_api.claim(text,text),forge_external_agent_api.progress(uuid,integer,uuid,text),forge_external_agent_api.finish(uuid,integer,uuid,text,text) TO fixture_muse;").await.unwrap();
            let external=cluster.connect("fixture_muse").await.unwrap();
            external.query_one("SELECT forge_external_agent_api.advertise('homelab','muse-db-primary','fixture-reader')",&[]).await.unwrap();
            let claim:Value=external.query_one("SELECT forge_external_agent_api.claim('homelab','muse-db-primary')",&[]).await.unwrap().get(0);
            let assignment=Uuid::parse_str(claim["assignment_id"].as_str().unwrap()).unwrap();
            let token=Uuid::parse_str(claim["token"].as_str().unwrap()).unwrap();
            let generation=claim["generation"].as_i64().unwrap() as i32;
            external.query_one("SELECT forge_external_agent_api.progress($1,$2,$3,'Owned external fixture progress')",&[&assignment,&generation,&token]).await.unwrap();
            external.query_one("SELECT forge_external_agent_api.finish($1,$2,$3,'Owned fixture plan content','Owned fixture plan summary')",&[&assignment,&generation,&token]).await.unwrap();
        });
        let (_, opened) = normal
            .service
            .dispatch(
                WorkDestination::Get,
                "fixture",
                WorkCommand::Get {
                    project_alias: "homelab".into(),
                    task_id: task,
                },
                Some(1),
            )
            .unwrap();
        assert_eq!(
            opened["event"]["task"]["external_result"]["content"],
            "Owned fixture plan content"
        );
        assert!(opened["event"]["task"].get("attempt").is_none());
        let (_, history) = normal
            .service
            .dispatch(
                WorkDestination::Events,
                "fixture",
                WorkCommand::Events {
                    project_alias: "homelab".into(),
                    task_id: task,
                    after_sequence: 0,
                    limit: 100,
                },
                None,
            )
            .unwrap();
        assert!(history["event"]["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(
                |event| event["external_progress"]["summary"] == "Owned external fixture progress"
            ));
        if managed {
            let workflow = runtime.block_on(async {
                cluster
                    .connect("postgres")
                    .await
                    .unwrap()
                    .query_one(
                        "SELECT workflow_id FROM forge_external_workflow.tasks WHERE task_id=$1",
                        &[&task],
                    )
                    .await
                    .unwrap()
                    .get::<_, Uuid>(0)
            });
            let mut defect = body.clone();
            defect["idempotency_key"] = json!("external-discovered-defect");
            defect["task_kind"] = json!("defect");
            defect["defect"] = json!({"summary":"Scoped external observation","location":{"repo_path":"src/rust/forge-work/src/work_api.rs","function":"submit","commit":"a".repeat(40)},"observed_behavior":"An owned fixture observation","expected_behavior":"Preserve the discovered workflow reference","severity":"high","evidence":"Disposable database fixture","discovering_task_id":task.to_string(),"discovering_workflow_id":workflow.to_string()});
            let command = decode(WorkDestination::Submit, "fixture", &defect).unwrap();
            let (_, accepted) = normal
                .service
                .dispatch(WorkDestination::Submit, "fixture", command, None)
                .unwrap();
            let defect_task =
                Uuid::parse_str(accepted["event"]["task_id"].as_str().unwrap()).unwrap();
            runtime.block_on(async {
                let admin=cluster.connect("postgres").await.unwrap();
                let content:String=admin.query_one("SELECT c.content FROM forge_external_agent.assignments a JOIN forge_external_workflow.contexts c USING(assignment_id) WHERE a.task_id=$1",&[&defect_task]).await.unwrap().get(0);
                assert_eq!(serde_json::from_str::<Value>(&content).unwrap()["task_kind"],"defect");
            });
            defect["idempotency_key"] = json!("external-forged-reference");
            defect["defect"]["discovering_workflow_id"] = json!(Uuid::new_v4().to_string());
            let command = decode(WorkDestination::Submit, "fixture", &defect).unwrap();
            assert!(matches!(
                normal
                    .service
                    .dispatch(WorkDestination::Submit, "fixture", command, None),
                Err(WorkError::Rejected { status: 400, .. })
            ));
        }
        cluster.stop();
    }
}
