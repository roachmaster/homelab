//! Read-only list classification over owned fixtures, including retained audits.
use super::*;
use crate::pg_fixture::DisposablePostgresCluster;
use forge_work::work_api::{WorkCommand, WorkDestination};

fn page(service: &Arc<WorkApiService>, scope: &str, limit: i64, cursor: Option<Uuid>) -> Value {
    service
        .dispatch(
            WorkDestination::List,
            "list-fixture",
            WorkCommand::List {
                project_alias: "homelab".into(),
                status_filter: Some(scope.into()),
                limit,
                cursor,
                task_kind_filter: None,
            },
            Some(1),
        )
        .unwrap()
        .1["event"]
        .clone()
}

pub(super) fn external(service: &Arc<WorkApiService>, task: Uuid, managed: bool) {
    let all = page(service, "all", 1, None);
    assert_eq!(all["scope_counts"]["all"], 1);
    assert_eq!(all["tasks"][0]["task_id"], task.to_string());
    assert!(all["tasks"][0]["updated_at"].as_str().is_some());
    if managed {
        assert_eq!(all["tasks"][0]["workflow_required"], true);
        assert!(all["tasks"][0]["workflow_phase"].as_str().is_some());
    }
}

pub(super) fn exercise(
    cluster: &DisposablePostgresCluster,
    runtime: &tokio::runtime::Runtime,
    source: Uuid,
) {
    let mut config = super::runner_repair_pg_tests::config();
    config.owner = "mission-control".into();
    let mut pg = PgConfig::new(&config.owner);
    pg.host = cluster.root.to_string_lossy().into_owned();
    pg.user = "forge_agent".into();
    let service = create_work_service(&config, &pg).unwrap().service;
    let ids = [
        (
            "0d116b62-fa06-4121-a32f-031110dd240d",
            "1eca52aa-f110-485e-8bdf-fee1242c7bf7",
        ),
        (
            "c8ec1815-bd21-429c-8ed0-7ed0ae0bc392",
            "ad573549-94d2-42b7-90da-1c6733751050",
        ),
    ];
    runtime.block_on(async{
        let admin=cluster.connect("postgres").await.unwrap();
        for (task,attempt) in ids {
            let task=Uuid::parse_str(task).unwrap();let attempt=Uuid::parse_str(attempt).unwrap();let unit=format!("forge-coding-{}",attempt.simple());let rec=Uuid::new_v4();
            admin.execute("INSERT INTO forge_agent.coding_tasks(owner_id,project_alias,id,correlation_id,idempotency_key,intent_digest,base_revision,mode,agent_profile,prompt,acceptance_criteria,pinned_recipe,recipe_digest,status,revision,latest_attempt_id,review_required) SELECT 'mission-control',project_alias,$1,$2,$3,intent_digest,base_revision,'plan',agent_profile,'Old diagnostic','Retain failure',pinned_recipe,recipe_digest,'failed',3,$4,false FROM forge_agent.coding_tasks WHERE id=$5",&[&task,&Uuid::new_v4(),&task.to_string(),&attempt,&source]).await.unwrap();
            admin.execute("INSERT INTO forge_agent.coding_attempts(owner_id,project_alias,task_id,attempt_id,attempt_no,attempt_token,agent_profile,pinned_recipe,recipe_digest,worker_role,worker_id,worker_incarnation,unit_name,phase,reservation_active,outcome,outcome_digest,publish_receipt,deadline_at,finished_at,published_at) SELECT owner_id,project_alias,id,$1,1,$2,agent_profile,pinned_recipe,recipe_digest,'forge_coding_worker','fixture',$3,$4,'stopped',false,'failed',repeat('e',64),'{}'::jsonb,clock_timestamp(),clock_timestamp(),clock_timestamp() FROM forge_agent.coding_tasks WHERE id=$5",&[&attempt,&Uuid::new_v4(),&Uuid::new_v4(),&unit,&task]).await.unwrap();
            admin.execute("INSERT INTO forge_agent.coding_reconciliation_receipts(receipt_id,owner_id,project_alias,task_id,attempt_id,task_revision,workspace_digest,stop_proof,reconciled_by) VALUES($1,'mission-control','homelab',$2,$3,3,repeat('d',64),jsonb_build_object('source','systemd','scope_stopped',true,'unit_name',$4::text),'fixture')",&[&rec,&task,&attempt,&unit]).await.unwrap();
            admin.execute("INSERT INTO forge_agent.coding_events(owner_id,project_alias,task_id,sequence,event_kind,payload) SELECT owner_id,project_alias,id,1,'queued',jsonb_build_object('task_id',id::text,'project_alias',project_alias,'revision',1,'status','queued','recipe_digest',recipe_digest,'actor',jsonb_build_object('kind','operator','id',owner_id)) FROM forge_agent.coding_tasks WHERE id=$1",&[&task]).await.unwrap();
            admin.execute("INSERT INTO forge_agent.coding_events(owner_id,project_alias,task_id,sequence,event_kind,payload) SELECT t.owner_id,t.project_alias,t.id,2,'checkpoint',jsonb_build_object('version',1,'policy','forge-operator-diagnostic-archive-v1','stage','operator_diagnostic_archived','owner_id',t.owner_id,'project_alias',t.project_alias,'task_id',t.id::text,'revision',t.revision,'status',t.status,'latest_attempt_id',t.latest_attempt_id::text,'result_sha256',NULL,'recipe_digest',t.recipe_digest,'reconciliation_receipt',to_jsonb(rec),'scope_proof',jsonb_build_object('attempt_id',t.latest_attempt_id::text,'unit_name',a.unit_name,'scope_stopped',true),'reason','obsolete one-off diagnostic; original failure retained','summary','Archived obsolete one-off diagnostic; original failed attempt and evidence retained.','actor',jsonb_build_object('kind','operator','id',t.owner_id),'source_sha256',repeat('a',64),'idempotency_key',$1::text,'reconciliation_receipt_id',rec.receipt_id::text,'source_proof',jsonb_build_object('qualified_source_revision',repeat('b',40),'qualification_receipt_sha256',repeat('c',64),'source_verified',true)) FROM forge_agent.coding_tasks t JOIN forge_agent.coding_attempts a ON a.attempt_id=t.latest_attempt_id JOIN forge_agent.coding_reconciliation_receipts rec ON rec.task_id=t.id WHERE t.id=$2",&[&Uuid::new_v4().to_string(),&task]).await.unwrap();
        }
    });
    let wrong_cursor = service.dispatch(
        WorkDestination::List,
        "list-fixture",
        WorkCommand::List {
            project_alias: "homelab".into(),
            status_filter: Some("all".into()),
            limit: 1,
            cursor: Some(source),
            task_kind_filter: None,
        },
        Some(1),
    );
    assert!(matches!(
        wrong_cursor,
        Err(WorkError::Rejected { status: 400, .. })
    ));
    let raw = page(&service, "failed", 50, None);
    assert!(raw.get("scope_counts").is_none());
    assert_eq!(raw["tasks"].as_array().unwrap().len(), 2);
    assert!(raw["tasks"][0].get("updated_at").is_none());
    let original=runtime.block_on(async{cluster.connect("postgres").await.unwrap().query_one("SELECT jsonb_agg(to_jsonb(t) ORDER BY id) FROM forge_agent.coding_tasks t WHERE owner_id='mission-control'",&[]).await.unwrap().get::<_,Value>(0)});
    let all = page(&service, "all", 1, None);
    assert_eq!(
        all["scope_counts"],
        json!({"all":2,"history":2,"attention":0,"active":0})
    );
    assert_eq!(all["tasks"][0]["status"], "failed");
    let cursor = Uuid::parse_str(all["next_cursor"].as_str().unwrap()).unwrap();
    let next = page(&service, "history", 1, Some(cursor));
    assert_eq!(next["scope_counts"], all["scope_counts"]);
    assert_ne!(next["tasks"][0]["task_id"], all["tasks"][0]["task_id"]);
    assert!(next["next_cursor"].is_null());
    runtime.block_on(async{
        let admin=cluster.connect("postgres").await.unwrap();
        assert_eq!(admin.query_one("SELECT jsonb_agg(to_jsonb(t) ORDER BY id) FROM forge_agent.coding_tasks t WHERE owner_id='mission-control'",&[]).await.unwrap().get::<_,Value>(0),original);
        let id=Uuid::parse_str(all["tasks"][0]["task_id"].as_str().unwrap()).unwrap();
        let observed:chrono::DateTime<chrono::Utc>=admin.query_one("SELECT created_at FROM forge_agent.coding_events WHERE task_id=$1 AND event_kind='checkpoint'",&[&id]).await.unwrap().get(0);
        assert_eq!(all["tasks"][0]["updated_at"],crate::work_common::format_ts(observed));
        // A worker-controlled checkpoint cannot claim an operator disposition.
        admin.execute("UPDATE forge_agent.coding_events SET payload=jsonb_set(payload,'{actor,kind}','\"worker\"') WHERE task_id=$1",&[&id]).await.unwrap();
    });
    assert_eq!(
        page(&service, "attention", 50, None)["scope_counts"]["attention"],
        1
    );
    runtime.block_on(async {
        cluster
            .connect("postgres")
            .await
            .unwrap()
            .execute(
                "UPDATE forge_agent.coding_tasks SET revision=4 WHERE id=$1",
                &[&Uuid::parse_str(next["tasks"][0]["task_id"].as_str().unwrap()).unwrap()],
            )
            .await
            .unwrap();
    });
    assert_eq!(
        page(&service, "history", 50, None)["scope_counts"]["history"],
        0
    );
    let snapshot = config
        .project_workflow("homelab")
        .unwrap()
        .snapshot(
            &config,
            &config.repair_policy,
            &forge_work::work_config::native_policy_matches,
        )
        .unwrap();
    let body = json!({"kind":"work_submit","request_id":"list-fixture","project_alias":"homelab","base_revision":"a".repeat(40),"mode":"plan","agent_profile":"grok-primary","prompt":"Reproduce a retained defect","acceptance_criteria":"Normal Plan admission and retained lineage","idempotency_key":"owned-native-defect","workflow_policy_digest":snapshot["policy_digest"],"task_kind":"defect","defect":{"summary":"Retained native fixture","location":{"repo_path":"src/rust/forge-work/src/work_api.rs","function":"submit","commit":"a".repeat(40)},"observed_behavior":"Owned fixture failure","expected_behavior":"A scoped normal defect task","severity":"high","evidence":"Disposable database fixture","discovering_task_id":ids[0].0}});
    let command =
        forge_work::work_api::decode(WorkDestination::Submit, "list-fixture", &body).unwrap();
    let submitted = service
        .dispatch(WorkDestination::Submit, "list-fixture", command, Some(1))
        .unwrap()
        .1;
    let defect = Uuid::parse_str(submitted["event"]["task_id"].as_str().unwrap()).unwrap();
    let opened = service
        .dispatch(
            WorkDestination::Get,
            "list-fixture",
            WorkCommand::Get {
                project_alias: "homelab".into(),
                task_id: defect,
            },
            Some(1),
        )
        .unwrap()
        .1;
    assert_eq!(opened["event"]["task"]["task_kind"], "defect");
    assert_eq!(opened["event"]["task"]["defect"], body["defect"]);
    let discoverer = service
        .dispatch(
            WorkDestination::Get,
            "list-fixture",
            WorkCommand::Get {
                project_alias: "homelab".into(),
                task_id: Uuid::parse_str(ids[0].0).unwrap(),
            },
            Some(1),
        )
        .unwrap()
        .1;
    assert_eq!(
        discoverer["event"]["task"]["filed_defects"][0]["task_id"],
        defect.to_string()
    );
    let listed = service
        .dispatch(
            WorkDestination::List,
            "list-fixture",
            WorkCommand::List {
                project_alias: "homelab".into(),
                status_filter: Some("all".into()),
                limit: 50,
                cursor: None,
                task_kind_filter: Some("defect".into()),
            },
            Some(1),
        )
        .unwrap()
        .1;
    assert_eq!(listed["event"]["scope_counts"]["all"], 1);
    assert_eq!(listed["event"]["tasks"][0]["task_kind"], "defect");
}
