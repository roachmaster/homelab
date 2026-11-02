//! Real executable/server processes on localhost with ephemeral test identities.
//! These tests establish no physical-machine or production deployment claim.
mod support;

use forge_bootstrap::Registry;
use forge_ide::{
    assembly::{register_ide, IdeConfig},
    task::InspectRequest,
};
use forge_provider::Factory;
use forge_task::Task;
use serde_json::json;
use std::{
    fs,
    net::TcpListener,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};
use support::{binary, failure, run, success, Fixture};

#[test]
fn help_and_bad_arguments_do_not_require_configuration_or_open_a_listener() {
    let mut command = binary();
    command.arg("--help");
    let output = run(command, Duration::from_secs(5));
    let help = success(&output);
    assert!(help.contains("serve") && help.contains("inspect") && help.contains("node.info"));
    for args in [
        vec!["no-such-command"],
        vec!["serve", "--config"],
        vec!["inspect", "--surprise"],
        vec!["serve", "--max-connections", "0"],
    ] {
        let mut command = binary();
        command.args(args);
        failure(&run(command, Duration::from_secs(5)));
    }
}

#[test]
fn two_process_node_info_and_inspection_match_the_existing_local_report() {
    let fixture = Fixture::new();
    let original = fs::read(fixture.project.join("src/lib.rs")).unwrap();
    let mut server = fixture.start(&fixture.server_config(), Some(2));
    let client = fixture.client_config(server.address);
    let output = fixture.client(&client, "node.info", None);
    let text = success(&output);
    assert!(text.contains("test-node"));
    let remote = fixture.client(&client, "inspect", Some("workspace"));
    let text = success(&remote);
    assert!(
        text.contains("complete: true")
            && text.contains("traits: 1")
            && text.contains("impl_blocks: 1"),
        "{text}"
    );
    let ((), factory) = Registry::new()
        .register(
            register_ide(
                "local-reference",
                IdeConfig::project(fixture.project.clone()),
            )
            .unwrap(),
        )
        .unwrap()
        .freeze()
        .unwrap()
        .into_factories();
    let local = factory
        .create()
        .unwrap()
        .run(InspectRequest::Project)
        .unwrap();
    assert_eq!(
        remote.stdout,
        forge_ide::yaml::to_yaml(&local, 4 * 1024 * 1024).unwrap(),
        "remote must preserve the full typed report, not just counts"
    );
    server.wait_success();
    assert_eq!(
        original,
        fs::read(fixture.project.join("src/lib.rs")).unwrap()
    );
}

#[test]
fn denied_and_unknown_project_aliases_return_no_report_or_local_path() {
    let fixture = Fixture::new();
    let mut config = fixture.server_config();
    config["projects"]
        .as_array_mut()
        .unwrap()
        .push(json!({"alias":"private","root":fixture.project}));
    let mut server = fixture.start(&config, Some(2));
    let client = fixture.client_config(server.address);
    for alias in ["private", "unknown"] {
        let output = fixture.client(&client, "inspect", Some(alias));
        let error = failure(&output);
        assert!(error.to_ascii_lowercase().contains("forbidden"));
        assert!(!error.contains(fixture.root.to_str().unwrap()));
    }
    server.wait_success();
}

#[test]
fn verified_identity_requires_operation_permission() {
    let fixture = Fixture::new();
    let mut config = fixture.server_config();
    config["peers"][0]["node_info"] = json!(false);
    let mut server = fixture.start(&config, Some(1));
    let output = fixture.client(&fixture.client_config(server.address), "node.info", None);
    assert!(failure(&output).to_ascii_lowercase().contains("forbidden"));
    server.wait_success();
}

#[test]
fn unmapped_verified_peer_is_rejected_without_stopping_the_agent() {
    let fixture = Fixture::new();
    let mut server = fixture.start(&fixture.server_config(), Some(2));
    let good = fixture.client_config(server.address);
    let mut bad = good.clone();
    bad["credentials"] = fixture.credentials("stranger");
    failure(&fixture.client(&bad, "node.info", None));
    success(&fixture.client(&good, "node.info", None));
    server.wait_success();
}

#[test]
fn wrong_server_tls_name_is_rejected_and_next_connection_can_succeed() {
    let fixture = Fixture::new();
    let mut server = fixture.start(&fixture.server_config(), Some(2));
    let good = fixture.client_config(server.address);
    let mut bad = good.clone();
    bad["nodes"][0]["server_name"] = json!("not-localhost.invalid");
    failure(&fixture.client(&bad, "node.info", None));
    success(&fixture.client(&good, "node.info", None));
    server.wait_success();
}

#[test]
fn wrong_target_and_restart_incarnation_are_not_accepted() {
    let fixture = Fixture::new();
    let mut server = fixture.start(&fixture.server_config(), Some(2));
    for field in ["node", "incarnation"] {
        let mut config = fixture.client_config(server.address);
        config["nodes"][0][field] = json!("wrong-target");
        let output = fixture.client(&config, "node.info", None);
        assert!(failure(&output).to_ascii_lowercase().contains("target"));
    }
    server.wait_success();
}

