//! Live smoke test for the disposable cluster fixture: skipped
//! unless PG16 is available (non-root, not disabled), exactly like
//! the Python suites' PG16_AVAILABLE gate.
#![cfg(feature = "test-support")]

use forge_postgres::pg_fixture::{pg16_available, DisposablePostgresCluster};

#[test]
fn cluster_applies_schema_and_grants() {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(async {
    if !pg16_available() {
        eprintln!("skipping: disposable PostgreSQL 16 unavailable");
        return;
    }
    let mut cluster = DisposablePostgresCluster::start().expect("cluster starts");
    let client = cluster.connect("forge_agent").await.expect("connects");
    let row = client
        .query_one("SELECT count(*) AS n FROM forge_agent.coding_tasks", &[])
        .await
        .expect("coding_tasks exists after apply");
    let count: i64 = row.get("n");
    assert_eq!(count, 0);
    cluster
        .grant_worker("owner-1", "homelab", "codex-primary", &["plan"], "forge_coding_worker")
        .await
        .expect("grant inserts");
    cluster.reset_coding_tables().await.expect("reset works");
    cluster.stop();
    });
}
