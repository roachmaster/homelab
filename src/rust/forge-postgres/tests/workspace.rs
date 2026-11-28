#![cfg(feature = "test-support")]
use forge_postgres::{
    pg_fixture::{pg16_available, DisposablePostgresCluster},
    workspace_store::dispatch,
    PgConfig,
};
use serde_json::{json, Value};

#[test]
fn existing_tabs_round_trip_with_lease_rotation_and_shared_history_isolation() {
    if !pg16_available() {
        assert!(std::env::var("FORGE_REQUIRE_DISPOSABLE_POSTGRES").as_deref() != Ok("1"));
        return;
    }
    let cluster = DisposablePostgresCluster::start().unwrap();
    let pg = PgConfig {
        host: cluster.root.display().to_string(),
        user: "forge_agent".into(),
        ..PgConfig::new("mission-control")
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(cluster.admin_exec(&[
        "INSERT INTO forge_agent.ide_workspace_history(owner_id,revision,transcripts,chat_history_active) VALUES ('mission-control',7,'[]',true)",
        "INSERT INTO forge_agent.ide_workspace_tabs(owner_id,tab_id,lease,base_history_revision,base_history_fingerprint,snapshot,chat_history_enabled) VALUES ('mission-control',repeat('0',32),repeat('3',64),4,repeat('a',64),'{\"chat\":{\"selected\":0,\"conversations\":[{\"draft\":\"old draft\",\"messages\":[]}]},\"workbench\":null,\"workbench_open\":false}',true)"
    ],true)).unwrap();
    let tab = "0".repeat(32);
    let restore = |t: &str| json!({"operation":"restore","tab":t,"preserve_chat":true});
    let first = dispatch(&pg, &restore(&tab)).unwrap();
    let lease = first["lease"].as_str().unwrap();
    let mut snap = first["snapshot"].clone();
    assert_eq!(snap["chat"]["conversations"][0]["draft"], "old draft");
    snap["workbench_open"] = true.into();
    let save = |lease: &str, rev: u32, snap: &Value| json!({"operation":"save","tab":tab,"lease":lease,"revision":rev,"snapshot":snap,"preserve_chat":true});
    assert_eq!(dispatch(&pg, &save(lease, 2, &snap)).unwrap(), json!({}));
    let mut stale = snap.clone();
    stale["workbench_open"] = false.into();
    dispatch(&pg, &save(lease, 1, &stale)).unwrap();
    let mut changed = snap.clone();
    changed["chat"]["conversations"][0]["draft"] = "replacement".into();
    assert_eq!(
        dispatch(&pg, &save(lease, 1, &changed)).unwrap_err(),
        forge_work::WorkError::rejected(409, "workspace_chat_changed")
    );
    let second = dispatch(&pg, &restore(&tab)).unwrap();
    assert_eq!(second["snapshot"], snap);
    assert_ne!(second["lease"], lease);
    assert_eq!(
        dispatch(&pg, &save(lease, 3, &snap)).unwrap_err(),
        forge_work::WorkError::rejected(401, "workspace_lease")
    );
    let check=rt.block_on(async{
        let client=cluster.connect("forge_agent").await.unwrap();
        client.query_one("SELECT h.revision,t.base_history_revision,t.base_history_fingerprint,t.chat_history_enabled FROM forge_agent.ide_workspace_tabs t JOIN forge_agent.ide_workspace_history h USING(owner_id)",&[]).await.unwrap()
    });
    assert_eq!(check.get::<_, i64>(0), 7);
    assert_eq!(check.get::<_, i64>(1), 4);
    assert_eq!(check.get::<_, String>(2), "a".repeat(64));
    assert!(!check.get::<_, bool>(3));
    for i in 1..=12 {
        assert!(dispatch(&pg, &restore(&format!("{i:032x}"))).unwrap()["snapshot"].is_null())
    }
    assert!(dispatch(&pg, &restore(&tab)).unwrap()["snapshot"].is_null());
}
