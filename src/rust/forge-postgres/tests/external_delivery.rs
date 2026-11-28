#![cfg(feature = "test-support")]
use forge_postgres::{
    external_delivery_adapter::PgExternalDeliveryStore,
    pg_fixture::{pg16_available, DisposablePostgresCluster},
    PgConfig,
};
use forge_work_publication::external_delivery::{DeliveryStoreError, ExternalDeliveryStorePort};
use serde_json::json;

#[test]
fn external_delivery_claim_renew_checkpoint_and_replay_use_scoped_role() {
    if !pg16_available() {
        assert!(std::env::var("FORGE_REQUIRE_DISPOSABLE_POSTGRES").as_deref() != Ok("1"));
        return;
    }
    let cluster = DisposablePostgresCluster::start().unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(cluster.admin_exec(&[
        "INSERT INTO forge_external_workflow.delivery_grants(worker_role,owner_id,project_alias,enabled) VALUES ('forge_coding_reconciler','fixture','homelab',true)",
        r#"INSERT INTO forge_agent.coding_tasks(owner_id,project_alias,id,idempotency_key,intent_digest,base_revision,mode,agent_profile,title,prompt,acceptance_criteria,pinned_recipe,recipe_digest,status,revision,review_required)
          VALUES ('fixture','homelab','ef9990ba-a143-4807-9fa3-c8c0302bc601','delivery-test',repeat('a',64),repeat('b',40),'plan','muse-db-primary','Fixture','Fixture prompt','Fixture acceptance',
          jsonb_build_object('version',2,'backend','muse','model','external-runner','resource_profile','forge-external-db-v1','mode','plan','generation_token_limit',NULL,'policy_digest',repeat('a',64)),repeat('a',64),'queued',1,false)"#,
        "INSERT INTO forge_external_workflow.workflows(workflow_id,owner_id,project_alias,root_task_id,active_task_id,author_profile,reviewer_profile,policy_digest,snapshot,phase) SELECT 'ed44ba76-c5e3-4165-b7dc-77fe715acfe1',owner_id,project_alias,id,id,agent_profile,'codex-reviewer',repeat('a',64),'{}','cancelled' FROM forge_agent.coding_tasks",
        "INSERT INTO forge_external_workflow.actions(workflow_id,owner_id,project_alias,workflow_revision,kind,source_key,intent) SELECT workflow_id,owner_id,project_alias,revision,'cancel','fixture','{}' FROM forge_external_workflow.workflows"
    ],false)).unwrap();
    let pg = PgConfig {
        host: cluster.root.display().to_string(),
        user: "forge_coding_reconciler".into(),
        ..PgConfig::new("fixture")
    };
    let mut store = PgExternalDeliveryStore::new(pg.clone(), "fixture").unwrap();
    let lease = store.claim("fixture-worker").unwrap().unwrap();
    assert_eq!(lease.kind, "cancel");
    assert!(store.claim("another-worker").unwrap().is_none());
    assert_eq!(store.renew(&lease).unwrap(), true);
    let mut wrong = lease.clone();
    wrong.token = uuid::Uuid::new_v4().to_string();
    assert!(matches!(
        store.renew(&wrong),
        Err(DeliveryStoreError::Rejected(409, _))
    ));
    let receipt =
        json!({"status":"cancelled","summary":"Fixture cancellation","receipt":{"fixture":true}});
    assert_eq!(store.checkpoint(&lease, &receipt).unwrap(), receipt);
    assert_eq!(store.checkpoint(&lease, &receipt).unwrap(), receipt);
    let different = json!({"status":"cancelled","summary":"Different","receipt":{}});
    assert!(matches!(
        store.checkpoint(&lease, &different),
        Err(DeliveryStoreError::Rejected(409, _))
    ));
    assert!(store.claim("fixture-worker").unwrap().is_none());
    let mut foreign = PgExternalDeliveryStore::new(pg, "another-owner").unwrap();
    assert!(foreign.claim("fixture-worker").is_err());
}
