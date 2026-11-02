//! External consumer: public imports, no inclusion of private example source.
use forge_bootstrap::Registry;
use forge_ide::{
    model::{Entry, Name, ProjectReport, TreeNode},
    remote::{InspectReply, RemoteInspectInput},
};
use forge_ide_web_host::{
    register_host, Host, HostConfig, HostLimits, InspectionPort, NodeConfig, ServeOutcome,
};
use forge_ingress_contract::{Header, Response, RouteKey};
use forge_remote::{RemoteError, Target};
use forge_remote_https::{ClientConfig, Credentials, Limits};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    cell::Cell,
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    net::TcpStream,
    path::PathBuf,
    rc::Rc,
    sync::atomic::AtomicBool,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
struct Files(PathBuf);
impl Files {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "forge-ui-host-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&p).unwrap();
        let mut files = Vec::new();
        for name in [
            "index.html",
            "style.css",
            "main.js",
            "worker.js",
            "app.wasm",
        ] {
            let bytes = b"test-only asset";
            fs::write(p.join(name), bytes).unwrap();
            files.push(json!({"path":name,"sha256":format!("{:x}",Sha256::digest(bytes))}));
        }
        fs::write(
            p.join("manifest.json"),
            serde_json::to_vec(&json!({"format_version":1,"abi_version":2,"files":files})).unwrap(),
        )
        .unwrap();
        Self(p)
    }
}
impl Drop for Files {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
struct Fake(Rc<Cell<usize>>);
impl InspectionPort for Fake {
    fn inspect(&self, r: RemoteInspectInput) -> Result<InspectReply, RemoteError> {
        self.0.set(self.0.get() + 1);
        Ok(InspectReply {
            responder: r.target,
            response: Response {
                header: (&Header::new(RouteKey::new("ide.inspect", 1).unwrap(), r.request_id)
                    .unwrap())
                    .into(),
                payload: ProjectReport {
                    format_version: 1,
                    complete: true,
                    entries: 1,
                    files: 0,
                    bytes_read: 0,
                    tree: TreeNode {
                        name: Name::Utf8(".".into()),
                        detail: Entry::Directory { children: vec![] },
                    },
                },
            },
        })
    }
}
fn config(f: &Files) -> HostConfig {
    HostConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        assets: f.0.clone(),
        limits: HostLimits::default(),
        nodes: vec![NodeConfig {
            key: "lab".into(),
            label: "Lab".into(),
            projects: vec!["workspace".into()],
            transport: ClientConfig::MutualTls {
                address: "127.0.0.1:1".parse().unwrap(),
                server_name: "localhost".into(),
                target: Target::new("node", "host").unwrap(),
                roots: vec![vec![1]],
                credentials: Credentials {
                    certificate_chain: vec![vec![1]],
                    private_key_pkcs8: vec![1],
                },
                limits: Limits::default(),
            },
        }],
    }
}
fn host(f: &Files) -> (Host, Rc<Cell<usize>>) {
    let group = register_host("ui", config(f)).unwrap();
    let ((), factory) = Registry::new()
        .register(group)
        .unwrap()
        .freeze()
        .unwrap()
        .into_factories();
    let calls = Rc::new(Cell::new(0));
    let ports: BTreeMap<String, Box<dyn InspectionPort>> = BTreeMap::from([(
        "lab".into(),
        Box::new(Fake(Rc::clone(&calls))) as Box<dyn InspectionPort>,
    )]);
    (factory.create_with_ports(ports).unwrap(), calls)
}
fn exchange(host: &Host, request: String) -> String {
    let address = host.local_addr().unwrap();
    let client = thread::spawn(move || {
        let mut s = TcpStream::connect(address).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        s.write_all(request.as_bytes()).unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        out
    });
    let stop = AtomicBool::new(false);
    loop {
        if host.serve_next(&stop).unwrap() == ServeOutcome::Served {
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }
    client.join().unwrap()
}
fn post(host: &Host, destination: &str, body: Value, extra: &str) -> String {
    let body = body.to_string();
    format!("POST /forge/v1/dispatch HTTP/1.1\r\nHost: {}\r\nOrigin: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nForge-Envelope-Version: 1\r\nForge-Contract-Version: 1\r\nForge-Request-Id: request-1\r\nForge-Destination: {}\r\n{}\r\n{}",host.local_addr().unwrap(),host.origin(),body.len(),destination,extra,body)
}
fn paired(host: &Host) -> (String, String, String) {
    let r = exchange(
        host,
        post(
            host,
            "ui.session.open",
            json!({"kind":"pair","request_id":"request-1","code":host.pairing_code().unwrap()}),
            "",
        ),
    );
    assert!(r.starts_with("HTTP/1.1 200"), "{r}");
    let cookie = r
        .lines()
        .find_map(|s| s.strip_prefix("Set-Cookie: "))
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let body: Value = serde_json::from_str(r.split_once("\r\n\r\n").unwrap().1).unwrap();
    (
        cookie,
        body["csrf"].as_str().unwrap().into(),
        body["event"]["catalog"]["session_epoch"]
            .as_str()
            .unwrap()
            .into(),
    )
}
fn inspect(host: &Host, cookie: &str, csrf: &str, epoch: &str, project: &str) -> String {
    let expires = forge_remote::now_unix_ms().unwrap() + 4000;
    post(host,"ide.inspect",json!({"kind":"inspect","request":{"token":{"session_epoch":epoch,"revision":1,"request_id":"request-1"},"node_key":"lab","target_node":"node","target_incarnation":"host","project":project,"destination":"ide.inspect","contract_version":1,"expires_unix_ms":expires}}),&format!("Cookie: {cookie}\r\nForge-CSRF: {csrf}\r\nForge-Target-Node: node\r\nForge-Target-Incarnation: host\r\nForge-Expires-Unix-Ms: {expires}\r\n"))
}
#[test]
fn public_registrar_is_effect_free_and_must_freeze() {
    let f = Files::new();
    let mut c = config(&f);
    c.assets = f.0.join("absent");
    let group = register_host("ui", c).unwrap();
    let ((), factory) = Registry::new()
        .register(group)
        .unwrap()
        .freeze()
        .unwrap()
        .into_factories();
    assert!(factory.create_with_ports(BTreeMap::new()).is_err());
}
#[test]
fn pair_resume_inspect_use_public_port_once_and_filtered_catalog() {
    let f = Files::new();
    let (h, calls) = host(&f);
    let (cookie, csrf, epoch) = paired(&h);
    let r = exchange(&h, inspect(&h, &cookie, &csrf, &epoch, "workspace"));
    assert!(r.starts_with("HTTP/1.1 200"), "{r}");
    assert!(r.contains("\"entries\""));
    assert_eq!(calls.get(), 1);
    let r = exchange(
        &h,
        post(
            &h,
            "ui.session.resume",
            json!({"kind":"resume","request_id":"request-1"}),
            &format!("Cookie: {cookie}\r\n"),
        ),
    );
    assert!(r.starts_with("HTTP/1.1 200"));
    assert!(r.contains(&epoch));
    assert_eq!(calls.get(), 1);
    assert!(h.pairing_code().is_none());
}
#[test]
fn denied_alias_csrf_epoch_and_header_mismatch_never_call_port() {
    let f = Files::new();
    let (h, calls) = host(&f);
    let (cookie, csrf, epoch) = paired(&h);
    for request in [
        inspect(&h, &cookie, "wrong", &epoch, "workspace"),
        inspect(&h, &cookie, &csrf, "wrong", "workspace"),
        inspect(&h, &cookie, &csrf, &epoch, "private"),
        inspect(&h, &cookie, &csrf, &epoch, "workspace")
            .replace("Forge-Target-Node: node", "Forge-Target-Node: other"),
    ] {
        let r = exchange(&h, request);
        assert!(!r.starts_with("HTTP/1.1 200"), "{r}");
    }
    assert_eq!(calls.get(), 0);
}
#[test]
fn wrong_origin_host_duplicate_headers_and_method_fail_closed() {
    let f = Files::new();
    let (h, calls) = host(&f);
    let body = json!({"kind":"pair","request_id":"request-1","code":h.pairing_code().unwrap()});
    let good = post(&h, "ui.session.open", body, "");
    for request in [
        good.replace(
            &format!("Origin: {}", h.origin()),
            "Origin: http://evil.example",
        ),
        good.replace(
            &format!("Host: {}", h.local_addr().unwrap()),
            "Host: evil.example",
        ),
        good.replace("Content-Type:", "Host: duplicated\r\nContent-Type:"),
        good.replace(
            "Content-Type:",
            "Transfer-Encoding: chunked\r\nContent-Type:",
        ),
        good.clone() + "GET / HTTP/1.1\r\n\r\n",
    ] {
        let r = exchange(&h, request);
        assert!(r.starts_with("HTTP/1.1 400"), "{r}");
    }
    let r = exchange(
        &h,
        format!(
            "GET /forge/v1/dispatch HTTP/1.1\r\nHost: {}\r\n\r\n",
            h.local_addr().unwrap()
        ),
    );
    assert!(r.starts_with("HTTP/1.1 405"));
    assert!(r.contains("Allow: POST"));
    assert_eq!(calls.get(), 0);
    assert!(h.pairing_code().is_some());
}
#[test]
fn pairing_is_one_use_and_attempts_bounded() {
    let f = Files::new();
    let (h, _) = host(&f);
    let code = h.pairing_code().unwrap();
    for _ in 0..20 {
        assert!(exchange(
            &h,
            post(
                &h,
                "ui.session.open",
                json!({"kind":"pair","request_id":"request-1","code":"wrong"}),
                ""
            )
        )
        .starts_with("HTTP/1.1 401"));
    }
    assert!(exchange(
        &h,
        post(
            &h,
            "ui.session.open",
            json!({"kind":"pair","request_id":"request-1","code":code}),
            ""
        )
    )
    .starts_with("HTTP/1.1 401"));
}
#[test]
fn static_assets_are_whitelisted_hashed_and_have_security_headers() {
    let f = Files::new();
    let (h, _) = host(&f);
    let r = exchange(
        &h,
        format!(
            "GET / HTTP/1.1\r\nHost: {}\r\n\r\n",
            h.local_addr().unwrap()
        ),
    );
    assert!(r.starts_with("HTTP/1.1 200"));
    assert!(r.contains("Content-Security-Policy:"));
    assert!(r.contains("nosniff"));
    assert!(!r.contains("session_epoch"));
    let r = exchange(
        &h,
        format!(
            "GET /../manifest.json HTTP/1.1\r\nHost: {}\r\n\r\n",
            h.local_addr().unwrap()
        ),
    );
    assert!(r.starts_with("HTTP/1.1 404"));
    fs::write(f.0.join("main.js"), b"modified").unwrap();
    let group = register_host("ui", config(&f)).unwrap();
    let ((), factory) = Registry::new()
        .register(group)
        .unwrap()
        .freeze()
        .unwrap()
        .into_factories();
    let ports: BTreeMap<String, Box<dyn InspectionPort>> = BTreeMap::from([(
        "lab".into(),
        Box::new(Fake(Rc::new(Cell::new(0)))) as Box<dyn InspectionPort>,
    )]);
    assert!(factory.create_with_ports(ports).is_err());
}

#[test]
fn legacy_or_unknown_asset_abi_is_rejected_before_host_admission() {
    for abi in [0, 1, 3] {
        let f = Files::new();
        let path = f.0.join("manifest.json");
        let mut manifest: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        manifest["abi_version"] = abi.into();
        fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let group = register_host("ui", config(&f)).unwrap();
        let ((), factory) = Registry::new()
            .register(group)
            .unwrap()
            .freeze()
            .unwrap()
            .into_factories();
        let calls = Rc::new(Cell::new(0));
        let ports: BTreeMap<String, Box<dyn InspectionPort>> = BTreeMap::from([(
            "lab".into(),
            Box::new(Fake(Rc::clone(&calls))) as Box<dyn InspectionPort>,
        )]);
        assert!(matches!(
            factory.create_with_ports(ports),
            Err(forge_ide_web_host::HostError::Configuration(
                "asset versions/count"
            ))
        ));
        assert_eq!(calls.get(), 0);
    }
}

#[test]
fn held_body_expires_without_pairing_or_remote_effects() {
    let f = Files::new();
    let mut c = config(&f);
    c.limits.request_timeout = Duration::from_millis(100);
    let group = register_host("ui", c).unwrap();
    let ((), factory) = Registry::new()
        .register(group)
        .unwrap()
        .freeze()
        .unwrap()
        .into_factories();
    let calls = Rc::new(Cell::new(0));
    let ports: BTreeMap<String, Box<dyn InspectionPort>> = BTreeMap::from([(
        "lab".into(),
        Box::new(Fake(Rc::clone(&calls))) as Box<dyn InspectionPort>,
    )]);
    let h = factory.create_with_ports(ports).unwrap();
    let request = post(
        &h,
        "ui.session.open",
        json!({"kind":"pair","request_id":"request-1","code":h.pairing_code().unwrap()}),
        "",
    );
    let address = h.local_addr().unwrap();
    let client = thread::spawn(move || {
        let mut s = TcpStream::connect(address).unwrap();
        let split = request.len() - 1;
        s.write_all(&request.as_bytes()[..split]).unwrap();
        thread::sleep(Duration::from_millis(180));
        let _ = s.write_all(&request.as_bytes()[split..]);
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        out
    });
    let start = std::time::Instant::now();
    loop {
        if h.serve_next(&AtomicBool::new(false)).unwrap() == ServeOutcome::Served {
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }
    assert!(start.elapsed() < Duration::from_secs(1));
    assert!(!client.join().unwrap().contains("200 OK"));
    assert!(h.pairing_code().is_some());
    assert_eq!(calls.get(), 0);
}
#[test]
fn stop_during_held_body_prevents_admission() {
    use std::sync::{atomic::Ordering, Arc};
    let f = Files::new();
    let (h, calls) = host(&f);
    let request = post(
        &h,
        "ui.session.open",
        json!({"kind":"pair","request_id":"request-1","code":h.pairing_code().unwrap()}),
        "",
    );
    let address = h.local_addr().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let signal = Arc::clone(&stop);
    let client = thread::spawn(move || {
        let mut s = TcpStream::connect(address).unwrap();
        let split = request.len() - 1;
        s.write_all(&request.as_bytes()[..split]).unwrap();
        thread::sleep(Duration::from_millis(20));
        signal.store(true, Ordering::Release);
        let _ = s.write_all(&request.as_bytes()[split..]);
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        out
    });
    loop {
        match h.serve_next(&stop).unwrap() {
            ServeOutcome::Served | ServeOutcome::Stopped => break,
            ServeOutcome::Idle => thread::sleep(Duration::from_millis(1)),
        }
    }
    assert!(!client.join().unwrap().contains("200 OK"));
    assert!(h.pairing_code().is_some());
    assert_eq!(calls.get(), 0);
}
#[test]
fn session_expiry_and_duplicate_cookie_fail_closed() {
    let f = Files::new();
    let mut c = config(&f);
    c.limits.session_ttl = Duration::from_millis(40);
    let group = register_host("ui", c).unwrap();
    let ((), factory) = Registry::new()
        .register(group)
        .unwrap()
        .freeze()
        .unwrap()
        .into_factories();
    let calls = Rc::new(Cell::new(0));
    let ports: BTreeMap<String, Box<dyn InspectionPort>> = BTreeMap::from([(
        "lab".into(),
        Box::new(Fake(Rc::clone(&calls))) as Box<dyn InspectionPort>,
    )]);
    let h = factory.create_with_ports(ports).unwrap();
    let (cookie, csrf, epoch) = paired(&h);
    let request = inspect(
        &h,
        &format!("{cookie}; {cookie}"),
        &csrf,
        &epoch,
        "workspace",
    );
    assert!(exchange(&h, request).starts_with("HTTP/1.1 400"));
    thread::sleep(Duration::from_millis(50));
    assert!(
        exchange(&h, inspect(&h, &cookie, &csrf, &epoch, "workspace")).starts_with("HTTP/1.1 401")
    );
    assert_eq!(calls.get(), 0);
}
#[cfg(unix)]
#[test]
fn symlink_assets_are_rejected_even_with_matching_hash() {
    let f = Files::new();
    fs::rename(f.0.join("main.js"), f.0.join("hidden.js")).unwrap();
    std::os::unix::fs::symlink("hidden.js", f.0.join("main.js")).unwrap();
    let group = register_host("ui", config(&f)).unwrap();
    let ((), factory) = Registry::new()
        .register(group)
        .unwrap()
        .freeze()
        .unwrap()
        .into_factories();
    let ports: BTreeMap<String, Box<dyn InspectionPort>> = BTreeMap::from([(
        "lab".into(),
        Box::new(Fake(Rc::new(Cell::new(0)))) as Box<dyn InspectionPort>,
    )]);
    assert!(factory.create_with_ports(ports).is_err());
}
struct Slow {
    calls: Rc<Cell<usize>>,
    expiry: Rc<Cell<u64>>,
}
impl InspectionPort for Slow {
    fn inspect(&self, r: RemoteInspectInput) -> Result<InspectReply, RemoteError> {
        self.expiry.set(r.expires_unix_ms);
        thread::sleep(Duration::from_millis(130));
        Fake(Rc::clone(&self.calls)).inspect(r)
    }
}
#[test]
fn slow_trusted_port_cannot_return_late_http_success() {
    let f = Files::new();
    let mut c = config(&f);
    c.limits.request_timeout = Duration::from_millis(100);
    let group = register_host("ui", c).unwrap();
    let ((), factory) = Registry::new()
        .register(group)
        .unwrap()
        .freeze()
        .unwrap()
        .into_factories();
    let calls = Rc::new(Cell::new(0));
    let expiry = Rc::new(Cell::new(0));
    let ports: BTreeMap<String, Box<dyn InspectionPort>> = BTreeMap::from([(
        "lab".into(),
        Box::new(Slow {
            calls: Rc::clone(&calls),
            expiry: Rc::clone(&expiry),
        }) as Box<dyn InspectionPort>,
    )]);
    let h = factory.create_with_ports(ports).unwrap();
    let (cookie, csrf, epoch) = paired(&h);
    let before = forge_remote::now_unix_ms().unwrap();
    let request = inspect(&h, &cookie, &csrf, &epoch, "workspace")
        .replace(&(before + 4000).to_string(), &(before + 90).to_string());
    // Build a canonical expiry independently of helper's wall-clock sampling.
    let future = before + 90;
    let mut request = request;
    let body_start = request.find("\r\n\r\n").unwrap() + 4;
    let mut body: Value = serde_json::from_str(&request[body_start..]).unwrap();
    body["request"]["expires_unix_ms"] = json!(future);
    request=post(&h,"ide.inspect",body,&format!("Cookie: {cookie}\r\nForge-CSRF: {csrf}\r\nForge-Target-Node: node\r\nForge-Target-Incarnation: host\r\nForge-Expires-Unix-Ms: {future}\r\n"));
    let response = exchange(&h, request);
    assert!(!response.contains("200 OK"));
    assert_eq!(calls.get(), 1);
    assert!(expiry.get() <= future);
}

#[test]
fn actual_host_factory_roundtrips_mtls_and_agent_policy_denies_alias() {
    use forge_ide::remote::{InspectCommand, ReportLimits};
    use forge_provider::Factory;
    use forge_remote_https::{
        gateway::{
            register_gateway_server, GatewayLimits, GatewayServerConfig,
            ServeOutcome as GatewayOutcome,
        },
        ide::IdeCodec,
        CertificateIdentity,
    };
    use rcgen::{
        BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
        KeyUsagePurpose,
    };
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc, Arc,
    };
    let mut ca = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca.key_usages = vec![KeyUsagePurpose::KeyCertSign];
    let key = KeyPair::generate().unwrap();
    let root = ca.self_signed(&key).unwrap().der().to_vec();
    let issuer = Issuer::new(ca, key);
    let leaf = |name: &str, usage| {
        let mut p = CertificateParams::new(vec![name.into()]).unwrap();
        p.extended_key_usages = vec![usage];
        let k = KeyPair::generate().unwrap();
        let cert = p.signed_by(&k, &issuer).unwrap();
        Credentials {
            certificate_chain: vec![cert.der().to_vec()],
            private_key_pkcs8: k.serialize_der(),
        }
    };
    let server_credentials = leaf("localhost", ExtendedKeyUsagePurpose::ServerAuth);
    let client_credentials = leaf("reader", ExtendedKeyUsagePurpose::ClientAuth);
    let server_config = GatewayServerConfig {
        address: "127.0.0.1:0".parse().unwrap(),
        allow_non_loopback: false,
        client_roots: vec![root.clone()],
        credentials: server_credentials,
        identities: vec![CertificateIdentity {
            certificate_der: client_credentials.certificate_chain[0].clone(),
            subject: "operator".into(),
        }],
        limits: GatewayLimits {
            accept_poll: Duration::from_millis(20),
            ..GatewayLimits::default()
        },
    };
    let (tx, rx) = mpsc::channel();
    let effects = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&effects);
    let server = thread::spawn(move || {
        let ((), factory) = Registry::new()
            .register(register_gateway_server("gateway", server_config).unwrap())
            .unwrap()
            .freeze()
            .unwrap()
            .into_factories();
        let server = factory.create().unwrap();
        tx.send(server.local_addr().unwrap()).unwrap();
        let codec = IdeCodec::new(ReportLimits::default()).unwrap();
        let until = std::time::Instant::now() + Duration::from_secs(8);
        let mut handled = 0;
        while handled < 2 && std::time::Instant::now() < until {
            let result = server
                .serve_next(&codec, |call, context| {
                    assert_eq!(context.subject(), "operator");
                    let InspectCommand::Project(alias) = call.request.payload;
                    if alias.as_str() != "workspace" {
                        return Err(RemoteError::Forbidden);
                    }
                    count.fetch_add(1, Ordering::SeqCst);
                    Ok(InspectReply {
                        responder: call.target,
                        response: Response {
                            header: (&call.request.header).into(),
                            payload: ProjectReport {
                                format_version: 1,
                                complete: true,
                                entries: 1,
                                files: 0,
                                bytes_read: 0,
                                tree: TreeNode {
                                    name: Name::Utf8(".".into()),
                                    detail: Entry::Directory { children: vec![] },
                                },
                            },
                        },
                    })
                })
                .unwrap();
            if result == GatewayOutcome::Handled {
                handled += 1;
            }
        }
        assert_eq!(handled, 2);
    });
    let address = rx.recv_timeout(Duration::from_secs(3)).unwrap();
    let f = Files::new();
    let mut c = config(&f);
    c.nodes[0].projects.push("denied".into());
    c.nodes[0].transport = ClientConfig::MutualTls {
        address,
        server_name: "localhost".into(),
        target: Target::new("node", "host").unwrap(),
        roots: vec![root],
        credentials: client_credentials,
        limits: Limits::default(),
    };
    let ((), factory) = Registry::new()
        .register(register_host("ui", c).unwrap())
        .unwrap()
        .freeze()
        .unwrap()
        .into_factories();
    let h = factory.create().unwrap();
    let (cookie, csrf, epoch) = paired(&h);
    let ok = exchange(&h, inspect(&h, &cookie, &csrf, &epoch, "workspace"));
    assert!(ok.contains("\"kind\":\"report\""), "{ok}");
    let denied = exchange(&h, inspect(&h, &cookie, &csrf, &epoch, "denied"));
    assert!(denied.contains("\"category\":\"denied\""), "{denied}");
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    server.join().unwrap();
}

#[test]
fn binary_help_does_not_load_config_or_bind() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_forge-ide-web-host"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8(out.stdout)
        .unwrap()
        .contains("serve --config"));
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_forge-ide-web-host"))
        .arg("unknown")
        .output()
        .unwrap();
    assert!(!out.status.success());
}
