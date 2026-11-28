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
    cluster.stop();
}
