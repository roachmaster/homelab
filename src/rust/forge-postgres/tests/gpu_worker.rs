#![cfg(feature = "test-support")]
use forge_postgres::gpu_adapter::DirectGpuStore;
use forge_postgres::{
    pg_fixture::{pg16_available, DisposablePostgresCluster},
    PgConfig,
};
use forge_work_worker::{
    gpu::{ForgeGpuWorker, GpuStorePort, HttpModelAdapter},
    runner::StopFlag,
};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::time::{Duration, Instant};
const MODEL: &str = "gpt-oss-20b";
const RUN: &str = "edc1657c-be09-4bf4-a359-314e8b22a3a0";

#[test]
fn queued_inference_reaches_loopback_model_then_publishes_a_fenced_result() {
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
        "INSERT INTO forge_agent.conversations(id,owner_id) VALUES ('8fd41739-72f6-4264-967e-b8e19e79a218','mission-control')",
        "INSERT INTO forge_agent.runs(id,conversation_id,request_id,model_id,distributed,run_deadline,next_attempt_at) SELECT 'edc1657c-be09-4bf4-a359-314e8b22a3a0',id,'fixture','gpt-oss-20b',true,now()+interval '2 minutes',now() FROM forge_agent.conversations",
        "INSERT INTO forge_agent.run_events(run_id,sequence,event_type,data) SELECT id,1,'queued','{\"max_tokens\":32,\"temperature\":0.0,\"allow_partial\":false}' FROM forge_agent.runs",
        "INSERT INTO forge_agent.messages(conversation_id,position,run_id,role,content) SELECT conversation_id,1,id,'user','fixture prompt' FROM forge_agent.runs",
    ],false)).unwrap();
    let cfg = PgConfig {
        host: cluster.root.display().to_string(),
        user: "forge_gpu_worker".into(),
        ..PgConfig::new("mission-control")
    };
    let mut guard = DirectGpuStore::new(cfg.clone());
    guard.advertise(MODEL, true).unwrap();
    assert_eq!(
        guard.next_model(&[MODEL.into()]).unwrap().as_deref(),
        Some(MODEL)
    );
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(true).unwrap();
    let stop = StopFlag::new();
    let shutdown = stop.clone();
    let server = std::thread::spawn(move || {
        let mut completions = 0;
        while !shutdown.is_set() {
            let (mut socket, _) = match listener.accept() {
                Ok(client) => client,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                }
                Err(e) => panic!("{e}"),
            };
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let completion = line.starts_with("POST /v1/chat/completions ");
            assert!(completion || line.starts_with("GET /v1/runtime "));
            let mut length = 0;
            let mut auth = false;
            loop {
                line.clear();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                let (k, v) = line.trim_end().split_once(':').unwrap();
                if k.eq_ignore_ascii_case("Authorization") {
                    auth = v.trim() == "Bearer fixture-model";
                }
                if k.eq_ignore_ascii_case("Content-Length") {
                    length = v.trim().parse().unwrap();
                }
            }
            assert!(auth);
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let reply = if completion {
                let doc: Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(doc["messages"][0]["content"], "fixture prompt");
                completions += 1;
                json!({"model":MODEL,"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":"fixture answer"}}],"usage":{"total_tokens":3}})
            } else {
                json!({"model":MODEL,"state":"ready"})
            };
            write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                reply.to_string().len()
            )
            .unwrap();
        }
        completions
    });
    let shutdown = stop.clone();
    let worker = std::thread::spawn(move || {
        ForgeGpuWorker::new(
            Box::new(DirectGpuStore::new(cfg)),
            Arc::new(HttpModelAdapter::new("fixture-model".into(), port)),
        )
        .run_forever(&shutdown)
    });
    let observed=rt.block_on(async{
        let client=cluster.connect("postgres").await.unwrap();
        let deadline=Instant::now()+Duration::from_secs(25);
        while Instant::now()<deadline {
            let row=client.query_one("SELECT r.status,i.outcome FROM forge_agent.runs r LEFT JOIN forge_agent.inference_results i ON i.run_id=r.id WHERE r.id=$1::text::uuid",&[&RUN]).await.unwrap();
            if let Some(outcome)=row.get::<_,Option<Value>>(1){return Some((row.get::<_,String>(0),outcome));}
            tokio::time::sleep(Duration::from_millis(50)).await;
        }None
    });
    stop.set();
    worker.join().unwrap();
    assert_eq!(server.join().unwrap(), 1);
    let (status, result) = observed.expect("native GPU worker must publish");
    assert_eq!(status, "result_ready");
    assert_eq!(
        result,
        json!({"answer":"fixture answer","finish_reason":"stop","usage":{"total_tokens":3}})
    );
    let token = rt.block_on(async {
        cluster
            .connect("postgres")
            .await
            .unwrap()
            .query_one(
                "SELECT attempt_token::text FROM forge_agent.runs WHERE id=$1::text::uuid",
                &[&RUN],
            )
            .await
            .unwrap()
            .get::<_, String>(0)
    });
    assert!(!guard
        .renew(RUN, "d2dd9655-8529-4e55-a670-4ee744374f4f")
        .unwrap());
    guard.publish(RUN, &token, &result).unwrap();
}
