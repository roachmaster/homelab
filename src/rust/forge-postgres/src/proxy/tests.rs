use super::*;
use forge_core::provider::Factory;
use serde_json::{json, Value};
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};
struct ConfigFixture {
    root: PathBuf,
    value: Value,
}
impl ConfigFixture {
    fn new(host: &str) -> Self {
        let root = std::env::temp_dir().join(format!("forge-proxy-unit-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let mut callers = serde_json::Map::new();
        for (i, name) in [
            "coordinator",
            "worker",
            "reconciler",
            "gpu",
            "muse",
            "operator",
        ]
        .iter()
        .enumerate()
        {
            let p = root.join(name);
            fs::write(&p, format!("{:064x}", i + 1)).unwrap();
            fs::set_permissions(&p, fs::Permissions::from_mode(0o600)).unwrap();
            callers.insert((*name).to_owned(), json!({"owner":"alice","token_file":p}));
        }
        Self {
            root,
            value: json!({"db":{"host":host,"port":5432,"dbname":"forge_agent"},"listen":{"host":"127.0.0.1","port":0},"callers":callers}),
        }
    }
    fn ready(&self) -> Proxy {
        ProxyFactory::new(ProxyConfig::parse(&self.value).unwrap())
            .create()
            .unwrap()
    }
}
impl Drop for ConfigFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
#[test]
fn configuration_registration_is_effect_free_and_cannot_widen() {
    let mut f = ConfigFixture::new("/nonexistent");
    f.value["callers"]["gpu"]["token_file"] = json!("/does/not/exist");
    assert!(register_proxy(ProxyConfig::parse(&f.value).unwrap())
        .info()
        .requires()
        .is_empty());
    f.value["callers"]["gpu"]["allowlist"] = json!({"fn":["forge_agent.coding_identity_register"]});
    assert!(matches!(
        ProxyConfig::parse(&f.value),
        Err(ProxyError::Config)
    ));
}
#[test]
fn auth_is_private_duplicate_credentials_and_unsafe_files_rejected() {
    let f = ConfigFixture::new("/none");
    let p = f.ready();
    assert!(p.authenticate("wrong").is_err());
    let c = p.authenticate(&format!("{:064x}", 1)).unwrap();
    assert_eq!(c.name(), "coordinator");
    let copy = c.clone();
    assert_eq!(
        p.operation(&copy, "read.forge_agent.coding_tasks", &json!({})),
        Err(ProxyError::Forbidden)
    );
    drop(p);
    fs::write(f.root.join("worker"), format!("{:064x}", 1)).unwrap();
    assert!(matches!(
        ProxyFactory::new(ProxyConfig::parse(&f.value).unwrap()).create(),
        Err(ProxyError::Config)
    ));
    fs::set_permissions(
        f.root.join("coordinator"),
        fs::Permissions::from_mode(0o666),
    )
    .unwrap();
    assert!(matches!(
        ProxyFactory::new(ProxyConfig::parse(&f.value).unwrap()).create(),
        Err(ProxyError::Config)
    ));
}
#[test]
fn closed_catalog_owner_scope_defaults_and_redaction() {
    let f = ConfigFixture::new("/none");
    let p = f.ready();
    let c = p.authenticate(&format!("{:064x}", 1)).unwrap();
    assert_eq!(catalog()["func_catalog"].as_object().unwrap().len(), 100);
    assert_eq!(catalog()["read_queries"].as_object().unwrap().len(), 81);
    assert!(OperationPlan::prepare(
        c,
        "read.forge_agent.coding_tasks",
        &json!({"filter":{"owner_id":"bob"}})
    )
    .is_err());
    assert!(OperationPlan::prepare(
        c,
        "read.forge_agent.coding_tasks",
        &json!({"order":"id;DELETE"})
    )
    .is_err());
    assert!(
        OperationPlan::prepare(c, "read.forge_agent.coding_tasks", &json!({"order":[{}]})).is_err()
    );
    assert!(OperationPlan::prepare(
        c,
        "read.forge_agent.coding_tasks",
        &json!({"filter":{"status":{}}})
    )
    .is_err());
    let r = p.authenticate(&format!("{:064x}", 3)).unwrap();
    let omitted = OperationPlan::prepare(
        r,
        "fn.forge_coding_reconciler_api.pending_reconciliations",
        &json!({}),
    )
    .unwrap();
    assert!(omitted.sql.ends_with("pending_reconciliations())"));
    let null = OperationPlan::prepare(
        r,
        "fn.forge_coding_reconciler_api.pending_reconciliations",
        &json!({"p_limit":null}),
    )
    .unwrap();
    assert!(null.sql.contains("p_limit =>"));
    assert_eq!(null.params, vec![None]);
    let plan=OperationPlan::prepare(c,"op.ide_chat_insert",&json!({"owner_id":"alice","id":uuid::Uuid::new_v4().to_string(),"title":"hello","messages":[],"revision":1})).unwrap();
    assert_eq!(plan.audit_args["messages"], json!({"redacted":true}));
    assert!(!plan.sql.contains("hello"));
    let g = p.authenticate(&format!("{:064x}", 4)).unwrap();
    assert!(OperationPlan::prepare(g, "notify.forge_agent_jobs", &json!({})).is_err());
    assert!(OperationPlan::prepare(g, "fn.forge_agent_worker_api.claim", &json!({})).is_err());
}
#[cfg(feature = "test-support")]
#[test]
fn actual_postgres_atomic_audit_scoped_reads_notifications_and_http() {
    use crate::pg_fixture::{pg16_available, DisposablePostgresCluster};
    use std::sync::Arc;
    if !pg16_available() {
        assert_ne!(
            std::env::var("FORGE_REQUIRE_DISPOSABLE_POSTGRES").as_deref(),
            Ok("1")
        );
        return;
    }
    let cluster = DisposablePostgresCluster::start().unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let f = ConfigFixture::new(cluster.root.to_str().unwrap());
    let proxy = Arc::new(f.ready());
    assert!(proxy.health());
    // Compile every permitted catalog operation as its actual role, without executing writes.
    rt.block_on(async {
        for (caller, _) in &proxy.config.callers {
            let admin = cluster.connect(&caller.role).await.unwrap();
            for kind in ["fn", "op", "readq", "read"] {
                for target in &caller.allowed[kind] {
                    let mut args = serde_json::Map::new();
                    let spec = match kind {
                        "fn" => catalog()["func_catalog"][target].as_array(),
                        "op" => catalog()["entry_points"][target][0].as_array(),
                        "readq" => catalog()["read_queries"][target][1].as_array(),
                        _ => None,
                    };
                    if let Some(spec) = spec {
                        for p in spec {
                            args.insert(p[0].as_str().unwrap().to_owned(), Value::Null);
                        }
                    }
                    let operation = format!("{kind}.{target}");
                    let plan =
                        OperationPlan::prepare(caller, &operation, &Value::Object(args)).unwrap();
                    if let Err(e) = admin.prepare(&plan.sql).await {
                        panic!(
                            "catalog SQL preparation rejected: {} role={} code={:?}",
                            operation,
                            caller.role,
                            e.code()
                        );
                    }
                }
            }
        }
    });
    let c = proxy.authenticate(&format!("{:064x}", 1)).unwrap();
    assert_eq!(proxy.operation(c,"readq.deadline_retry_actions",&json!({"workflow_id":uuid::Uuid::new_v4().to_string(),"owner_id":"alice","project_alias":"homelab","policy":"fixture","task_id":uuid::Uuid::new_v4().to_string(),"before":"2026-12-01T00:00:00Z"})).unwrap(),json!({"rows":[]}));
    assert_eq!(
        proxy
            .operation(
                c,
                "readq.worker_readiness_fresh",
                &json!({"owner_id":"alice","policy_digests":["fixture"]})
            )
            .unwrap(),
        json!({"rows":[]})
    );
    assert!(proxy
        .operation(
            c,
            "readq.worker_readiness_fresh",
            &json!({"owner_id":"bob","policy_digests":[]})
        )
        .is_err());
    proxy
        .operation(
            c,
            "op.ide_workspace_history_insert",
            &json!({"owner_id":"alice"}),
        )
        .unwrap();
    // The native transport exposes the existing date defect; M57 fixes only that cast.
    let chat_id = uuid::Uuid::new_v4().to_string();
    let chat_args = json!({"owner_id":"alice","id":chat_id,"expires_on":"2026-12-01","time_zone":"UTC","expires_at":"2026-12-02T00:00:00Z","title":"date fixture","messages":[{"role":"user","content":"fixture"}]});
    assert_eq!(
        proxy
            .operation(c, "op.ide_chat_insert", &chat_args)
            .unwrap()["expires_on"],
        "2026-12-01"
    );
    let signature =
        "forge_pg_proxy_api.ide_chat_insert(text,uuid,text,jsonb,bigint,text,text,text)";
    let definition = rt.block_on(async {
        cluster
            .connect("postgres")
            .await
            .unwrap()
            .query_one(
                "SELECT pg_get_functiondef($1::text::regprocedure)",
                &[&signature],
            )
            .await
            .unwrap()
            .get::<_, String>(0)
    });
    let old = definition.replace(
        "p_expires_on::date, p_time_zone",
        "p_expires_on, p_time_zone",
    );
    assert_ne!(old, definition);
    rt.block_on(async {
        cluster
            .connect("postgres")
            .await
            .unwrap()
            .batch_execute(&old)
            .await
            .unwrap();
    });
    assert_eq!(
        proxy.operation(
            c,
            "op.ide_chat_insert",
            &json!({"owner_id":"alice","id":uuid::Uuid::new_v4().to_string()})
        ),
        Err(ProxyError::Database)
    );
    let verifier = crate::pg_fixture::sql_dir().join("management/verify-pg-proxy-chat-date.sql");
    assert_ne!(cluster.run_sql_script(&verifier, "postgres").unwrap().0, 0);
    rt.block_on(async {
        let admin = cluster.connect("postgres").await.unwrap();
        admin
            .batch_execute("SET ROLE forge_agent_owner")
            .await
            .unwrap();
        admin
            .batch_execute(include_str!(
                "../../../../../deploy/mission-control/sql/migrations/0057_pg_proxy_chat_date.sql"
            ))
            .await
            .unwrap();
    });
    assert_eq!(cluster.run_sql_script(&verifier, "postgres").unwrap().0, 0);
    cluster.apply_and_verify().unwrap();
    let id = uuid::Uuid::new_v4();
    let args = json!({"owner_id":"alice","id":id.to_string(),"title":"secret content"});
    assert_eq!(
        forge_core::io_client::IoClient::execute(
            &*proxy,
            ProxyCommand {
                bearer: format!("{:064x}", 1),
                operation: "op.ide_conversation_insert".to_owned(),
                args: args.clone()
            }
        )
        .unwrap()["id"],
        json!(id.to_string())
    );
    rt.block_on(async{let admin=cluster.connect("postgres").await.unwrap();let row=admin.query_one("SELECT actor_kind,actor_id,db_role,target FROM forge_pg_proxy.audit ORDER BY recorded_at DESC LIMIT 1",&[]).await.unwrap();assert_eq!(row.get::<_,String>(0),"coordinator");assert_eq!(row.get::<_,String>(1),"coordinator");assert_eq!(row.get::<_,String>(2),"forge_agent");assert!(!row.get::<_,Value>(3).to_string().contains("secret content"));admin.batch_execute("INSERT INTO forge_agent.conversations(owner_id,id,title) VALUES('bob','123e4567-e89b-42d3-a456-426614174000','foreign')").await.unwrap();});
    let rows = proxy
        .operation(c, "read.forge_agent.conversations", &json!({"order":"id"}))
        .unwrap();
    assert_eq!(rows["rows"].as_array().unwrap().len(), 1);
    assert_eq!(rows["rows"][0]["owner_id"], "alice");
    assert_eq!(
        proxy
            .operation(c, "read.forge_agent.conversations.count", &json!({}))
            .unwrap(),
        json!({"count":1})
    );
    assert_eq!(
        proxy
            .operation(
                c,
                "read.forge_agent.conversations.exists",
                &json!({"filter":{"id":[id.to_string()]}})
            )
            .unwrap(),
        json!({"exists":true})
    );
    let rec = proxy.authenticate(&format!("{:064x}", 3)).unwrap();
    assert!(proxy
        .operation(
            rec,
            "fn.forge_coding_reconciler_api.pending_reconciliations",
            &json!({})
        )
        .is_ok());
    rt.block_on(async{cluster.connect("postgres").await.unwrap().batch_execute("REVOKE EXECUTE ON FUNCTION forge_pg_proxy_api.audit(text,text,text,jsonb) FROM forge_agent").await.unwrap();});
    let failed = uuid::Uuid::new_v4();
    assert_eq!(
        proxy.operation(
            c,
            "op.ide_conversation_insert",
            &json!({"owner_id":"alice","id":failed.to_string()})
        ),
        Err(ProxyError::Database)
    );
    rt.block_on(async{let admin=cluster.connect("postgres").await.unwrap();assert_eq!(admin.query_one("SELECT count(*) FROM forge_agent.conversations WHERE id=$1",&[&failed]).await.unwrap().get::<_,i64>(0),0);admin.batch_execute("GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.audit(text,text,text,jsonb) TO forge_agent").await.unwrap();});
    let hub = Notifications::start(&proxy).unwrap();
    assert_eq!(
        proxy
            .operation(c, "notify.forge_agent_jobs", &json!({}))
            .unwrap(),
        json!({"notified":"forge_agent_jobs"})
    );
    let g = proxy.authenticate(&format!("{:064x}", 4)).unwrap();
    assert_eq!(
        hub.wait(g, "forge_agent_jobs", 2.0).unwrap()["notifications"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        hub.wait(c, "forge_agent_jobs", 2.0).unwrap()["notifications"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(hub.wait(g, "forge_coding_jobs", 0.0).is_err());
    assert_eq!(
        hub.wait(g, "forge_agent_jobs", 0.0).unwrap(),
        json!({"notifications":[]})
    );
    drop(hub);
    let mut server = service::ProxyServer::start(proxy.clone()).unwrap();
    use std::io::{Read, Write};
    let send = |headers: &str, body: &str| {
        let mut s = std::net::TcpStream::connect(server.local_addr()).unwrap();
        s.set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        write!(s,"POST /v1/operation HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\n{headers}Content-Length: {}\r\n\r\n{body}",body.len()).unwrap();
        let mut response = String::new();
        s.read_to_string(&mut response).unwrap();
        response
    };
    assert!(send("", "{}").starts_with("HTTP/1.1 401"));
    let auth = format!("Authorization: Bearer {:064x}\r\n", 1);
    assert!(send(&(auth.clone() + &auth), "{}").starts_with("HTTP/1.1 401"));
    assert!(send(
        &auth,
        "{\"operation\":\"read.forge_agent.conversations.count\",\"args\":{}}"
    )
    .contains("\"count\":1"));
    assert!(send(&auth, "{\"operation\":\"x\",\"operation\":\"y\"}").starts_with("HTTP/1.1 400"));
    assert!(send(
        &auth,
        "{\"operation\":\"read.forge_agent.conversations\",\"owner\":\"bob\"}"
    )
    .starts_with("HTTP/1.1 400"));
    server.shutdown();
    assert_eq!(
        proxy.operation(c, "read.forge_agent.conversations", &json!({})),
        Err(ProxyError::Closed)
    );
}

#[test]
fn explicit_passfile_is_private_complete_and_handles_escaped_delimiters() {
    let mut f = ConfigFixture::new("/var/run/postgresql");
    let path = f.root.join("passwords");
    let roles = [
        "forge_agent",
        "forge_coding_worker",
        "forge_coding_reconciler",
        "forge_gpu_worker",
        "muse_proxy_writer",
        "forge_pg_proxy",
    ];
    let raw = roles
        .iter()
        .map(|r| format!("localhost:5432:forge_agent:{r}:fixture\\:password\\\\suffix\n"))
        .collect::<String>();
    fs::write(&path, raw).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    f.value["db"]["password_file"] = json!(path);
    let config = ProxyConfig::parse(&f.value).unwrap();
    let passwords = super::config::passwords(&config).unwrap();
    assert_eq!(passwords.len(), 6);
    assert_eq!(passwords["forge_agent"], "fixture:password\\suffix");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
    assert!(super::config::passwords(&config).is_err());
}

#[test]
fn narrowed_permissions_reject_before_connection_and_role_inventory_stays_closed() {
    let mut f = ConfigFixture::new("/no/database");
    f.value["callers"]["worker"]["allowlist"] = json!({"fn":[]});
    let p = f.ready();
    let worker = p.authenticate(&format!("{:064x}", 2)).unwrap();
    assert_eq!(
        p.operation(worker, "fn.forge_coding_worker_api.claim", &json!({})),
        Err(ProxyError::Forbidden)
    );
    for name in [
        "forge_agent_worker_api.lock_attempt",
        "forge_coding_worker_api.queue_review",
        "forge_external_agent_api.artifact",
        "forge_external_agent_api.capabilities",
    ] {
        assert!(p.config.callers.iter().all(|(c, _)| !c.allows("fn", name)));
    }
    let unscoped: std::collections::BTreeSet<_> = catalog()["read_queries"]
        .as_object()
        .unwrap()
        .iter()
        .filter(|(_, v)| {
            !v[1].as_array().unwrap().iter().any(|p| {
                p[0].as_str()
                    .is_some_and(|s| s == "owner_id" || s.ends_with("_owner_id"))
            })
        })
        .map(|(k, _)| k.as_str())
        .collect();
    assert_eq!(
        unscoped,
        std::collections::BTreeSet::from([
            "author_handoff_available",
            "gpu_ready_models",
            "gpu_worker_ready"
        ])
    );
}
#[test]
fn linked_credentials_are_refused() {
    let f = ConfigFixture::new("/none");
    let original = f.root.join("coordinator");
    let moved = f.root.join("saved");
    fs::rename(&original, &moved).unwrap();
    std::os::unix::fs::symlink(&moved, &original).unwrap();
    assert!(matches!(
        ProxyFactory::new(ProxyConfig::parse(&f.value).unwrap()).create(),
        Err(ProxyError::Config)
    ));
    fs::remove_file(&original).unwrap();
    fs::hard_link(&moved, &original).unwrap();
    assert!(matches!(
        ProxyFactory::new(ProxyConfig::parse(&f.value).unwrap()).create(),
        Err(ProxyError::Config)
    ));
}

#[test]
fn token_modes_allow_service_group_only_and_fifo_never_blocks() {
    let f = ConfigFixture::new("/none");
    let token = f.root.join("coordinator");
    fs::set_permissions(&token, fs::Permissions::from_mode(0o640)).unwrap();
    assert!(super::config::token_hash(&token).is_ok());
    for mode in [0o644, 0o666] {
        fs::set_permissions(&token, fs::Permissions::from_mode(mode)).unwrap();
        assert_eq!(super::config::token_hash(&token), Err(ProxyError::Config));
    }
    fs::remove_file(&token).unwrap();
    nix::unistd::mkfifo(
        &token,
        nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
    )
    .unwrap();
    assert_eq!(super::config::token_hash(&token), Err(ProxyError::Config));
}
