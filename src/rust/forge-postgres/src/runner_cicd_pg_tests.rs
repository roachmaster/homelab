//! Owned sealed stage fixtures exercise the normal read API; they are not CI
//! qualification receipts and never change any live task/build status.
use super::*;
use crate::pg_fixture::DisposablePostgresCluster;
use forge_work::work_api::{WorkCommand, WorkDestination};

pub(super) struct ReadFixture<'a> {
    pub cluster: &'a DisposablePostgresCluster,
    pub runtime: &'a tokio::runtime::Runtime,
    pub service: &'a Arc<WorkApiService>,
    pub pg: &'a PgConfig,
    pub task: Uuid,
    pub workflow: Uuid,
    pub lineage: Uuid,
}
struct StageFixture<'a> {
    action: Uuid,
    correlation: Uuid,
    index: i32,
    commit: &'a str,
    pipeline: &'a str,
    phase: &'a str,
    bytes: &'a [u8],
    bad_chunk: bool,
}
async fn stage(client: &tokio_postgres::Client, scope: &ReadFixture<'_>, stage: StageFixture<'_>) {
    let StageFixture {
        action,
        correlation,
        index,
        commit,
        pipeline,
        phase,
        bytes,
        bad_chunk,
    } = stage;
    let sha = forge_work::canonical::sha256_hex(bytes);
    let count = i32::try_from(bytes.len().div_ceil(16384)).unwrap();
    let length = i32::try_from(bytes.len()).unwrap();
    let branch = if pipeline == "main" {
        "main"
    } else {
        "feature/codex"
    };
    let branch_scope = if pipeline == "main" {
        "main".into()
    } else {
        forge_work_worker::agent_branch::branch_scope_for_branch(branch).unwrap()
    };
    client.execute("INSERT INTO forge_agent.jenkins_stage_results(owner_id,project_alias,action_id,stage_index,correlation_id,task_id,job_name,build_number,commit_sha,branch_name,branch_scope,pipeline_scope,logical_stage_id,phase_id,phase_kind,label,status,returncode,tests_passed,tests_failed,tests_skipped,test_summary,error_excerpt,log_sha256,log_byte_count,log_chunk_count,receipt_sha256) VALUES($1,'homelab',$2,$3,$4,$5,$6,1,$7,$8,$9,$10,'check',$11,'build','Owned fixture phase','passed',0,7,0,0,$12,'Owned fixture excerpt',$13,$14,$15,$16)",&[&scope.pg.owner,&action,&index,&correlation,&scope.task,&forge_work_publication::pipeline_projection::NATIVE_JENKINS_JOB,&commit,&branch,&branch_scope,&pipeline,&phase,&json!({"cases":[{"name":"owned fixture","status":"passed"}]}),&sha,&length,&count,&"a".repeat(64)]).await.unwrap();
    for (i, chunk) in bytes.chunks(16384).enumerate() {
        let hash = if bad_chunk {
            "0".repeat(64)
        } else {
            forge_work::canonical::sha256_hex(chunk)
        };
        client.execute("INSERT INTO forge_agent.jenkins_stage_log_chunks(owner_id,project_alias,action_id,stage_index,chunk_index,chunk_bytes,byte_offset,byte_len,chunk_sha256) VALUES($1,'homelab',$2,$3,$4,$5,$6,$7,$8)",&[&scope.pg.owner,&action,&index,&(i as i32),&chunk,&((i*16384) as i32),&(chunk.len() as i32),&hash]).await.unwrap();
    }
}
fn log(
    scope: &ReadFixture<'_>,
    correlation: Uuid,
    action: Uuid,
    phase: &str,
    cursor: Option<String>,
    limit: i64,
) -> Result<Value, WorkError> {
    scope
        .service
        .dispatch(
            WorkDestination::CicdStageLog,
            "fixture",
            WorkCommand::CicdStageLog {
                project_alias: "homelab".into(),
                task_id: scope.task,
                correlation_id: correlation,
                action_id: action,
                stage_id: phase.into(),
                view: "console".into(),
                cursor,
                limit_bytes: limit,
            },
            Some(1),
        )
        .map(|(_, body)| body)
}
pub(super) fn exercise(scope: ReadFixture<'_>) {
    let branch_action = Uuid::new_v4();
    let foreign_action = Uuid::new_v4();
    let main_action = Uuid::new_v4();
    let bad_action = Uuid::new_v4();
    let live_action = Uuid::new_v4();
    let candidate_event_action = Uuid::new_v4();
    let correlation = scope.runtime.block_on(async {
        scope
            .cluster
            .connect("postgres")
            .await
            .unwrap()
            .query_one(
                "SELECT correlation_id FROM forge_agent.coding_tasks WHERE id=$1",
                &[&scope.task],
            )
            .await
            .unwrap()
            .get::<_, Uuid>(0)
    });
    let long = format!("é{}", "x".repeat(149998));
    scope.runtime.block_on(async {
        let admin=scope.cluster.connect("postgres").await.unwrap();
        admin.execute("INSERT INTO forge_agent.agent_branch_state(owner_id,project_alias,agent_profile,branch_name,branch_scope,reservation_status,active_scope_kind,active_scope_id,active_root_task_id,active_task_id,reserved_at,reservation_epoch) VALUES($1,'homelab','codex-author','feature/codex',$2,'active','native_workflow',$3,$4,$4,clock_timestamp(),1)",&[&scope.pg.owner,&forge_work_worker::agent_branch::branch_scope_for_branch("feature/codex").unwrap(),&scope.workflow,&scope.task]).await.unwrap();
        stage(&admin,&scope,StageFixture{action:branch_action,correlation,index:0,commit:&"a".repeat(40),pipeline:"branch",phase:"fixture_0",bytes:long.as_bytes(),bad_chunk:false}).await;
        stage(&admin,&scope,StageFixture{action:branch_action,correlation,index:1,commit:&"a".repeat(40),pipeline:"branch",phase:"fixture_1",bytes:long.as_bytes(),bad_chunk:false}).await;
        stage(&admin,&scope,StageFixture{action:foreign_action,correlation:Uuid::new_v4(),index:0,commit:&"a".repeat(40),pipeline:"branch",phase:"foreign",bytes:b"Never cross task ownership",bad_chunk:false}).await;
        stage(&admin,&scope,StageFixture{action:bad_action,correlation,index:0,commit:&"a".repeat(40),pipeline:"branch",phase:"bad_chunk",bytes:b"Integrity mismatch",bad_chunk:true}).await;
        // A genuine task-lineage binding to a *pending* fixture main
        // verification permits this action/commit only, without a CI pass.
        admin.execute("INSERT INTO forge_agent.delivery_ledger(verification_id,owner_id,project_alias,source_kind,workflow_id,root_task_id,transport_scope_id,correlation_id,commit_sha,tree_sha,lineage_id) VALUES($1,$2,'homelab','native',$3,$4,$3,$5,$6,$7,$8)",&[&main_action,&scope.pg.owner,&scope.workflow,&scope.task,&Uuid::new_v4(),&"c".repeat(40),&"d".repeat(40),&scope.lineage]).await.unwrap();
        admin.execute("INSERT INTO forge_agent.delivery_operations(verification_id,owner_id,project_alias,kind) VALUES($1,$2,'homelab','verify')",&[&main_action,&scope.pg.owner]).await.unwrap();
        stage(&admin,&scope,StageFixture{action:main_action,correlation:Uuid::new_v4(),index:0,commit:&"c".repeat(40),pipeline:"main",phase:"main_bound",bytes:b"Bound main console\n",bad_chunk:false}).await;
        stage(&admin,&scope,StageFixture{action:foreign_action,correlation:Uuid::new_v4(),index:1,commit:&"c".repeat(40),pipeline:"main",phase:"wrong_action",bytes:b"Never cross main action",bad_chunk:false}).await;
        stage(&admin,&scope,StageFixture{action:main_action,correlation:Uuid::new_v4(),index:1,commit:&"e".repeat(40),pipeline:"main",phase:"wrong_commit",bytes:b"Never cross main commit",bad_chunk:false}).await;
        for (sequence,action,kind,corr,commit,branch,branch_scope) in [
            (1_i64,main_action,"stage_started",Uuid::new_v4().to_string(),"c".repeat(40),"main","main".to_string()),
            (2,foreign_action,"build_started",Uuid::new_v4().to_string(),"c".repeat(40),"main","main".to_string()),
            (3,candidate_event_action,"build_started",Uuid::new_v4().to_string(),"c".repeat(40),"feature/codex",forge_work_worker::agent_branch::branch_scope_for_branch("feature/codex").unwrap()),
            (4,live_action,"stage_started",correlation.to_string(),"a".repeat(40),"feature/codex",forge_work_worker::agent_branch::branch_scope_for_branch("feature/codex").unwrap()),
        ] {
            let stage=(kind=="stage_started").then_some("check");
            admin.execute("INSERT INTO forge_agent.jenkins_events(owner_id,project_alias,sequence,jenkins_instance,job_name,delivery_id,event_kind,build_number,commit_sha,branch,summary,correlation_id,occurred_at,payload_sha256,action_id,branch_scope,stage_name) VALUES($1,'homelab',$2,'owned-fixture',$3,$4,$5,1,$6,$7,'Owned pending event fixture',$8,clock_timestamp(),$9,$10,$11,$12)",&[&scope.pg.owner,&sequence,&forge_work_publication::pipeline_projection::NATIVE_JENKINS_JOB,&format!("fixture.{action}.{sequence}"),&kind,&commit,&branch,&corr,&"b".repeat(64),&action,&branch_scope,&stage]).await.unwrap();
        }

    });
    let first = log(&scope, correlation, branch_action, "fixture_0", None, 3).unwrap();
    let page = &first["event"];
    assert_eq!(page["content"], "éx");
    assert_eq!(page["total_bytes"], 150000);
    assert_eq!(page["byte_offset"], 0);
    assert_eq!(page["truncated"], true);
    assert_eq!(page["tests_passed"], 7);
    assert_eq!(page["task_id"], scope.task.to_string());
    assert_eq!(page["correlation_id"], correlation.to_string());
    assert_eq!(page["action_id"], branch_action.to_string());
    let cursor = page["next_cursor"].as_str().unwrap().to_string();
    let digest = cursor.split('.').next().unwrap();
    let next = log(
        &scope,
        correlation,
        branch_action,
        "fixture_0",
        Some(cursor.clone()),
        3,
    )
    .unwrap();
    assert_eq!(next["event"]["content"], "xxx");
    assert_eq!(next["event"]["byte_offset"], 3);
    assert!(next["event"]["prev_cursor"].is_string());
    assert!(matches!(
        log(
            &scope,
            correlation,
            branch_action,
            "fixture_0",
            Some(format!("{}.3", "0".repeat(64))),
            3
        ),
        Err(WorkError::Rejected { status: 409, .. })
    ));
    for (offset, limit) in [(1, 3), (150001, 3), (0, 1), (0, 32769)] {
        assert!(matches!(
            log(
                &scope,
                correlation,
                branch_action,
                "fixture_0",
                Some(format!("{digest}.{offset}")),
                limit
            ),
            Err(WorkError::Rejected { status: 400, .. })
        ));
    }
    assert!(matches!(
        log(&scope, correlation, branch_action, "check", None, 32768),
        Err(WorkError::Rejected { status: 503, .. })
    )); // aggregate exceeds shipped browser bound
    assert!(log(&scope, correlation, branch_action, "fixture_1", None, 32768).is_ok()); // each bounded phase remains usable
    assert!(matches!(
        log(&scope, correlation, bad_action, "bad_chunk", None, 32768),
        Err(WorkError::Rejected { status: 503, .. })
    ));
    assert!(matches!(
        log(
            &scope,
            Uuid::new_v4(),
            main_action,
            "main_bound",
            None,
            32768
        ),
        Err(WorkError::Rejected { status: 403, .. })
    ));
    assert!(matches!(
        log(&scope, correlation, foreign_action, "foreign", None, 32768),
        Err(WorkError::NotFound { .. }) | Err(WorkError::Rejected { status: 404, .. })
    ));
    assert!(matches!(
        log(
            &scope,
            correlation,
            foreign_action,
            "wrong_action",
            None,
            32768
        ),
        Err(WorkError::NotFound { .. }) | Err(WorkError::Rejected { status: 404, .. })
    ));
    assert!(matches!(
        log(
            &scope,
            correlation,
            main_action,
            "wrong_commit",
            None,
            32768
        ),
        Err(WorkError::NotFound { .. }) | Err(WorkError::Rejected { status: 404, .. })
    ));
    assert_eq!(
        log(&scope, correlation, main_action, "main_bound", None, 32768).unwrap()["event"]
            ["content"],
        "Bound main console\n"
    );
    let store = PgWorkStore::new(
        PgSessionFactory::new(scope.pg.clone()),
        scope.pg.owner.clone(),
        vec!["homelab".into()],
        32,
        ReviewPolicy::agent_defaults(),
    );
    crate::work_common::run_read_tx(&store, |tx| {
        let rows = crate::work_projection_cicd::rows(
            tx,
            crate::work_projection_queries::JENKINS_EVENTS_FOR_FAMILY,
            &[
                &scope.pg.owner,
                &"homelab",
                &vec![correlation.to_string()],
                &vec!["c".repeat(40)],
                &Some(main_action),
                &Some("c".repeat(40)),
            ],
        )?;
        assert!(rows
            .iter()
            .any(|row| row["action_id"] == main_action.to_string()));
        assert!(rows
            .iter()
            .any(|row| row["action_id"] == candidate_event_action.to_string()));
        assert!(!rows
            .iter()
            .any(|row| row["action_id"] == foreign_action.to_string()));
        Ok(())
    })
    .unwrap();
    let (_, opened) = scope
        .service
        .dispatch(
            WorkDestination::Get,
            "fixture",
            WorkCommand::Get {
                project_alias: "homelab".into(),
                task_id: scope.task,
            },
            Some(1),
        )
        .unwrap();
    let cicd = &opened["event"]["task"]["correlation_cicd"];
    assert_eq!(
        cicd["branch_state"]["active_scope_id"],
        scope.workflow.to_string()
    );
    assert_eq!(cicd["waiting_on_branch_owner"], false);
    let runs = cicd["runs"].as_array().unwrap();
    assert!(runs
        .iter()
        .any(|run| run["action_id"] == main_action.to_string()));
    assert!(!runs
        .iter()
        .any(|run| run["action_id"] == foreign_action.to_string()));
    let live = runs
        .iter()
        .find(|run| run["action_id"] == live_action.to_string())
        .unwrap();
    assert_eq!(live["phase"], "running");
    assert!(live["started_at"]
        .as_str()
        .is_some_and(|stamp| stamp.ends_with('Z')));

    let (_, catalog) = scope
        .service
        .dispatch(
            WorkDestination::Catalog,
            "fixture",
            WorkCommand::Catalog,
            Some(1),
        )
        .unwrap();
    assert_eq!(catalog["event"]["kind"], "work_catalog");
    let standalone=forge_work::work_config::parse_work_config(&json!({"owner":scope.pg.owner,"allowed_projects":["homelab"],"allowed_modes":["plan","review","implement"],"profiles":{"codex-author":{"backend":"codex","model":"gpt-5","resource_profile":"forge-coding-v10","branch":"feature/codex"}}})).unwrap();
    let service = create_work_service(&standalone, scope.pg).unwrap();
    let command=forge_work::work_api::decode(WorkDestination::Submit,"fixture",&json!({"kind":"work_submit","request_id":"fixture","project_alias":"homelab","base_revision":"a".repeat(40),"mode":"implement","agent_profile":"codex-author","prompt":"Owned pending review read","acceptance_criteria":"Do not offer unverified acceptance","idempotency_key":"pending-review-read"})).unwrap();
    let (_, queued) = service
        .service
        .dispatch(WorkDestination::Submit, "fixture", command, None)
        .unwrap();
    let task = Uuid::parse_str(queued["event"]["task_id"].as_str().unwrap()).unwrap();
    let (_, detail) = service
        .service
        .dispatch(
            WorkDestination::Get,
            "fixture",
            WorkCommand::Get {
                project_alias: "homelab".into(),
                task_id: task,
            },
            None,
        )
        .unwrap();
    let review = &detail["event"]["task"]["review"];
    assert_eq!(review["stage"], "waiting_for_result");
    assert_eq!(review["required_hosted_green"], false);
    assert_eq!(review["can_accept"], false);
    assert_eq!(review["can_repair"], false);
}