#[test]
fn unknown_node_and_duplicate_names_fail_before_any_network_attempt() {
    let fixture = Fixture::new();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut config = fixture.client_config(listener.local_addr().unwrap());
    config["nodes"][0]["name"] = json!("not-lab");
    failure(&fixture.client(&config, "node.info", None));
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    let mut config = fixture.client_config(listener.local_addr().unwrap());
    let duplicate = config["nodes"][0].clone();
    config["nodes"].as_array_mut().unwrap().push(duplicate);
    failure(&fixture.client(&config, "node.info", None));
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn report_limits_on_server_and_client_do_not_emit_partial_yaml() {
    let fixture = Fixture::new();
    let mut server_config = fixture.server_config();
    server_config["limits"]["response_bytes"] = json!(256);
    let mut server = fixture.start(&server_config, Some(1));
    failure(&fixture.client(
        &fixture.client_config(server.address),
        "inspect",
        Some("workspace"),
    ));
    server.wait_success();
    let mut server = fixture.start(&fixture.server_config(), Some(1));
    let mut client = fixture.client_config(server.address);
    client["limits"]["response_bytes"] = json!(256);
    failure(&fixture.client(&client, "inspect", Some("workspace")));
    server.wait_success();
}

#[test]
fn unreachable_and_stalled_peers_fail_within_the_configured_deadline() {
    let fixture = Fixture::new();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    failure(&fixture.client(&fixture.client_config(address), "node.info", None));

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut client = fixture.client_config(listener.local_addr().unwrap());
    client["limits"]["timeout_ms"] = json!(150);
    let (stop, stopped) = mpsc::channel();
    let peer = thread::spawn(move || {
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            match listener.accept() {
                Ok((socket, _)) => {
                    let _ = stopped.recv_timeout(Duration::from_secs(5));
                    drop(socket);
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < end => {
                    thread::sleep(Duration::from_millis(5))
                }
                Err(e) => panic!("bounded stalled-peer fixture failed: {e}"),
            }
        }
    });
    let start = Instant::now();
    let output = fixture.client(&client, "node.info", None);
    let elapsed = start.elapsed();
    let _ = stop.send(());
    peer.join().unwrap();
    failure(&output);
    assert!(
        elapsed < Duration::from_secs(4),
        "client ignored its deadline"
    );
}

#[test]
fn duplicate_project_configuration_does_not_publish_readiness() {
    let fixture = Fixture::new();
    let mut config = fixture.server_config();
    let duplicate = config["projects"][0].clone();
    config["projects"].as_array_mut().unwrap().push(duplicate);
    let path = fixture.config("bad-server.json", &config);
    let mut command = binary();
    command.arg("serve").arg("--config").arg(path);
    failure(&run(command, Duration::from_secs(5)));
}

#[cfg(unix)]
#[test]
fn sigterm_and_sigint_stop_idle_agents_and_release_their_listeners() {
    use nix::sys::signal::Signal;
    let fixture = Fixture::new();
    for signal in [Signal::SIGTERM, Signal::SIGINT] {
        let mut server = fixture.start(&fixture.server_config(), None);
        server.signal(signal);
        server.wait_success();
        let _rebound = TcpListener::bind(server.address).expect("graceful stop closes listener");
    }
}

#[cfg(unix)]
#[test]
fn shutdown_during_a_held_request_rejects_dispatch_after_stop_acknowledgement() {
    use nix::sys::signal::Signal;
    use std::io::{Read, Write};
    let fixture = Fixture::new();
    let mut config = fixture.server_config();
    config["limits"]["timeout_ms"] = json!(5000);
    let mut server = fixture.start(&config, None);
    let mut stream = fixture.tls(server.address);
    let body = b"{\"payload\":\"info\"}";
    let expires = forge_remote::now_unix_ms().unwrap() + 5000;
    write!(stream,
        "POST /forge/v1/dispatch HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nForge-Envelope-Version: 1\r\nForge-Destination: node.info\r\nForge-Contract-Version: 1\r\nForge-Target-Node: test-node\r\nForge-Target-Incarnation: test-host\r\nForge-Request-Id: shutdown-held\r\nForge-Expires-Unix-Ms: {expires}\r\nConnection: close\r\n\r\n",
        body.len()).unwrap();
    stream.write_all(&body[..body.len() - 1]).unwrap();
    stream.flush().unwrap();
    // The incomplete body makes dispatch impossible before the signal. Await the
    // handler's acknowledgement rather than sleeping and guessing it was handled.
    server.signal(Signal::SIGTERM);
    server.wait_stop_requested();
    stream.write_all(&body[body.len() - 1..]).unwrap();
    stream.flush().unwrap();
    let mut response = String::new();
    // TLS close_notify is not part of this assertion: EOF without it may leave
    // rustls reporting UnexpectedEof after a complete HTTP response.
    let _ = stream.take(64 * 1024).read_to_string(&mut response);
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.contains("unavailable"), "{response}");
    assert!(!response.contains("\"outcome\":\"success\""), "{response}");
    server.wait_success();
}
