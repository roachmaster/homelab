#![cfg(feature = "test-support")]

use forge_mcp_db::app::{json_value, McpApp};
use forge_mcp_db::auth::{BearerTokenAuth, FailureLimiter, Principal};
use forge_mcp_db::backend::Backend;
use forge_mcp_db::config::load_config;
use forge_postgres::pg_fixture::{pg16_available, repo_root, DisposablePostgresCluster};
use serde_json::{json, Value};
use std::sync::Arc;

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
    exercise_agent_views(&cluster, &runtime, backend);
}

fn exercise_agent_views(
    cluster: &DisposablePostgresCluster,
    runtime: &tokio::runtime::Runtime,
    backend: Backend,
) {
    let setup = r#"INSERT INTO forge_agent.coding_tasks(owner_id,project_alias,id,idempotency_key,intent_digest,base_revision,mode,agent_profile,title,prompt,acceptance_criteria,pinned_recipe,recipe_digest,status,revision,review_required)
          VALUES ('fixture','homelab','ef9990ba-a143-4807-9fa3-c8c0302bc601','delivery-test',repeat('a',64),repeat('b',40),'plan','muse-db-primary','Fixture','Fixture prompt','Fixture acceptance',
          jsonb_build_object('version',2,'backend','muse','model','external-runner','resource_profile','forge-external-db-v1','mode','plan','generation_token_limit',NULL,'policy_digest',repeat('a',64)),repeat('a',64),'queued',1,false)"#;
    runtime
        .block_on(cluster.admin_exec(&[setup], false))
        .unwrap();
    let file = cluster.root.join("mcp.json");
    std::fs::write(&file, "{}").unwrap();
    let config = Arc::new(load_config(file.to_str().unwrap()).unwrap());
    let app = McpApp::new(
        config.clone(),
        Arc::new(BearerTokenAuth::new(&config.token_file)),
        backend,
        Arc::new(FailureLimiter::new(5, 60, 60, 60)),
    );
    let reader = Principal {
        token_id: "fixture".into(),
        name: "fixture".into(),
        scopes: ["read".into()].into(),
        subject: None,
    };
    let call = |name: &str, arguments: Value| {
        app.call_tool(&json!({"name":name,"arguments":arguments}), &reader)
    };
    let compact = |result: Value| -> Value {
        assert!(result.get("structuredContent").is_none());
        assert_ne!(result["isError"], true, "{result}");
        serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap()
    };
    let id = "ef9990ba-a143-4807-9fa3-c8c0302bc601";
    let brief = compact(call("get_task", json!({"id":id})));
    assert_eq!(brief.as_object().unwrap().len(), 4);
    assert_eq!(brief["title"], "Fixture");
    assert!(brief.get("prompt").is_none());
    let assignment = compact(call("get_task", json!({"id": id,"section":"assignment"})));
    assert_eq!(
        assignment,
        json!({"prompt":"Fixture prompt","acceptance_criteria":"Fixture acceptance"})
    );
    let metadata = call(
        "get_task",
        json!({"id":id,"section":"metadata","view":"structured"}),
    );
    assert_eq!(
        metadata["structuredContent"]["base_revision"],
        "b".repeat(40)
    );
    assert_eq!(metadata["content"], json!([]));
    let sql = "select n as item from generate_series(1,12) n order by n";
    let page = compact(call("query", json!({"sql":sql})));
    assert_eq!(page["rows"].as_array().unwrap().len(), 10);
    assert_eq!(page["columns"], json!(["item"]));
    let end = compact(call(
        "query",
        json!({"sql":sql,"cursor":page["next_cursor"]}),
    ));
    assert_eq!(end, json!({"columns":["item"],"rows":[[11],[12]]}));
    let full = call("query", json!({"sql":sql,"view":"full"}));
    assert_eq!(full["structuredContent"]["rows"][0], json!({"item":1}));
    assert_eq!(full["structuredContent"]["row_count"], 12);
    assert_eq!(
        serde_json::from_str::<Value>(full["content"][0]["text"].as_str().unwrap()).unwrap(),
        full["structuredContent"]
    );
    assert_eq!(
        call("get_task", json!({"id":id,"section":"private"}))["isError"],
        true
    );
    assert_eq!(
        call("query", json!({"sql":sql,"view":false}))["isError"],
        true
    );
    let catalog = compact(call(
        "list_tables",
        json!({"schema":"forge_agent","page_size":1}),
    ));
    assert_eq!(catalog["columns"], json!(["table_schema", "table_name"]));
    assert!(catalog["next_cursor"].is_string());
    let description = call(
        "describe_table",
        json!({"schema":"forge_agent","table":"coding_tasks","view":"structured"}),
    );
    assert_eq!(description["content"], json!([]));
    assert_eq!(
        description["structuredContent"]["columns"],
        json!(["column_name", "data_type"])
    );
    let full_description = call(
        "describe_table",
        json!({"schema":"forge_agent","table":"coding_tasks","view":"full"}),
    );
    assert_eq!(
        full_description["structuredContent"]["columns"],
        json!(["column_name", "data_type", "is_nullable", "column_default"])
    );
    let wrong_cursor = call(
        "query",
        json!({"sql":"SELECT 1","cursor":page["next_cursor"]}),
    );
    assert_eq!(wrong_cursor["isError"], true);
    let duplicate = compact(call("query", json!({"sql":"select 1 as n, 2 as n"})));
    assert_eq!(duplicate, json!({"columns":["n","n"],"rows":[[1,2]]}));
    let large = call(
        "query",
        json!({"sql":"select array_fill(repeat('x',1000),array[20])"}),
    );
    assert_eq!(large["isError"], true);
}
