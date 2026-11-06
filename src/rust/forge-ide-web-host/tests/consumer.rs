//! External consumer: public imports, no inclusion of private example source.
use forge_bootstrap::Registry;
use forge_ide::{
    model::{Entry, Name, ProjectReport, TreeNode},
    remote::{InspectReply, RemoteInspectInput},
};
use forge_ide_web_host::{
    register_host, Host, HostConfig, HostLimits, InspectionPort, NodeConfig, ServeOutcome,
    SessionAuth,
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
            serde_json::to_vec(&json!({"format_version":1,"abi_version":3,"files":files})).unwrap(),
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
        public_origin: None,
        session_auth: SessionAuth::Pairing,
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
    host_with_config(config(f))
}
fn host_with_config(config: HostConfig) -> (Host, Rc<Cell<usize>>) {
    let group = register_host("ui", config).unwrap();
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
    format!("POST /forge/v1/dispatch HTTP/1.1\r\nHost: {}\r\nOrigin: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nForge-Envelope-Version: 1\r\nForge-Contract-Version: 1\r\nForge-Request-Id: request-1\r\nForge-Destination: {}\r\n{}\r\n{}",host.origin().split_once("://").unwrap().1,host.origin(),body.len(),destination,extra,body)
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
    session_values(host, &r)
}
fn resumed(host: &Host, cookie: Option<&str>) -> (String, String, String) {
    let r = exchange(
        host,
        post(
            host,
            "ui.session.resume",
            json!({"kind":"resume","request_id":"request-1"}),
            &cookie.map_or_else(String::new, |value| format!("Cookie: {value}\r\n")),
        ),
    );
    session_values(host, &r)
}
fn session_values(host: &Host, r: &str) -> (String, String, String) {
    assert!(r.starts_with("HTTP/1.1 200"), "{r}");
    let cookie_header = r
        .lines()
        .find_map(|s| s.strip_prefix("Set-Cookie: "))
        .unwrap();
    assert_eq!(
        cookie_header.split(';').any(|part| part.trim() == "Secure"),
        host.origin().starts_with("https://")
    );
    let cookie = cookie_header.split(';').next().unwrap().to_owned();
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

struct SourcePort {
    reads: Rc<Cell<usize>>,
    corrupt: Rc<Cell<bool>>,
}
impl InspectionPort for SourcePort {
    fn inspect(&self, r: RemoteInspectInput) -> Result<InspectReply, RemoteError> {
        let text = "exact café <script> text\n";
        Ok(InspectReply {
            responder: r.target,
            response: Response {
                header: (&Header::new(RouteKey::new("ide.inspect", 1).unwrap(), r.request_id)
                    .unwrap())
                    .into(),
                payload: ProjectReport {
                    format_version: 1,
                    complete: true,
                    entries: 2,
                    files: 1,
                    bytes_read: text.len(),
                    tree: TreeNode {
                        name: Name::Utf8(".".into()),
                        detail: Entry::Directory {
                            children: vec![TreeNode {
                                name: Name::Utf8("notes.txt".into()),
                                detail: Entry::File {
                                    size_bytes: text.len() as u64,
                                    content: forge_ide::model::FileContent::Text {
                                        sha256: forge_ide::source::digest(text.as_bytes()),
                                        lines: 1,
                                        rust: None,
                                    },
                                },
                            }],
                        },
                    },
                },
            },
        })
    }
    fn source(
        &self,
        r: forge_ide::remote_source::RemoteSourceInput,
    ) -> Result<forge_ide::remote_source::SourceReply, RemoteError> {
        self.reads.set(self.reads.get() + 1);
        assert_eq!(r.selection.path, vec![Name::Utf8("notes.txt".into())]);
        Ok(forge_ide::remote_source::SourceReply {
            responder: r.target,
            response: Response {
                header: (&Header::new(RouteKey::new("ide.source", 1).unwrap(), r.request_id)
                    .unwrap())
                    .into(),
                payload: forge_ide::source::SourceDocument {
                    text: if self.corrupt.get() {
                        "changed".into()
                    } else {
                        "exact café <script> text\n".into()
                    },
                    sha256: r.selection.sha256,
                },
            },
        })
    }
}
#[test]
fn source_uses_only_current_report_entry_and_revalidates_substituted_port() {
    let files = Files::new();
    let group = register_host("source-host", config(&files)).unwrap();
    let ((), factory) = Registry::new()
        .register(group)
        .unwrap()
        .freeze()
        .unwrap()
        .into_factories();
    let reads = Rc::new(Cell::new(0));
    let corrupt = Rc::new(Cell::new(false));
    let ports: BTreeMap<String, Box<dyn InspectionPort>> = BTreeMap::from([(
        "lab".into(),
        Box::new(SourcePort {
            reads: Rc::clone(&reads),
            corrupt: Rc::clone(&corrupt),
        }) as Box<dyn InspectionPort>,
    )]);
    let host = factory.create_with_ports(ports).unwrap();
    let (cookie, csrf, epoch) = paired(&host);
    let expires = forge_remote::now_unix_ms().unwrap() + 4000;
    let headers=format!("Cookie: {cookie}\r\nForge-CSRF: {csrf}\r\nForge-Target-Node: node\r\nForge-Target-Incarnation: host\r\nForge-Expires-Unix-Ms: {expires}\r\n");
    let body = json!({"kind":"source","request":{"token":{"session_epoch":epoch,"revision":2,"request_id":"request-1"},"report":{"session_epoch":epoch,"revision":1,"request_id":"request-1"},"node_key":"lab","target_node":"node","target_incarnation":"host","project":"workspace","destination":"ide.source","contract_version":1,"expires_unix_ms":expires,"entry":1}});
    // No accepted report is not source authority.
    assert!(
        exchange(&host, post(&host, "ide.source", body.clone(), &headers))
            .starts_with("HTTP/1.1 401")
    );
    assert!(
        exchange(&host, inspect(&host, &cookie, &csrf, &epoch, "workspace"))
            .starts_with("HTTP/1.1 200")
    );
    for mutation in 0..8 {
        let mut bad = body.clone();
        match mutation {
            0 => bad["request"]["report"]["revision"] = 99.into(),
            1 => bad["request"]["report"]["session_epoch"] = "old".into(),
            2 => bad["request"]["token"]["session_epoch"] = "old".into(),
            3 => bad["request"]["entry"] = 0.into(),
            4 => bad["request"]["entry"] = 999.into(),
            5 => bad["request"]["project"] = "other".into(),
            6 => bad["request"]["node_key"] = "other".into(),
            _ => bad["request"]["path"] = "/secret".into(),
        }
        let response = exchange(&host, post(&host, "ide.source", bad, &headers));
        assert!(
            response.starts_with("HTTP/1.1 401") || response.starts_with("HTTP/1.1 400"),
            "mutation {mutation}: {response}"
        );
    }
    let denied = exchange(
        &host,
        post(
            &host,
            "ide.source",
            body.clone(),
            &headers.replace(&csrf, "wrong-csrf"),
        ),
    );
    assert!(denied.starts_with("HTTP/1.1 401"));
    assert_eq!(reads.get(), 0);
    let response = exchange(&host, post(&host, "ide.source", body.clone(), &headers));
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    let value: Value = serde_json::from_str(response.split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(
        value["event"]["result"]["outcome"]["document"]["text"],
        "exact café <script> text\n"
    );
    assert_eq!(reads.get(), 1);
    corrupt.set(true);
    let response = exchange(&host, post(&host, "ide.source", body.clone(), &headers));
    let value: Value = serde_json::from_str(response.split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(value["event"]["result"]["outcome"]["kind"], "error");
    assert_eq!(
        value["event"]["result"]["outcome"]["category"],
        "invalid_response"
    );
    let newer = inspect(&host, &cookie, &csrf, &epoch, "workspace")
        .replace("\"revision\":1", "\"revision\":3");
    assert!(exchange(&host, newer).starts_with("HTTP/1.1 200"));
    let before = reads.get();
    assert!(exchange(&host, post(&host, "ide.source", body, &headers)).starts_with("HTTP/1.1 401"));
    assert_eq!(reads.get(), before);
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
fn public_origin_requires_canonical_https_and_keeps_loopback_binding() {
    let f = Files::new();
    for invalid in [
        "",
        "http://portal.example.com",
        "https://",
        "https://Portal.example.com",
        "https://portal.example.com/",
        "https://portal.example.com/path",
        "https://portal.example.com?query",
        "https://portal.example.com#fragment",
        "https://user@portal.example.com",
        "https://portal.example.com:443",
        "https://portal.example.com:0443",
        "https://portal.example.com:08443",
        "https://portal.example.com:0",
        "https://portal.example.com:65536",
        "https://portal.example.com:+8443",
        "https://portal.example.com:8443:80",
        "https://portal.example.com.",
        "https://portal..example.com",
        "https://-portal.example.com",
        "https://portal-.example.com",
        "https://portal_example.com",
        "https://127.0.0.1",
        "https://[::1]",
        "https://portal.example.com\r\nHost: evil.example",
    ] {
        let mut c = config(&f);
        c.public_origin = Some(invalid.into());
        assert!(register_host("ui", c).is_err(), "accepted {invalid:?}");
    }
    let mut c = config(&f);
    c.public_origin = Some(format!("https://{}.example.com", "a".repeat(64)));
    assert!(register_host("ui", c).is_err());
    let mut c = config(&f);
    c.public_origin = Some("https://portal.example.com".into());
    c.bind = "0.0.0.0:8787".parse().unwrap();
    assert!(register_host("ui", c).is_err());
}
#[test]
fn public_origin_rejects_browser_numeric_hosts_without_rejecting_dns_digits() {
    let f = Files::new();
    for invalid in [
        "https://2130706433",
        "https://2130706433:8443",
        "https://0x7f000001",
        "https://0X7F000001",
        "https://127.1",
        "https://0177.0.0.1",
        "https://0x7f.0.0.1",
        "https://0",
        "https://00",
        "https://09",
        "https://0x",
        "https://4294967296",
        "https://0xffffffffffffffffffffffffffff",
        "https://portal.123",
        "https://portal.09",
        "https://portal.0x",
        "https://portal.0xface",
    ] {
        let mut c = config(&f);
        c.public_origin = Some(invalid.into());
        assert!(register_host("ui", c).is_err(), "accepted {invalid:?}");
    }
    for valid in [
        "https://portal.thedevex.com",
        "https://node123.example.com",
        "https://123.example.com",
        "https://0x7f000001.example.com",
        "https://portal.xn--p1ai",
        "https://portal.example.com:8443",
        "https://portal.0xg",
    ] {
        let mut c = config(&f);
        c.public_origin = Some(valid.into());
        assert!(register_host("ui", c).is_ok(), "rejected {valid:?}");
    }
}
#[test]
fn https_origin_uses_secure_cookie_and_preserves_session_csrf_project_admission() {
    let f = Files::new();
    for origin in [
        "https://portal.example.com",
        "https://portal.example.com:8443",
    ] {
        let mut c = config(&f);
        c.public_origin = Some(origin.into());
        let (h, calls) = host_with_config(c);
        assert_eq!(h.origin(), origin);
        assert!(h.local_addr().unwrap().ip().is_loopback());
        assert!(exchange(
            &h,
            inspect(&h, "forge_ui=wrong", "wrong", "wrong", "workspace")
        )
        .starts_with("HTTP/1.1 401"));
        let (cookie, csrf, epoch) = paired(&h);
        for request in [
            inspect(&h, "forge_ui=wrong", &csrf, &epoch, "workspace"),
            inspect(&h, &cookie, "wrong", &epoch, "workspace"),
            inspect(&h, &cookie, &csrf, "wrong", "workspace"),
            inspect(&h, &cookie, &csrf, &epoch, "private"),
        ] {
            assert!(exchange(&h, request).starts_with("HTTP/1.1 401"));
        }
        assert_eq!(calls.get(), 0);
        assert!(
            exchange(&h, inspect(&h, &cookie, &csrf, &epoch, "workspace"))
                .starts_with("HTTP/1.1 200")
        );
        assert_eq!(calls.get(), 1);
        assert!(exchange(
            &h,
            post(
                &h,
                "ui.session.resume",
                json!({"kind":"resume","request_id":"request-1"}),
                &format!("Cookie: {cookie}\r\n")
            )
        )
        .starts_with("HTTP/1.1 200"));
        assert_eq!(calls.get(), 1);
    }
}
#[test]
fn https_origin_rejects_wrong_authority_origin_and_forwarded_header_spoofing() {
    let f = Files::new();
    let mut c = config(&f);
    c.public_origin = Some("https://portal.example.com".into());
    let (h, calls) = host_with_config(c);
    let request = post(
        &h,
        "ui.session.open",
        json!({"kind":"pair","request_id":"request-1","code":h.pairing_code().unwrap()}),
        "",
    );
    for invalid in [
        request.replace(
            "Host: portal.example.com",
            &format!("Host: {}", h.local_addr().unwrap()),
        ),
        request.replace(
            "Host: portal.example.com",
            "Host: evil.example\r\nX-Forwarded-Host: portal.example.com",
        ),
        request.replace(
            "Origin: https://portal.example.com",
            "Origin: http://portal.example.com\r\nX-Forwarded-Proto: https",
        ),
        request.replace(
            "Origin: https://portal.example.com",
            "Origin: https://evil.example\r\nForwarded: proto=https;host=portal.example.com",
        ),
        request.replace(
            "Origin: https://portal.example.com",
            "Origin: https://portal.example.com:443",
        ),
    ] {
        assert!(exchange(&h, invalid).starts_with("HTTP/1.1 400"));
    }
    assert_eq!(calls.get(), 0);
    assert!(h.pairing_code().is_some());
    assert!(exchange(
        &h,
        "GET / HTTP/1.1\r\nHost: portal.example.com\r\n\r\n".into()
    )
    .starts_with("HTTP/1.1 200"));
    assert!(exchange(&h, request).starts_with("HTTP/1.1 200"));
}
#[test]
fn startup_file_loads_explicit_https_origin_and_old_config_remains_local() {
    let f = Files::new();
    fs::write(f.0.join("client.der"), [1]).unwrap();
    fs::write(f.0.join("client.key.der"), [1]).unwrap();
    fs::write(f.0.join("ca.der"), [1]).unwrap();
    let path = f.0.join("host.json");
    let mut value = json!({
        "bind":"127.0.0.1:0", "assets":f.0,
        "credentials":{"chain":["client.der"],"key":"client.key.der","roots":["ca.der"]},
        "nodes":[{"key":"lab","label":"Lab","projects":["workspace"],
            "address":"127.0.0.1:7443","server_name":"localhost","node":"node","incarnation":"host"}]
    });
    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(forge_ide_web_host::load_config(&path)
        .unwrap()
        .public_origin
        .is_none());
    value["public_origin"] = json!("https://portal.example.com");
    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    assert_eq!(
        forge_ide_web_host::load_config(&path)
            .unwrap()
            .public_origin
            .as_deref(),
        Some("https://portal.example.com")
    );
    assert_eq!(
        forge_ide_web_host::load_config(&path).unwrap().session_auth,
        SessionAuth::Pairing
    );
    value["session_auth"] = json!("trusted_proxy");
    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    assert_eq!(
        forge_ide_web_host::load_config(&path).unwrap().session_auth,
        SessionAuth::TrustedProxy
    );
    value["session_auth"] = json!("from_header");
    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(forge_ide_web_host::load_config(&path).is_err());
    value["session_auth"] = json!("trusted_proxy");
    value["public_origin"] = json!("http://portal.example.com");
    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(forge_ide_web_host::load_config(&path).is_err());
}
#[test]
fn trusted_proxy_mode_requires_explicit_https_origin_and_loopback() {
    let f = Files::new();
    let mut c = config(&f);
    c.session_auth = SessionAuth::TrustedProxy;
    assert!(register_host("ui", c.clone()).is_err());
    c.public_origin = Some("https://portal.example.com".into());
    assert!(register_host("ui", c.clone()).is_ok());
    c.bind = "0.0.0.0:8787".parse().unwrap();
    assert!(register_host("ui", c).is_err());
}
#[test]
fn trusted_proxy_resume_establishes_session_without_pairing_and_preserves_active_clients() {
    let f = Files::new();
    let mut c = config(&f);
    c.session_auth = SessionAuth::TrustedProxy;
    c.public_origin = Some("https://portal.example.com".into());
    let (h, calls) = host_with_config(c);
    assert!(h.pairing_code().is_none());
    assert!(exchange(
        &h,
        post(
            &h,
            "ui.session.open",
            json!({"kind":"pair","request_id":"request-1","code":"unused"}),
            ""
        )
    )
    .starts_with("HTTP/1.1 401"));
    assert!(exchange(
        &h,
        inspect(&h, "forge_ui=wrong", "wrong", "wrong", "workspace")
    )
    .starts_with("HTTP/1.1 401"));
    let (cookie, csrf, epoch) = resumed(&h, None);
    assert_eq!(
        resumed(&h, None),
        (cookie.clone(), csrf.clone(), epoch.clone())
    );
    assert_eq!(
        resumed(&h, Some("forge_ui=wrong")),
        (cookie.clone(), csrf.clone(), epoch.clone())
    );
    assert_eq!(
        resumed(&h, Some(&cookie)),
        (cookie.clone(), csrf.clone(), epoch.clone())
    );
    for request in [
        inspect(&h, "forge_ui=wrong", &csrf, &epoch, "workspace"),
        inspect(&h, &cookie, "wrong", &epoch, "workspace"),
        inspect(&h, &cookie, &csrf, "wrong", "workspace"),
        inspect(&h, &cookie, &csrf, &epoch, "private"),
        inspect(&h, &cookie, &csrf, &epoch, "workspace")
            .replace("Forge-Target-Node: node", "Forge-Target-Node: other"),
    ] {
        assert!(!exchange(&h, request).starts_with("HTTP/1.1 200"));
    }
    assert_eq!(calls.get(), 0);
    assert!(
        exchange(&h, inspect(&h, &cookie, &csrf, &epoch, "workspace")).starts_with("HTTP/1.1 200")
    );
    assert_eq!(calls.get(), 1);
    assert!(h.pairing_code().is_none());
}
#[test]
fn trusted_proxy_expired_resume_rotates_credentials_without_manual_pairing() {
    let f = Files::new();
    let mut c = config(&f);
    c.session_auth = SessionAuth::TrustedProxy;
    c.public_origin = Some("https://portal.example.com".into());
    c.limits.session_ttl = Duration::from_millis(80);
    let (h, calls) = host_with_config(c);
    let (old_cookie, old_csrf, old_epoch) = resumed(&h, None);
    thread::sleep(Duration::from_millis(160));
    assert!(exchange(
        &h,
        inspect(&h, &old_cookie, &old_csrf, &old_epoch, "workspace")
    )
    .starts_with("HTTP/1.1 401"));
    let (cookie, csrf, epoch) = resumed(&h, Some(&old_cookie));
    assert_ne!(cookie, old_cookie);
    assert_ne!(csrf, old_csrf);
    assert_ne!(epoch, old_epoch);
    assert!(exchange(
        &h,
        inspect(&h, &old_cookie, &old_csrf, &old_epoch, "workspace")
    )
    .starts_with("HTTP/1.1 401"));
    assert_eq!(calls.get(), 0);
    assert!(
        exchange(&h, inspect(&h, &cookie, &csrf, &epoch, "workspace")).starts_with("HTTP/1.1 200")
    );
    assert_eq!(calls.get(), 1);
}
#[test]
fn trusted_proxy_resume_keeps_origin_checks_and_headers_cannot_enable_mode() {
    let f = Files::new();
    let mut c = config(&f);
    c.public_origin = Some("https://portal.example.com".into());
    let (default_host, _) = host_with_config(c.clone());
    let forwarded = "Cf-Access-Jwt-Assertion: fake\r\nX-Forwarded-Proto: https\r\nX-Forwarded-Host: portal.example.com\r\n";
    assert!(exchange(
        &default_host,
        post(
            &default_host,
            "ui.session.resume",
            json!({"kind":"resume","request_id":"request-1"}),
            forwarded
        )
    )
    .starts_with("HTTP/1.1 401"));
    assert!(default_host.pairing_code().is_some());
    c.session_auth = SessionAuth::TrustedProxy;
    let (h, calls) = host_with_config(c);
    let request = post(
        &h,
        "ui.session.resume",
        json!({"kind":"resume","request_id":"request-1"}),
        forwarded,
    );
    for invalid in [
        request.replace(
            "Origin: https://portal.example.com",
            "Origin: https://evil.example",
        ),
        request.replace(
            "Host: portal.example.com\r\nOrigin",
            "Host: evil.example\r\nOrigin",
        ),
    ] {
        let response = exchange(&h, invalid);
        assert!(response.starts_with("HTTP/1.1 400"));
        assert!(!response.contains("Set-Cookie:"));
    }
    assert!(exchange(&h, request).starts_with("HTTP/1.1 200"));
    assert_eq!(calls.get(), 0);
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
    for abi in [0, 1, 2, 4] {
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
