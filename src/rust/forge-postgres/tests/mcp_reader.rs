#![cfg(feature = "test-support")]

use forge_mcp_db::app::json_value;
use forge_mcp_db::backend::Backend;
use forge_postgres::pg_fixture::{pg16_available, repo_root, DisposablePostgresCluster};
use serde_json::json;

#[test]
fn mcp_reader_keeps_exact_values_across_pages_and_database_denies_writes() {
    if !pg16_available() {
        assert!(
            std::env::var("FORGE_REQUIRE_DISPOSABLE_POSTGRES").as_deref() != Ok("1"),
            "Main requires the disposable PostgreSQL reader"
        );
        return;
    }
    let cluster = DisposablePostgresCluster::start().unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(cluster.admin_exec(&[
        "CREATE ROLE homelab_mcp LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT NOREPLICATION CONNECTION LIMIT 8",
        "ALTER ROLE homelab_mcp SET default_transaction_read_only=on",
        "ALTER ROLE homelab_mcp SET statement_timeout='5s'"
    ],false)).unwrap();
    let (status, _, stderr) = cluster
        .run_sql_script(
            &repo_root().join("deploy/mission-control/mcp-postgres/sql/grant-mcp-readonly.sql"),
            "postgres",
        )
        .unwrap();
    assert_eq!(status, 0, "{stderr}");
    let backend = Backend::new(
        format!(
            "host={} dbname=forge_agent user=homelab_mcp",
            cluster.root.display()
        ),
        5000,
    );
    let query="SELECT g, 61.70::numeric, interval '3 days', array[[1.25::numeric,2],[NULL,4]], '16:30:00-06'::timetz FROM generate_series(1,3) g ORDER BY g";
    let first = backend.run(query, &[], 2, 0, true).unwrap();
    let last = backend.run(query, &[], 2, 2, true).unwrap();
    assert!(first.has_more);
    assert!(!last.has_more);
    let cells: Vec<_> = last.rows[0].iter().map(|c| json_value(c, 4096)).collect();
    assert_eq!(
        json!(cells),
        json!([
            3,
            "61.70",
            "3 days, 0:00:00",
            [["1.25", "2"], [null, "4"]],
            "16:30:00-06:00"
        ])
    );
    assert!(backend
        .run(
            "SELECT count(*) FROM forge_agent.coding_tasks",
            &[],
            1,
            0,
            true
        )
        .is_ok());
    // Use RETURNING so the backend executes the read-only role's denied statement.
    assert!(backend
        .run(
            "DELETE FROM forge_agent.coding_tasks WHERE false RETURNING task_id",
            &[],
            1,
            0,
            true
        )
        .is_err());
}
