//! External consumer: public imports, no inclusion of private example source.
use forge_bootstrap::Registry;
use forge_ide::{
    model::{Entry, Name, ProjectReport, TreeNode},
    remote::{InspectReply, RemoteInspectInput},
};
use forge_ide_web_host::{
    register_host, Host, HostConfig, HostError, HostLimits, InspectionPort, NodeConfig,
    ServeOutcome, SessionAuth,
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
            serde_json::to_vec(&json!({"format_version":1,"abi_version":13,"files":files}))
                .unwrap(),
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
        chat: None,
        bind: "127.0.0.1:0".parse().unwrap(),
        public_origin: None,
        session_auth: SessionAuth::Pairing,
        assets: f.0.clone(),
        alternate_cooked_assets: None,
        cooked_replay_archive: None,
        studio: None,
        work: None,
        usage: None,
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
#[cfg(unix)]
#[path = "consumer/work.rs"]
mod work;
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
fn exchange_bytes(host: &Host, request: String) -> Vec<u8> {
    let address = host.local_addr().unwrap();
    let client = thread::spawn(move || {
        let mut s = TcpStream::connect(address).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        s.write_all(request.as_bytes()).unwrap();
        let mut out = Vec::new();
        s.read_to_end(&mut out).unwrap();
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
fn exchange(host: &Host, request: String) -> String {
    String::from_utf8(exchange_bytes(host, request)).unwrap()
}
fn post(host: &Host, destination: &str, body: Value, extra: &str) -> String {
    let body = body.to_string();
    format!("POST /forge/v1/dispatch HTTP/1.1\r\nHost: {}\r\nOrigin: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nForge-Envelope-Version: 1\r\nForge-Contract-Version: 1\r\nForge-Request-Id: request-1\r\nForge-Destination: {}\r\n{}\r\n{}",host.origin().split_once("://").unwrap().1,host.origin(),body.len(),destination,extra,body)
}
fn workspace_post(host: &Host, body: &Value, auth: Option<(&str, &str)>) -> String {
    let body = body.to_string();
    let authority = host.origin().split_once("://").unwrap().1;
    let auth = auth.map_or_else(String::new, |(cookie, csrf)| {
        format!("Cookie: {cookie}\r\nForge-CSRF: {csrf}\r\n")
    });
    format!(
        "POST /forge/v1/dispatch/workspace HTTP/1.1\r\nHost: {authority}\r\nOrigin: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{auth}\r\n{body}",
        host.origin(),
        body.len(),
    )
}

fn workspace_reply(host: &Host, body: Value, auth: (&str, &str)) -> Value {
    let response = exchange(host, workspace_post(host, &body, Some(auth)));
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    let (headers, body) = response.split_once("\r\n\r\n").unwrap();
    assert!(headers
        .lines()
        .any(|line| line == "Cache-Control: no-store"));
    serde_json::from_str(body).unwrap()
}

fn workspace_snapshot(draft: &str) -> Value {
    json!({"chat":{"selected":0,"conversations":[{"messages":[],"draft":draft}]},
           "workbench":null,"workbench_open":false})
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
fn configured_host_resources_open_only_at_factory_creation() {
    let files = Files::new();
    for (config, error) in [
        (
            HostConfig {
                cooked_replay_archive: Some(files.0.join("absent-archive")),
                ..config(&files)
            },
            "cooked replay archive root",
        ),
        (
            HostConfig {
                chat: Some(forge_ide_web_host::ChatConfig {
                    endpoint: "127.0.0.1:1".parse().unwrap(),
                    token_file: files.0.join("absent-chat-token"),
                    workspace_persistence: false,
                    async_dispatch: false,
                    mail_enabled: false,
                    mail_archive_enabled: false,
                }),
                ..config(&files)
            },
            "invalid chat token file",
        ),
        (
            HostConfig {
                studio: Some(forge_ide_web_host::StudioConfig {
                    endpoint: "127.0.0.1:1".parse().unwrap(),
                    token_file: files.0.join("absent-studio-token"),
                    preview_origin: "https://preview.example.test".into(),
                }),
                ..config(&files)
            },
            "invalid chat token file",
        ),
        (
            HostConfig {
                work: Some(forge_ide_web_host::WorkConfig {
                    endpoint: "127.0.0.1:1".parse().unwrap(),
                    token_file: files.0.join("absent-work-token"),
                    allowed_projects: vec!["coding-only".into()],
                    allowed_profiles: vec!["agy-plan-v1".into()],
                    allowed_modes: vec![forge_ide_web_host::WorkMode::Plan],
                }),
                ..config(&files)
            },
            "invalid chat token file",
        ),
        (
            HostConfig {
                usage: Some(forge_ide_web_host::UsageConfig {
                    endpoint: "127.0.0.1:1".parse().unwrap(),
                    token_file: files.0.join("absent-usage-token"),
                    allowed_projects: vec!["coding-only".into()],
                    allowed_profiles: vec!["agy-plan-v1".into()],
                }),
                ..config(&files)
            },
            "invalid chat token file",
        ),
    ] {
        let (_, factory) = Registry::new()
            .register(register_host("ui", config).unwrap())
            .unwrap()
            .freeze()
            .unwrap()
            .into_factories();
        let ports = BTreeMap::from([(
            "lab".into(),
            Box::new(Fake(Rc::new(Cell::new(0)))) as Box<dyn InspectionPort>,
        )]);
        assert!(matches!(
            factory.create_with_ports(ports),
            Err(HostError::Configuration(actual)) if actual == error
        ));
    }
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
    let old_config = forge_ide_web_host::load_config(&path).unwrap();
    assert!(old_config.public_origin.is_none());
    assert!(old_config.alternate_cooked_assets.is_none());
    assert!(old_config.work.is_none());
    value["public_origin"] = json!("https://portal.example.com");
    value["work"] = json!({"endpoint":"127.0.0.1:18081","token_file":"work.token",
        "allowed_projects":["coding-only"],"allowed_profiles":["agy-plan-v1"],"allowed_modes":["plan"]});
    value["alternate_cooked_assets"] = json!("pinned-cooked-assets");
    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    let configured = forge_ide_web_host::load_config(&path).unwrap();
    assert_eq!(configured.work.unwrap().token_file, f.0.join("work.token"));
    assert_eq!(
        configured.alternate_cooked_assets,
        Some(f.0.join("pinned-cooked-assets"))
    );
    assert_eq!(
        configured.public_origin.as_deref(),
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
fn workspace_checkpoint_requires_session_csrf_origin_and_a_bounded_body() {
    let files = Files::new();
    let (host, calls) = host(&files);
    let (cookie, csrf, _) = paired(&host);
    let restore = json!({"operation":"restore","tab":"a".repeat(32)});
    let good = workspace_post(&host, &restore, Some((&cookie, &csrf)));
    for (request, status) in [
        (workspace_post(&host, &restore, None), 401),
        (
            workspace_post(&host, &restore, Some(("forge_ui=wrong", &csrf))),
            401,
        ),
        (
            workspace_post(&host, &restore, Some((&cookie, "wrong"))),
            401,
        ),
        (good.replace(&format!("Forge-CSRF: {csrf}\r\n"), ""), 401),
        (
            good.replace(
                &format!("Origin: {}", host.origin()),
                "Origin: https://different.example",
            ),
            400,
        ),
        (
            good.replace(&format!("Origin: {}\r\n", host.origin()), ""),
            400,
        ),
        (
            workspace_post(
                &host,
                &json!({"operation":"restore","tab":"A".repeat(32)}),
                Some((&cookie, &csrf)),
            ),
            400,
        ),
    ] {
        let reply = exchange(&host, request);
        assert!(reply.starts_with(&format!("HTTP/1.1 {status} ")), "{reply}");
        assert!(reply.contains("\r\nCache-Control: no-store\r\n"));
    }

    // An oversized Content-Length is rejected before the host waits for its body.
    let (headers, _) = good.split_once("\r\n\r\n").unwrap();
    let oversized = headers.replace(
        &format!("Content-Length: {}", restore.to_string().len()),
        &format!("Content-Length: {}", 2 * 1024 * 1024 + 1),
    ) + "\r\n\r\n";
    assert!(exchange(&host, oversized).starts_with("HTTP/1.1 413 "));

    let loaded = workspace_reply(&host, restore, (&cookie, &csrf));
    assert!(loaded["lease"]
        .as_str()
        .is_some_and(|lease| !lease.is_empty()));
    assert_eq!(loaded["snapshot"], Value::Null);
    assert_eq!(calls.get(), 0);
}

#[test]
fn workspace_checkpoint_isolates_tabs_and_keeps_the_newest_revision_across_resume() {
    let files = Files::new();
    let mut settings = config(&files);
    settings.public_origin = Some("https://portal.example.com".into());
    settings.session_auth = SessionAuth::TrustedProxy;
    let (host, calls) = host_with_config(settings);
    let (cookie, csrf, epoch) = resumed(&host, None);
    let auth = (cookie.as_str(), csrf.as_str());
    let first_tab = "a".repeat(32);
    let second_tab = "b".repeat(32);
    let restore = |tab: &str| json!({"operation":"restore","tab":tab});
    let save = |tab: &str, lease: &str, revision: u32, snapshot: &Value| json!({"operation":"save","tab":tab,"lease":lease,"revision":revision,"snapshot":snapshot});

    let first = workspace_reply(&host, restore(&first_tab), auth);
    let first_lease = first["lease"].as_str().unwrap();
    assert_eq!(first["snapshot"], Value::Null);
    let mut latest = workspace_snapshot("Newest draft in first tab");
    let long_conversation = json!({"messages":[
        {"role":"user","content":"p".repeat(2048)},
        {"role":"assistant","content":"a".repeat(4096)},
        {"role":"user","content":"f".repeat(1024)},
        {"role":"assistant","content":"b".repeat(4096)}
    ],"draft":""});
    latest["chat"]["conversations"]
        .as_array_mut()
        .unwrap()
        .extend([long_conversation.clone(), long_conversation]);
    assert!(latest.to_string().len() > HostLimits::default().request_bytes);
    let older = workspace_snapshot("Out-of-order draft");
    for (revision, snapshot) in [(2, &latest), (1, &older), (2, &older)] {
        assert_eq!(
            workspace_reply(
                &host,
                save(&first_tab, first_lease, revision, snapshot),
                auth
            ),
            json!({})
        );
    }

    let second = workspace_reply(&host, restore(&second_tab), auth);
    let second_lease = second["lease"].as_str().unwrap();
    assert_eq!(second["snapshot"], Value::Null);
    assert_ne!(second_lease, first_lease);
    let cross_tab = save(&second_tab, first_lease, 3, &older);
    assert!(
        exchange(&host, workspace_post(&host, &cross_tab, Some(auth))).starts_with("HTTP/1.1 401 ")
    );
    let other = workspace_snapshot("Second tab draft");
    assert_eq!(
        workspace_reply(&host, save(&second_tab, second_lease, 1, &other), auth),
        json!({})
    );

    assert_eq!(
        resumed(&host, Some(&cookie)),
        (cookie.clone(), csrf.clone(), epoch)
    );
    let reloaded = workspace_reply(&host, restore(&first_tab), auth);
    let new_lease = reloaded["lease"].as_str().unwrap();
    assert_ne!(new_lease, first_lease);
    assert_eq!(reloaded["snapshot"], latest);
    let delayed = save(&first_tab, first_lease, u32::MAX, &older);
    assert!(
        exchange(&host, workspace_post(&host, &delayed, Some(auth))).starts_with("HTTP/1.1 401 ")
    );
    let mut invalid = latest.clone();
    invalid["chat"]["selected"] = 7.into();
    let invalid = save(&first_tab, new_lease, 1, &invalid);
    assert!(
        exchange(&host, workspace_post(&host, &invalid, Some(auth))).starts_with("HTTP/1.1 400 ")
    );
    assert_eq!(
        workspace_reply(&host, restore(&first_tab), auth)["snapshot"],
        latest
    );
    assert_eq!(
        workspace_reply(&host, restore(&second_tab), auth)["snapshot"],
        other
    );
    assert_eq!(calls.get(), 0);
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
fn portal_home_serves_the_fixed_script_free_landing_page() {
    let files = Files::new();
    let (host, _) = host(&files);
    let address = host.local_addr().unwrap();
    let request = |method: &str, target: &str| {
        exchange(
            &host,
            format!("{method} {target} HTTP/1.1\r\nHost: {address}\r\n\r\n"),
        )
    };

    for path in ["/home", "/home/"] {
        let response = request("GET", path);
        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.contains("Content-Type: text/html; charset=utf-8"));
        assert!(response.contains("Content-Security-Policy:"));
        assert!(!response.contains("Set-Cookie:"));
        assert!(response.contains("<h1>Forge portal</h1>"));
        assert!(response.contains("href=\"/design/home.css\""));
        assert!(response.contains("id=\"home-tools\""));
        assert!(response.contains("id=\"home-docs\""));
        assert!(response.contains("home-character-feature"));
        assert!(response.contains("class=\"home-footer\""));
        assert!(response.contains("aria-label=\"GitHub\""));
        assert!(!response.contains("Back to Forge"));
        for (destination, label) in [
            ("/", "Open Forge IDE"),
            ("/design", "Browse design docs"),
            ("/design/characters", "Explore Scout character creation"),
            ("/", "Forge IDE"),
            ("/studio", "Forge Studio"),
            ("/playtest", "Room One playtest"),
            ("/design/forge-providers", "Forge plug-and-play"),
            ("/design#forge-foundations", "Forge foundations"),
            ("/design#chat-and-mail", "Conversation storage"),
            ("/design#game-development", "Game development"),
            (
                "/design#wireframes-and-workspace",
                "Wireframes &amp; IDE screens",
            ),
            (
                "https://github.com/roachmaster/homelab",
                "Homelab on GitHub",
            ),
        ] {
            assert!(
                response.contains(&format!("href=\"{destination}\"")),
                "{destination}"
            );
            assert!(
                response.contains(&format!("aria-label=\"{label}\"")),
                "{label}"
            );
        }
        assert!(!response.contains("<script"));
        assert!(!response.contains("target=\"_blank\""));

        let head = request("HEAD", path);
        assert!(head.starts_with("HTTP/1.1 200 OK"));
        assert!(head.ends_with("\r\n\r\n"));

        let post = request("POST", path);
        assert!(post.starts_with("HTTP/1.1 405 Method Not Allowed"));
        assert!(post.contains("\r\nAllow: GET, HEAD\r\n"));
    }

    let stylesheet = request("GET", "/design/home.css");
    assert!(stylesheet.starts_with("HTTP/1.1 200 OK"));
    assert!(stylesheet.contains("Content-Type: text/css; charset=utf-8"));
    assert!(stylesheet.contains(".home-primary"));
    assert!(request("GET", "/home/unknown").starts_with("HTTP/1.1 404 Not Found"));
    assert!(request("GET", "/design").contains("href=\"/home\""));
    for path in [
        "/home",
        "/design",
        "/design/implementation",
        "/design/characters",
        "/design/forge-providers",
        "/studio",
        "/playtest",
    ] {
        let response = request("GET", path);
        let header = response
            .split_once("<header class=\"site-header\">")
            .unwrap()
            .1
            .split_once("</header>")
            .unwrap()
            .0;
        assert!(
            header.contains("class=\"wordmark return-link\" href=\"/home\""),
            "{path}"
        );
    }
}

#[test]
fn design_reference_serves_only_the_fixed_documents_and_images() {
    let f = Files::new();
    let (h, _) = host(&f);
    let request = |method: &str, target: &str| {
        exchange(
            &h,
            format!(
                "{method} {target} HTTP/1.1\r\nHost: {}\r\n\r\n",
                h.local_addr().unwrap()
            ),
        )
    };
    for path in ["/design", "/design/"] {
        let response = request("GET", path);
        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.contains("Content-Type: text/html; charset=utf-8"));
        assert!(response.contains("Content-Security-Policy:"));
        assert!(response.contains("Forge IDE design"));
        assert!(response.contains("href=\"/design/index.css\""));
        assert!(response.contains("href=\"/design/implementation\""));
        assert!(response.contains("href=\"/design/characters\""));
        assert!(response.contains("href=\"/design/forge-providers\""));
        assert!(response.contains("href=\"/design/chat-dispatch\""));
        assert!(response.contains("href=\"/design/mail\""));
        assert!(response.contains("href=\"/design/game-studio\""));
        assert!(response.contains("href=\"/design/game-physics\""));
        assert!(response.contains("href=\"/design/game-asset-pipeline\""));
        assert!(response.contains("href=\"/design/forge-workers\""));
        assert!(response.contains("href=\"/design/agent-feedback-loop\""));
        assert!(response.contains("href=\"/playtest\""));
        for href in [
            "/design/previous-chats",
            "/design/wireframes#phone",
            "/design/wireframes#desktop",
            "/design/workspace",
        ] {
            assert!(response.contains(&format!("href=\"{href}\"")));
        }
        assert!(response.contains("aria-label=\"Forge views\""));
        assert!(response.contains("aria-label=\"Design topics\""));
        assert!(response.contains("href=\"/design/docs.css\""));
        assert!(!response.contains("id=\"previous-chat-menu\""));
        assert!(!response.contains("<img"));
        assert!(!response.contains("<script"));
        assert!(!response.contains("target=\"_blank\""));
    }
    assert!(request("GET", "/design/style.css").contains("Content-Type: text/css; charset=utf-8"));
    assert!(request("GET", "/design/index.css").contains("Content-Type: text/css; charset=utf-8"));
    assert!(request("GET", "/design/docs.css").contains("Content-Type: text/css; charset=utf-8"));
    for (path, heading, marker) in [
        (
            "/design/autonomous-agents",
            "Autonomous agent workflow",
            "Configure an author and an independent reviewer",
        ),
        (
            "/design/agent-feedback-loop",
            "Agent feedback and repair",
            "Authorizing automatic repair never grants automatic approval",
        ),
        (
            "/design/characters",
            "Scout character creation",
            "src=\"/design/characters/",
        ),
        (
            "/design/implementation",
            "Current Forge implementation",
            "class=\"implementation-report\"",
        ),
        (
            "/design/previous-chats",
            "Previous chats",
            "id=\"previous-chat-menu\"",
        ),
        (
            "/design/wireframes",
            "IDE wireframes",
            "class=\"wireframe-stage\"",
        ),
        (
            "/design/workspace",
            "Workspace data flow",
            "Workspace recovery",
        ),
    ] {
        for target in [path.to_owned(), format!("{path}/")] {
            let response = request("GET", &target);
            assert!(response.starts_with("HTTP/1.1 200 OK"));
            assert!(response.contains("Content-Type: text/html; charset=utf-8"));
            assert!(response.contains("Content-Security-Policy:"));
            assert!(response.contains("<h1") && response.contains(&format!(">{heading}</h1>")));
            assert!(response.contains(marker));
            assert!(response.contains("aria-label=\"Forge views\""));
            assert!(response.contains("aria-label=\"Design topics\""));
            assert!(response.contains("href=\"/design/docs.css\""));
            assert!(!response.contains("target=\"_blank\""));
            for href in ["/#agents", "/#workbench", "/home", "/design"] {
                assert!(response.contains(&format!("href=\"{href}\"")));
            }
            assert!(response.contains(">Agents</a>"));
            assert!(!response.contains("href=\"/#mail\""));
            assert_eq!(
                response.contains("src=\"/design/previous-chats.js\""),
                path == "/design/previous-chats"
            );
            let head = request("HEAD", &target);
            assert!(head.starts_with("HTTP/1.1 200 OK"));
            assert!(head.ends_with("\r\n\r\n"));
            assert!(request("POST", &target).starts_with("HTTP/1.1 405"));
        }
        assert!(request("GET", &format!("{path}/unknown")).starts_with("HTTP/1.1 404"));
    }
    let asset_request = |method: &str, target: &str| {
        let response = exchange_bytes(
            &h,
            format!(
                "{method} {target} HTTP/1.1\r\nHost: {}\r\n\r\n",
                h.local_addr().unwrap()
            ),
        );
        let split = response
            .windows(4)
            .position(|bytes| bytes == b"\r\n\r\n")
            .unwrap()
            + 4;
        (
            String::from_utf8(response[..split].to_vec()).unwrap(),
            response[split..].to_vec(),
        )
    };
    let characters = request("GET", "/design/characters");
    assert!(characters.contains("href=\"/design/characters.css\""));
    assert!(characters.contains("href=\"/design#game-development\""));
    assert_eq!(characters.matches("class=\"character-clip\"").count(), 16);
    assert!(!characters.contains("<script"));
    let characters_css = request("GET", "/design/characters.css");
    assert!(characters_css.starts_with("HTTP/1.1 200 OK"));
    assert!(characters_css.contains("Content-Type: text/css; charset=utf-8"));
    assert!(characters_css.contains("prefers-reduced-motion"));
    for (name, mime) in [
        ("scout-illustrated-idle.svg", "image/svg+xml"),
        ("scout-illustrated-front.svg", "image/svg+xml"),
        ("scout-illustrated-back.svg", "image/svg+xml"),
        ("scout-illustrated-ladder.svg", "image/svg+xml"),
        ("mannequin-idle.png", "image/png"),
        ("pixel-idle.png", "image/png"),
        ("illustrated-sheet.png", "image/png"),
        ("illustrated-manifest.json", "application/json"),
        ("showcase-manifest.json", "application/json"),
    ] {
        let path = format!("/design/characters/{name}");
        let (response, body) = asset_request("GET", &path);
        assert!(response.starts_with("HTTP/1.1 200 OK"), "{name}");
        assert!(
            response.contains(&format!("Content-Type: {mime}")),
            "{name}"
        );
        assert!(response.contains("Content-Security-Policy:"), "{name}");
        if mime == "image/png" {
            assert!(body.starts_with(b"\x89PNG\r\n\x1a\n"), "{name}");
        }
        assert!(request("HEAD", &path).ends_with("\r\n\r\n"));
        assert!(request("POST", &path).starts_with("HTTP/1.1 405"));
    }
    for clip in [
        "idle",
        "walk",
        "crouch",
        "jump",
        "attack",
        "spin",
        "kick",
        "roundhouse_kick",
        "punch",
        "uppercut",
        "roll",
        "ledge_pull_up",
        "rope_climb",
        "ladder_climb",
        "barrel_vault",
        "barrel_jump_off",
    ] {
        for (extension, mime) in [("png", "image/png"), ("gif", "image/gif")] {
            let path = format!("/design/characters/clips/{clip}.{extension}");
            let (response, body) = asset_request("GET", &path);
            assert!(response.starts_with("HTTP/1.1 200 OK"), "{path}");
            assert!(
                response.contains(&format!("Content-Type: {mime}")),
                "{path}"
            );
            if extension == "png" {
                assert!(body.starts_with(b"\x89PNG\r\n\x1a\n"), "{path}");
            } else {
                assert!(
                    body.starts_with(b"GIF87a") || body.starts_with(b"GIF89a"),
                    "{path}"
                );
                assert_eq!(u16::from_le_bytes([body[6], body[7]]), 128, "{path}");
                assert_eq!(u16::from_le_bytes([body[8], body[9]]), 176, "{path}");
            }
        }
    }
    let showcase = request("GET", "/design/characters/showcase-manifest.json");
    let showcase: serde_json::Value =
        serde_json::from_str(showcase.split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(showcase["atlas_scale"], 4);
    assert_eq!(showcase["logical_frame_size"], serde_json::json!([32, 44]));
    assert_eq!(showcase["atlas_frame_size"], serde_json::json!([128, 176]));
    assert_eq!(showcase["clip_count"], 16);
    assert_eq!(showcase["frame_count"], 103);
    for path in [
        "/design/characters/build_assets.py",
        "/design/characters/unknown.png",
        "/design/characters/clips/unknown.gif",
        "/design/characters/../assets.rs",
    ] {
        assert!(request("GET", path).starts_with("HTTP/1.1 404"), "{path}");
    }
    let implementation = request("GET", "/design/implementation");
    assert!(implementation.contains("4,096 context / 1,024 completion"));
    assert!(implementation.contains("no Studio relay configured"));
    assert!(implementation.contains("without managed-write opt-in"));
    assert!(implementation.contains("revision/journal checks"));
    assert!(implementation.contains("<a href=\"/design/chat-history\">Chat history</a>"));
    assert!(!implementation.contains("target=\"_blank\""));
    assert!(!implementation.contains("<script"));
    let preview = request("GET", "/design/previous-chats");
    assert!(preview.contains("Interactive preview"));
    assert!(preview.contains("aria-label=\"Reset preview\""));
    assert!(preview.contains("class=\"state-table\""));
    assert!(preview.contains("<th>State</th>"));
    let history = request("GET", "/design/chat-history");
    assert!(history.contains("href=\"/design/previous-chats\""));
    for path in ["/design/forge-providers", "/design/forge-providers/"] {
        let response = request("GET", path);
        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.contains("Content-Type: text/html; charset=utf-8"));
        assert!(response.contains("Content-Security-Policy:"));
        assert!(response.contains("<h1>Forge plug-and-play</h1>"));
        assert!(response.contains("href=\"/design#forge-foundations\""));
        assert!(!response.contains("<script"));
    }
    assert!(request("GET", "/design/forge-providers/unknown").starts_with("HTTP/1.1 404"));
    assert!(request("GET", "/design/previous-chats.js")
        .contains("Content-Type: text/javascript; charset=utf-8"));
    assert!(
        request("GET", "/design/chat-history").contains("Content-Type: text/html; charset=utf-8")
    );
    assert!(request("GET", "/design/chat-history.css")
        .contains("Content-Type: text/css; charset=utf-8"));
    for path in ["/design/chat-dispatch", "/design/chat-dispatch/"] {
        let response = request("GET", path);
        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.contains("Content-Type: text/html; charset=utf-8"));
        assert!(response.contains("href=\"/design/chat-dispatch-sequence.svg\""));
    }
    for path in ["/design/mail", "/design/mail/"] {
        let response = request("GET", path);
        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.contains("Content-Type: text/html; charset=utf-8"));
        assert!(response.contains("Retired portal view"));
        assert!(
            response.contains("Saved PostgreSQL Mail threads and backend contracts are preserved.")
        );
        assert!(response.contains("href=\"/#agents\""));
        assert!(!response.contains("href=\"/#mail\""));
        assert!(response.contains("href=\"/design/forge-ide-mail-wireframe.svg\""));
        assert!(response.contains("href=\"/design/mail-dispatch-sequence.svg\""));
    }
    for path in ["/design/game-studio", "/design/game-studio/"] {
        let response = request("GET", path);
        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.contains("Content-Type: text/html; charset=utf-8"));
        assert!(response.contains("Content-Security-Policy:"));
        assert!(response.contains("<h1>Extending Homelab with a Rust Game Engine</h1>"));
        assert!(response.contains("game.build.submit"));
        assert_eq!(response.matches("<table>").count(), 5);
        assert!(response.contains("href=\"/design/game-engine-architecture.svg\""));
        assert!(response.contains("href=\"/design/game-engine-build-sequence.svg\""));
        assert!(response.contains("href=\"/playtest\""));
        assert!(response.contains("href=\"/design/game-physics\""));
        assert!(response
            .contains("href=\"/design/game-asset-pipeline\">Game asset pipeline design</a>"));
        assert!(response.contains("href=\"/design/forge-workers\">Forge worker design</a>"));
        assert!(!response.contains("<script"));
        let head = request("HEAD", path);
        assert!(head.starts_with("HTTP/1.1 200 OK"));
        assert!(head.ends_with("\r\n\r\n"));
    }
    assert!(
        request("GET", "/design/game-studio.css").contains("Content-Type: text/css; charset=utf-8")
    );
    assert!(request("POST", "/design/game-studio").starts_with("HTTP/1.1 405"));
    for path in ["/design/game-physics", "/design/game-physics/"] {
        let response = request("GET", path);
        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.contains("Content-Type: text/html; charset=utf-8"));
        assert!(response.contains("Content-Security-Policy:"));
        assert!(response.contains("<h1>Physics for the Forge Game Runtime</h1>"));
        assert!(response.contains("<strong>Implemented repository slice.</strong>"));
        assert!(response.contains("href=\"/design/game-physics/tick.svg\""));
        assert!(response.contains("href=\"/design/game-physics/animation.svg\""));
        assert!(response.contains("href=\"/design/game-physics/authority.svg\""));
        assert!(response.contains("Collision world and diagnostics"));
        assert!(response.contains("href=\"/design/game-asset-pipeline\""));
        assert!(!response.contains("<script"));
        assert!(!response.contains("target=\"_blank\""));
        let head = request("HEAD", path);
        assert!(head.starts_with("HTTP/1.1 200 OK"));
        assert!(head.ends_with("\r\n\r\n"));
    }
    assert!(request("POST", "/design/game-physics").starts_with("HTTP/1.1 405"));
    for path in [
        "/design/game-asset-pipeline",
        "/design/game-asset-pipeline/",
    ] {
        let response = request("GET", path);
        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.contains("Content-Type: text/html; charset=utf-8"));
        assert!(response.contains("Content-Security-Policy:"));
        assert!(response.contains("<h1>Asset Pipeline for the Forge Game Runtime</h1>"));
        assert!(response.contains("<strong>Implemented repository slice.</strong>"));
        assert!(response.contains("href=\"/design/game-studio\""));
        assert!(response.contains("href=\"/design/game-physics\""));
        assert!(response.contains("href=\"/design/game-asset-pipeline/cook.svg\""));
        assert!(response.contains("href=\"/design/game-asset-pipeline/load.svg\""));
        assert!(response.contains("href=\"/design/game-asset-pipeline/contract.svg\""));
        assert!(response.contains("href=\"/design/forge-workers/playtest\""));
        assert!(!response.contains("<script"));
        assert!(!response.contains("target=\"_blank\""));
        let head = request("HEAD", path);
        assert!(head.starts_with("HTTP/1.1 200 OK"));
        assert!(head.ends_with("\r\n\r\n"));
    }
    assert!(request("POST", "/design/game-asset-pipeline").starts_with("HTTP/1.1 405"));
    for (path, heading, diagram, cross_link, detail) in [
        (
            "/design/forge-workers",
            "Forge worker and main-thread ownership",
            "ownership.svg",
            "/design/forge-workers/playtest",
            "<dt>JavaScript I/O workers</dt>",
        ),
        (
            "/design/forge-workers/playtest",
            "Playtest worker flow",
            "playtest.svg",
            "/design/forge-workers",
            "Rust matches the result to pending work, verifies the expected digest",
        ),
        (
            "/design/forge-workers/studio",
            "Studio build worker flow",
            "studio.svg",
            "/design/forge-workers",
            "The durable Studio build service is implemented for the static-web profile.",
        ),
    ] {
        for target in [path.to_owned(), format!("{path}/")] {
            let response = request("GET", &target);
            assert!(response.starts_with("HTTP/1.1 200 OK"));
            assert!(response.contains("Content-Type: text/html; charset=utf-8"));
            assert!(response.contains("Content-Security-Policy:"));
            assert!(response.contains("<h1") && response.contains(&format!(">{heading}</h1>")));
            assert!(response.contains("<strong>Implemented repository slice.</strong>"));
            assert!(response.contains(detail));
            assert!(response.contains(&format!("href=\"/design/forge-workers/{diagram}\"")));
            assert!(response.contains(&format!("href=\"{cross_link}\"")));
            assert!(!response.contains("<script"));
            assert!(!response.contains("target=\"_blank\""));
            let head = request("HEAD", &target);
            assert!(head.starts_with("HTTP/1.1 200 OK"));
            assert!(head.ends_with("\r\n\r\n"));
        }
        assert!(request("POST", path).starts_with("HTTP/1.1 405"));
    }
    let workers = request("GET", "/design/forge-workers");
    assert!(workers.contains("href=\"/design/forge-workers/studio\""));
    for path in [
        "/design/chat-dispatch-sequence.svg",
        "/design/forge-ide-mail-wireframe.svg",
        "/design/mail-dispatch-sequence.svg",
        "/design/game-engine-architecture.svg",
        "/design/game-engine-build-sequence.svg",
        "/design/game-physics/tick.svg",
        "/design/game-physics/animation.svg",
        "/design/game-physics/authority.svg",
        "/design/game-asset-pipeline/cook.svg",
        "/design/game-asset-pipeline/load.svg",
        "/design/game-asset-pipeline/contract.svg",
        "/design/forge-workers/ownership.svg",
        "/design/forge-workers/playtest.svg",
        "/design/forge-workers/studio.svg",
    ] {
        let diagram = request("HEAD", path);
        assert!(diagram.starts_with("HTTP/1.1 200 OK"));
        assert!(diagram.contains("Content-Type: image/svg+xml"));
        assert!(diagram.ends_with("\r\n\r\n"));
    }
    assert!(request("GET", "/design/chat-dispatch-sequence.mmd").starts_with("HTTP/1.1 404"));
    assert!(request("GET", "/design/game-physics/tick.mmd").starts_with("HTTP/1.1 404"));
    assert!(request("GET", "/design/game-asset-pipeline/cook.mmd").starts_with("HTTP/1.1 404"));
    assert!(request("GET", "/design/forge-workers/ownership.mmd").starts_with("HTTP/1.1 404"));
    for path in [
        "/design/forge-ide-chat-desktop.png",
        "/design/forge-ide-chat-phone.png",
        "/design/forge-ide-workbench-desktop.png",
        "/design/forge-ide-workbench-phone.png",
        "/design/forge-ide-workbench-project-options.png",
        "/design/forge-ide-chat-history-phone.png",
        "/design/forge-ide-workbench-explorer-phone.png",
        "/design/forge-ide-mail-inbox-phone.png",
        "/design/forge-ide-mail-thread-phone.png",
        "/design/forge-ide-mail-thread-desktop.png",
        "/design/chat-history-open.png",
        "/design/chat-history-retention.png",
        "/design/chat-history-delete.png",
        "/design/chat-history-expiry.png",
        "/design/forge-ide-mail-wireframe.png",
    ] {
        let response = request("HEAD", path);
        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.contains("Content-Type: image/png"));
        assert!(response.ends_with("\r\n\r\n"));
    }
    assert!(request("GET", "/design/manifest.json").starts_with("HTTP/1.1 404 Not Found"));
    assert!(request("POST", "/design").starts_with("HTTP/1.1 405 Method Not Allowed"));
}

#[test]
fn browser_playtest_serves_generated_assets_on_fixed_paths() {
    let f = Files::new();
    let (h, _) = host(&f);
    let request = |method, path| {
        exchange(
            &h,
            format!(
                "{method} {path} HTTP/1.1\r\nHost: {}\r\n\r\n",
                h.local_addr().unwrap()
            ),
        )
    };
    for path in ["/playtest", "/playtest/"] {
        let response = request("GET", path);
        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.contains("Content-Type: text/html; charset=utf-8"));
        assert!(response.contains("Content-Security-Policy:"));
        assert!(response.contains("<canvas id=\"playtest-board\""));
        assert!(response.contains("<details id=\"playtest-debug\"><summary>Debug tools</summary>"));
        assert!(response.contains("id=\"playtest-step\""));
        assert!(response.contains("href=\"/design/game-studio\""));
        assert!(response.contains("src=\"/playtest/main.js\""));
        assert!(request("HEAD", path).ends_with("\r\n\r\n"));
    }
    assert!(request("GET", "/playtest/style.css").contains("Content-Type: text/css; charset=utf-8"));
    let script = request("GET", "/playtest/main.js");
    assert!(script.contains("Content-Type: text/javascript; charset=utf-8"));
    assert!(script.contains("requestAnimationFrame"));
    assert!(script.contains("/playtest/assets-worker.js"));
    assert!(!script.contains("await fetch("));
    let worker = request("GET", "/playtest/assets-worker.js");
    assert!(worker.contains("Content-Type: text/javascript; charset=utf-8"));
    assert!(worker.contains("await fetch("));
    assert!(!worker.contains("crypto.subtle"));
    assert!(request("POST", "/playtest/assets-worker.js").starts_with("HTTP/1.1 405"));
    assert!(request("POST", "/playtest").starts_with("HTTP/1.1 405"));
    assert!(request("GET", "/playtest/unknown.js").starts_with("HTTP/1.1 404"));
}

#[test]
fn stale_browser_documents_redirect_to_the_portal() {
    let f = Files::new();
    let (h, _) = host(&f);
    let get = |target, headers| {
        exchange(
            &h,
            format!(
                "GET {target} HTTP/1.1\r\nHost: {}\r\n{headers}\r\n",
                h.local_addr().unwrap()
            ),
        )
    };
    let html = "Accept: text/html,application/xhtml+xml,*/*;q=0.8\r\n";
    let navigation = format!("{html}Sec-Fetch-Mode: navigate\r\nSec-Fetch-Dest: document\r\n");
    for target in [
        "/?old=1",
        "/index.html?old=1",
        "/obsolete-workbench",
        "/cards_seed.md",
    ] {
        let response = get(target, navigation.as_str());
        assert!(response.starts_with("HTTP/1.1 302 Found"));
        assert!(response.contains("\r\nLocation: /\r\n"));
        assert!(response.contains("\r\nCache-Control: no-store\r\n"));
        assert!(!response.contains("Set-Cookie:"));
    }
    assert!(get("/obsolete-workbench", html).starts_with("HTTP/1.1 302 Found"));

    for target in ["/", "/index.html", "/worker.js"] {
        assert!(get(target, navigation.as_str()).starts_with("HTTP/1.1 200 OK"));
    }
    for target in ["/forge/v1/dispatch", "/forge/v1/dispatch/workspace"] {
        let response = get(target, navigation.as_str());
        assert!(response.starts_with("HTTP/1.1 405 Method Not Allowed"));
        assert!(response.contains("\r\nAllow: POST\r\n"));
    }
    for target in [
        "/api/unknown",
        "/forge/v1/dispatch?old=1",
        "/cdn-cgi/access/unknown",
        "//external.invalid",
    ] {
        assert!(get(target, navigation.as_str()).starts_with("HTTP/1.1 404 Not Found"));
    }
    for headers in [
        "Accept: application/json\r\n",
        "Accept: text/html;q=0,application/json\r\n",
        "Accept: text/html\r\nSec-Fetch-Mode: cors\r\nSec-Fetch-Dest: empty\r\n",
        "Accept: text/html\r\nSec-Fetch-Mode: navigate\r\nSec-Fetch-Dest: iframe\r\n",
    ] {
        let response = get("/missing.js", headers);
        assert!(response.starts_with("HTTP/1.1 404 Not Found"));
        assert!(response.ends_with("{\"error\":\"not_found\"}"));
    }
}

#[test]
fn legacy_or_unknown_asset_abi_is_rejected_before_host_admission() {
    for abi in [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 14] {
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

#[cfg(unix)]
#[test]
fn retired_chat_and_mail_destinations_are_rejected_before_upstream_io() {
    use std::net::TcpListener;
    use std::os::unix::fs::PermissionsExt;

    let files = Files::new();
    let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
    upstream.set_nonblocking(true).unwrap();
    let token_path = files.0.join("runner.token");
    fs::write(&token_path, "0123456789abcdef".repeat(4)).unwrap();
    fs::set_permissions(&token_path, fs::Permissions::from_mode(0o600)).unwrap();
    let mut settings = config(&files);
    settings.chat = Some(forge_ide_web_host::ChatConfig {
        endpoint: upstream.local_addr().unwrap(),
        token_file: token_path,
        workspace_persistence: true,
        async_dispatch: true,
        mail_enabled: true,
        mail_archive_enabled: true,
    });
    let (host, calls) = host_with_config(settings);
    let (cookie, csrf, _) = paired(&host);
    let auth = format!("Cookie: {cookie}\r\nForge-CSRF: {csrf}\r\n");
    let uuid = "4fb285b2-5a53-4caa-90b6-d0aff012c0a4";
    for (destination, body) in [
        (
            "ide.chat",
            json!({"kind":"chat","request":{"request_id":"request-1","messages":[{"role":"user","content":"hi"}]}}),
        ),
        (
            "ide.propose_edit",
            json!({"kind":"propose_edit","request":{"request_id":"request-1","messages":[{"role":"user","content":"hi"}]}}),
        ),
        (
            "ide.chat.history.list",
            json!({"kind":"chat_history_list","request_id":"request-1","limit":10,"cursor":null}),
        ),
        (
            "ide.chat.history.delete",
            json!({"kind":"chat_history_delete","request_id":"request-1","chat_id":uuid,"expected_revision":1}),
        ),
        (
            "ide.chat.run.submit",
            json!({"kind":"chat_run_submit","request_id":"request-1","chat_id":uuid,"content":"hi","idempotency_key":uuid}),
        ),
        (
            "ide.chat.run.models",
            json!({"kind":"chat_run_models","request_id":"request-1"}),
        ),
        (
            "ide.mail.list",
            json!({"kind":"mail_list","request_id":"request-1"}),
        ),
        (
            "ide.mail.send",
            json!({"kind":"mail_send","request_id":"request-1","thread_id":uuid,"subject":"Hi","content":"Hello","idempotency_key":uuid}),
        ),
        (
            "ide.mail.archive",
            json!({"kind":"mail_archive_thread","request_id":"request-1","thread_id":uuid,"expected_revision":1}),
        ),
    ] {
        let response = exchange(&host, post(&host, destination, body, &auth));
        assert!(
            response.starts_with("HTTP/1.1 400") || response.starts_with("HTTP/1.1 401"),
            "{destination} must be rejected: {response}"
        );
        assert_eq!(
            upstream.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock,
            "{destination} must not contact upstream"
        );
    }
    assert_eq!(calls.get(), 0);
}

#[test]
fn legacy_mail_and_agents_entry_paths_redirect_to_agents_fragment() {
    let files = Files::new();
    let (host, _) = host(&files);
    let address = host.local_addr().unwrap();
    for path in ["/mail", "/mail/", "/agents", "/agents/"] {
        for method in ["GET", "HEAD"] {
            let response = exchange(
                &host,
                format!("{method} {path} HTTP/1.1\r\nHost: {address}\r\n\r\n"),
            );
            assert!(
                response.starts_with("HTTP/1.1 302 Found"),
                "{method} {path}: {response}"
            );
            assert!(
                response.contains("Location: /#agents\r\n"),
                "{method} {path}: {response}"
            );
        }
        let post_resp = exchange(
            &host,
            format!("POST {path} HTTP/1.1\r\nHost: {address}\r\n\r\n"),
        );
        assert!(
            post_resp.starts_with("HTTP/1.1 405 Method Not Allowed"),
            "POST {path}: {post_resp}"
        );
    }
}

#[test]
fn workspace_endpoint_round_trips_legacy_chat_and_strips_top_level_mail() {
    let files = Files::new();
    let (host, _) = host(&files);
    let (cookie, csrf, _) = paired(&host);
    let address = host.local_addr().unwrap();
    let origin = host.origin().to_owned();
    let ws_post = |body: Value| {
        let raw = body.to_string();
        exchange(
            &host,
            format!(
                "POST /forge/v1/dispatch/workspace HTTP/1.1\r\nHost: {address}\r\nOrigin: {origin}\r\nContent-Type: application/json\r\nCookie: {cookie}\r\nForge-CSRF: {csrf}\r\nContent-Length: {}\r\n\r\n{raw}",
                raw.len()
            ),
        )
    };
    let tab = "a".repeat(32);
    let restored = ws_post(json!({"operation":"restore","tab":tab,"preserve_chat":true}));
    assert!(restored.starts_with("HTTP/1.1 200 OK"), "{restored}");
    let body: Value = serde_json::from_str(restored.split_once("\r\n\r\n").unwrap().1).unwrap();
    let lease = body["lease"].as_str().unwrap().to_owned();
    assert!(body["snapshot"].is_null());

    let legacy_snapshot = json!({
        "chat": {
            "selected": 0,
            "conversations": [{
                "messages": [
                    {"role":"user","content":"Retained question"},
                    {"role":"assistant","content":"Retained answer"}
                ],
                "draft": "Keep me"
            }]
        },
        "mail": {"active": true, "draft": "legacy mail payload"},
        "workbench": null,
        "workbench_open": true
    });
    let saved = ws_post(json!({
        "operation": "save",
        "tab": tab,
        "lease": lease,
        "revision": 1,
        "snapshot": legacy_snapshot,
        "preserve_chat": true
    }));
    assert!(saved.starts_with("HTTP/1.1 200 OK"), "{saved}");

    let restored_again = ws_post(json!({"operation":"restore","tab":tab,"preserve_chat":true}));
    assert!(
        restored_again.starts_with("HTTP/1.1 200 OK"),
        "{restored_again}"
    );
    let restored_body: Value =
        serde_json::from_str(restored_again.split_once("\r\n\r\n").unwrap().1).unwrap();
    let snapshot = &restored_body["snapshot"];
    assert_eq!(snapshot["chat"]["conversations"][0]["draft"], "Keep me");
    assert_eq!(snapshot["workbench_open"], true);
    assert!(snapshot.get("mail").is_none());
}

#[test]
fn direct_workbench_edit_write_routes_save_and_undo_and_rejects_apply() {
    use forge_ide::managed_edit::{
        ManagedCommand, ManagedResponse, ManagedRevision, WriteIntent, WriteOutcome, WriteReceipt,
    };

    struct WritePort {
        reads: Rc<Cell<usize>>,
        writes: Rc<Cell<usize>>,
    }
    impl InspectionPort for WritePort {
        fn inspect(&self, r: RemoteInspectInput) -> Result<InspectReply, RemoteError> {
            SourcePort {
                reads: Rc::clone(&self.reads),
                corrupt: Rc::new(Cell::new(false)),
            }
            .inspect(r)
        }
        fn source(
            &self,
            r: forge_ide::remote_source::RemoteSourceInput,
        ) -> Result<forge_ide::remote_source::SourceReply, RemoteError> {
            SourcePort {
                reads: Rc::clone(&self.reads),
                corrupt: Rc::new(Cell::new(false)),
            }
            .source(r)
        }
        fn managed_write(
            &self,
            input: forge_remote_https::managed_write::ManagedCall,
        ) -> Result<forge_remote_https::managed_write::ManagedReply, RemoteError> {
            let header = (&input.request.header).into();
            let payload = match input.request.payload.command {
                ManagedCommand::BaseRevision { digest, .. } => {
                    let revision: ManagedRevision = serde_json::from_value(json!({
                        "device": 1,
                        "inode": 2,
                        "digest": digest
                    }))
                    .unwrap();
                    ManagedResponse::BaseRevision { revision }
                }
                ManagedCommand::Execute { request } => {
                    self.writes.set(self.writes.get() + 1);
                    let outcome = match request.intent {
                        WriteIntent::Apply { replacement, .. } => WriteOutcome::Applied {
                            digest: forge_ide::source::digest(replacement.as_bytes()),
                        },
                        WriteIntent::Undo { .. } => WriteOutcome::Reverted {
                            digest: forge_ide::source::digest(b"exact caf\xc3\xa9 <script> text\n"),
                        },
                    };
                    ManagedResponse::Receipt {
                        receipt: WriteReceipt {
                            operation: request.operation,
                            outcome,
                            undo_until_unix_ms: Some(9_000_000_000_000),
                        },
                    }
                }
                ManagedCommand::Status { .. } => ManagedResponse::Status { receipt: None },
            };
            Ok(forge_remote_https::managed_write::ManagedReply {
                responder: input.target,
                response: Response { header, payload },
            })
        }
    }

    let files = Files::new();
    let reads = Rc::new(Cell::new(0));
    let writes = Rc::new(Cell::new(0));
    let ((), factory) = Registry::new()
        .register(register_host("write-host", config(&files)).unwrap())
        .unwrap()
        .freeze()
        .unwrap()
        .into_factories();
    let host = factory
        .create_with_ports(BTreeMap::from([(
            "lab".into(),
            Box::new(WritePort {
                reads: Rc::clone(&reads),
                writes: Rc::clone(&writes),
            }) as Box<dyn InspectionPort>,
        )]))
        .unwrap();
    let (cookie, csrf, epoch) = paired(&host);
    let auth = format!("Cookie: {cookie}\r\nForge-CSRF: {csrf}\r\n");
    assert!(
        exchange(&host, inspect(&host, &cookie, &csrf, &epoch, "workspace"))
            .starts_with("HTTP/1.1 200")
    );
    let original = "exact café <script> text\n";
    let digest = format!("{:x}", Sha256::digest(original.as_bytes()));

    // Legacy Apply proposal intent is rejected cleanly without touching the port.
    let apply = json!({
        "kind": "edit_write",
        "request": {
            "request_id": "request-1",
            "operation": "op-apply",
            "intent": {"kind": "apply", "proposal_id": "stale-proposal"}
        }
    });
    let apply_resp = exchange(&host, post(&host, "ide.edit_write", apply, &auth));
    assert!(apply_resp.starts_with("HTTP/1.1 200"), "{apply_resp}");
    let apply_json: Value =
        serde_json::from_str(apply_resp.split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(apply_json["event"]["outcome"], json!({"kind": "rejected"}));
    assert_eq!(writes.get(), 0);

    // Direct Workbench SaveDocument and Undo execute through managed_write.
    let save = json!({
        "kind": "edit_write",
        "request": {
            "request_id": "request-1",
            "operation": "op-save-1",
            "intent": {
                "kind": "save_document",
                "file": {
                    "report": {"session_epoch": epoch, "revision": 1, "request_id": "request-1"},
                    "node_key": "lab",
                    "project": "workspace",
                    "entry": 1,
                    "sha256": digest,
                    "byte_start": 0,
                    "byte_end": original.len()
                },
                "text": "updated line\n"
            }
        }
    });
    let save_resp = exchange(&host, post(&host, "ide.edit_write", save, &auth));
    assert!(save_resp.starts_with("HTTP/1.1 200"), "{save_resp}");
    let save_json: Value =
        serde_json::from_str(save_resp.split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(save_json["event"]["outcome"]["kind"], "applied");
    assert_eq!(writes.get(), 1);

    let undo = json!({
        "kind": "edit_write",
        "request": {
            "request_id": "request-1",
            "operation": "op-undo-1",
            "intent": {"kind": "undo", "applied_operation": "op-save-1"}
        }
    });
    let undo_resp = exchange(&host, post(&host, "ide.edit_write", undo, &auth));
    assert!(undo_resp.starts_with("HTTP/1.1 200"), "{undo_resp}");
    let undo_json: Value =
        serde_json::from_str(undo_resp.split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(undo_json["event"]["outcome"]["kind"], "reverted");
    assert_eq!(writes.get(), 2);
}

#[cfg(unix)]
#[test]
fn studio_catalog_uses_the_private_relay_and_filters_project_grants() {
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    use std::os::unix::fs::PermissionsExt;

    let files = Files::new();
    let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
    let token = "0123456789abcdef".repeat(4);
    let token_file = files.0.join("studio.token");
    fs::write(&token_file, &token).unwrap();
    fs::set_permissions(&token_file, fs::Permissions::from_mode(0o600)).unwrap();
    let mut settings = config(&files);
    settings.studio = Some(forge_ide_web_host::StudioConfig {
        endpoint: upstream.local_addr().unwrap(),
        token_file,
        preview_origin: "https://preview.example.invalid".into(),
    });
    let (host, _) = host_with_config(settings);
    let (cookie, csrf, _) = paired(&host);
    let auth = format!("Cookie: {cookie}\r\nForge-CSRF: {csrf}\r\n");
    let list = json!({"kind":"game_project_list","request_id":"request-1"});
    assert!(
        exchange(&host, post(&host, "game.project.list", list.clone(), ""))
            .starts_with("HTTP/1.1 401")
    );
    let denied = json!({"kind":"game_project_get","request_id":"request-1",
                        "project":{"node":"lab","project":"unapproved"}});
    assert!(
        exchange(&host, post(&host, "game.project.get", denied, &auth)).starts_with("HTTP/1.1 401")
    );
    let worker = thread::spawn(move || {
        let (mut stream, _) = upstream.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut request = BufReader::new(&mut stream);
        let mut line = String::new();
        request.read_line(&mut line).unwrap();
        assert_eq!(line, "POST /v1/dispatch HTTP/1.1\r\n");
        let mut headers = BTreeMap::new();
        loop {
            line.clear();
            assert_ne!(request.read_line(&mut line).unwrap(), 0);
            if line == "\r\n" {
                break;
            }
            let (name, value) = line.trim_end().split_once(": ").unwrap();
            headers.insert(name.to_ascii_lowercase(), value.to_owned());
        }
        assert_eq!(headers["authorization"], format!("Bearer {token}"));
        assert_eq!(headers["forge-destination"], "game.project.list");
        assert_eq!(headers["forge-request-id"], "request-1");
        assert!(!headers.contains_key("cookie"));
        let mut body = vec![0; headers["content-length"].parse().unwrap()];
        request.read_exact(&mut body).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap()["kind"],
            "game_project_list"
        );
        let revision = "a".repeat(40);
        let reply = json!({"kind":"game_projects_listed","request_id":"request-1","projects":[
            {"node":"lab","project":"workspace","revision":revision,"profiles":["static-web"]},
            {"node":"lab","project":"unapproved","revision":revision,"profiles":["static-web"]}
        ]})
        .to_string();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{reply}",
            reply.len()
        )
        .unwrap();
    });
    let response = exchange(&host, post(&host, "game.project.list", list, &auth));
    assert!(response.starts_with("HTTP/1.1 200 "), "{response}");
    let (_, body) = response.split_once("\r\n\r\n").unwrap();
    let catalog: Value = serde_json::from_str(body).unwrap();
    let projects = catalog["projects"].as_array().unwrap();
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0]["project"], "workspace");
    let page = exchange(
        &host,
        format!(
            "GET /studio HTTP/1.1\r\nHost: {}\r\n\r\n",
            host.local_addr().unwrap()
        ),
    );
    assert!(page.contains("id=\"studio-controls\""));
    assert!(page.contains("/studio/main.js"));
    worker.join().unwrap();
}

/// Direct tests of ChatConfig compatibility and strict catalog schema boundaries.
#[cfg(unix)]
mod manual_mail_archive_boundary {
    use super::*;
    use std::net::TcpListener;
    use std::os::unix::fs::PermissionsExt;

    fn settings(files: &Files, upstream: &TcpListener, enabled: bool) -> HostConfig {
        let token_file = files.0.join("owned-archive-runner.token");
        fs::write(&token_file, "ab".repeat(32)).unwrap();
        fs::set_permissions(&token_file, fs::Permissions::from_mode(0o600)).unwrap();
        // Deliberately omit the property to prove backwards-compatible
        // configuration parsing keeps manual archival off until explicit opt-in.
        let mut chat: forge_ide_web_host::ChatConfig = serde_json::from_value(json!({
            "endpoint":upstream.local_addr().unwrap(),"token_file":token_file,
            "workspace_persistence":true,"async_dispatch":true,"mail_enabled":true
        }))
        .unwrap();
        assert!(!chat.mail_archive_enabled);
        chat.mail_archive_enabled = enabled;
        let mut config = config(files);
        config.chat = Some(chat);
        config
    }

    fn fixture(enabled: bool) -> (Files, TcpListener, Host, Rc<Cell<usize>>) {
        let files = Files::new();
        let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
        upstream.set_nonblocking(true).unwrap();
        let (host, calls) = host_with_config(settings(&files, &upstream, enabled));
        (files, upstream, host, calls)
    }

    fn no_upstream(upstream: &TcpListener) {
        assert!(
            matches!(upstream.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
            "rejected admission must not open an upstream connection"
        );
    }

    fn catalog(response: &str) -> Value {
        serde_json::from_str::<Value>(response.split_once("\r\n\r\n").unwrap().1).unwrap()["event"]
            ["catalog"]
            .clone()
    }

    fn pair_catalog(host: &Host) -> (String, String, Value) {
        let response = exchange(
            host,
            post(
                host,
                "ui.session.open",
                json!({
                    "kind":"pair","request_id":"request-1","code":host.pairing_code().unwrap()
                }),
                "",
            ),
        );
        let (cookie, csrf, _) = session_values(host, &response);
        (cookie, csrf, catalog(&response))
    }

    #[test]
    fn inconsistent_archive_config_is_rejected_before_construction() {
        let files = Files::new();
        let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
        upstream.set_nonblocking(true).unwrap();
        let mut settings = config(&files);
        settings.assets = files.0.join("assets-must-not-be-read");
        let token_file = files.0.join("token-must-not-be-read");
        settings.chat = Some(forge_ide_web_host::ChatConfig {
            endpoint: upstream.local_addr().unwrap(),
            token_file: token_file.clone(),
            workspace_persistence: false,
            async_dispatch: false,
            mail_enabled: false,
            mail_archive_enabled: true,
        });
        assert!(matches!(
            register_host("archive-invalid", settings),
            Err(HostError::Configuration(
                "Mail archive requires mail to be enabled"
            ))
        ));
        assert!(!token_file.exists());
        assert!(!files.0.join("assets-must-not-be-read").exists());
        no_upstream(&upstream);
    }

    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct LegacyCatalogSchema {
        protocol_version: u16,
        session_epoch: String,
        timeout_ms: u64,
        nodes: Vec<forge_ide_ui::protocol::NodeChoice>,
        #[serde(default)]
        chat_history: bool,
        #[serde(default)]
        async_chat: bool,
        #[serde(default)]
        mail_enabled: bool,
        recent_write: Option<forge_ide_ui::protocol::ResumableWrite>,
    }

    #[test]
    fn truthy_catalog_field_fences_a_strict_legacy_schema_after_epoch_rotation() {
        let (_old_files, old_upstream, old_host, _) = fixture(false);
        let (_, _, old_catalog) = pair_catalog(&old_host);
        let legacy: LegacyCatalogSchema = serde_json::from_value(old_catalog).unwrap();
        assert_eq!(legacy.protocol_version, 1);
        assert!(
            !legacy.session_epoch.is_empty() && legacy.timeout_ms > 0 && !legacy.nodes.is_empty()
        );
        assert!(
            legacy.chat_history
                && legacy.async_chat
                && legacy.mail_enabled
                && legacy.recent_write.is_none()
        );
        no_upstream(&old_upstream);

        let files = Files::new();
        let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
        upstream.set_nonblocking(true).unwrap();
        let mut settings = settings(&files, &upstream, true);
        settings.session_auth = SessionAuth::TrustedProxy;
        settings.public_origin = Some("https://portal.example.com".into());
        settings.limits.session_ttl = Duration::from_millis(80);
        let (host, calls) = host_with_config(settings);
        let first = exchange(
            &host,
            post(
                &host,
                "ui.session.resume",
                json!({"kind":"resume","request_id":"request-1"}),
                "",
            ),
        );
        let (cookie, _, epoch) = session_values(&host, &first);
        thread::sleep(Duration::from_millis(160));
        let rotated = exchange(
            &host,
            post(
                &host,
                "ui.session.resume",
                json!({"kind":"resume","request_id":"request-1"}),
                &format!("Cookie: {cookie}\r\n"),
            ),
        );
        let (_, _, next_epoch) = session_values(&host, &rotated);
        assert_ne!(epoch, next_epoch);
        let catalog = catalog(&rotated);
        assert_eq!(catalog["mail_archive_enabled"], true);
        let error = serde_json::from_value::<LegacyCatalogSchema>(catalog.clone())
            .err()
            .expect("legacy strict schema must reject the new advertised capability");
        assert!(error.to_string().contains("mail_archive_enabled"));
        let current: forge_ide_ui::protocol::Catalog = serde_json::from_value(catalog).unwrap();
        assert!(current.mail_archive_enabled);
        no_upstream(&upstream);
        assert_eq!(calls.get(), 0);
    }
}

#[test]
fn coding_agent_and_usage_design_readers_serve_only_fixed_pages_and_diagrams() {
    let files = Files::new();
    let (host, _) = host(&files);
    let address = host.local_addr().unwrap();
    let request = |method: &str, target: &str| {
        exchange(
            &host,
            format!("{method} {target} HTTP/1.1\r\nHost: {address}\r\n\r\n"),
        )
    };

    let home = request("GET", "/home");
    assert!(home.starts_with("HTTP/1.1 200 OK"));
    for (href, label) in [
        ("/design/coding-agents", "Coding-agent dispatch design"),
        ("/design/agent-usage", "Agent usage visibility design"),
    ] {
        assert!(home.contains(&format!("href=\"{href}\"")), "{href}");
        assert!(home.contains(&format!("aria-label=\"{label}\"")), "{label}");
    }
    assert!(home.contains("href=\"/agents\""));
    assert!(home.contains("Agents and the source workbench."));
    assert!(!home.contains("Chat, Mail, and the source workbench."));

    let index = request("GET", "/design");
    assert!(index.starts_with("HTTP/1.1 200 OK"));
    assert!(index.contains("href=\"/design/coding-agents\""));
    assert!(index.contains("href=\"/design/agent-usage\""));
    assert!(!index.contains("href=\"/agents\""));
    assert!(!index.contains("<img"));

    struct DesignReaderCase {
        path: &'static str,
        heading: &'static str,
        cross_link: &'static str,
        source_url: &'static str,
        markers: &'static [&'static str],
        diagrams: &'static [&'static str],
    }
    let pages = [
        DesignReaderCase {
            path: "/design/coding-agents",
            heading: "Coding-agent dispatch",
            cross_link: "/design/agent-usage",
            source_url: "https://github.com/roachmaster/homelab/blob/main/docs/architecture/proposals/forge-coding-agent-dispatch.md",
            markers: &[
                "<strong>Implemented; activation is deployment controlled.</strong>",
                "generation_token_limit: null",
                "awaiting_review",
                "needs_reconcile",
                "coding_reconciliation_receipts",
                "Muse through PostgreSQL",
                "ExternalReviewContextProvider",
                "one 50-event page",
                "external-agent-workflow.md",
            ],
            diagrams: &[
                "/design/coding-agents/ownership.svg",
                "/design/coding-agents/lifecycle.svg",
                "/design/coding-agents/completion.svg",
                "/design/coding-agents/takeover.svg",
            ],
        },
        DesignReaderCase {
            path: "/design/agent-usage",
            heading: "Agent usage and token limits",
            cross_link: "/design/coding-agents",
            source_url: "https://github.com/roachmaster/homelab/blob/main/docs/architecture/proposals/forge-agent-usage.md",
            markers: &[
                "<strong>Implemented; activation is deployment controlled.</strong>",
                "generation_token_limit: null",
                "codex-cli 0.160.0",
                "agy 1.5.12",
                "Forge-observed jobs",
                "unsupported_headless",
                "Static design illustration only:",
            ],
            diagrams: &[
                "/design/agent-usage/providers.svg",
                "/design/agent-usage/collection.svg",
            ],
        },
    ];

    for DesignReaderCase {
        path,
        heading,
        cross_link,
        source_url,
        markers,
        diagrams,
    } in pages
    {
        for target in [path.to_owned(), format!("{path}/")] {
            let response = request("GET", &target);
            assert!(response.starts_with("HTTP/1.1 200 OK"), "{target}");
            assert!(response.contains("Content-Type: text/html; charset=utf-8"));
            assert!(response.contains("Content-Security-Policy:"));
            assert_eq!(response.matches("<h1").count(), 1, "{target}");
            assert!(
                response.contains(&format!("<h1>{heading}</h1>")),
                "{target}"
            );
            assert!(
                response.contains(&format!("href=\"{cross_link}\"")),
                "{target}"
            );
            assert!(
                response.contains(&format!("href=\"{source_url}\"")),
                "{target}"
            );
            assert!(response.contains("class=\"wireframe-stage\""), "{target}");
            assert!(response.contains("loading=\"lazy\""), "{target}");
            assert!(response.contains("aria-label=\"Forge views\""), "{target}");
            assert!(
                response.contains("aria-label=\"Design topics\""),
                "{target}"
            );
            assert!(
                response.contains("href=\"/design#forge-foundations\" aria-current=\"location\""),
                "{target}"
            );
            assert!(response.contains("class=\"docs-breadcrumb\""), "{target}");
            assert!(response.contains("href=\"/design/docs.css\""), "{target}");
            assert!(
                response.contains("class=\"wordmark return-link\" href=\"/home\""),
                "{target}"
            );
            for href in ["/#agents", "/#workbench", "/home", "/design"] {
                assert!(
                    response.contains(&format!("href=\"{href}\"")),
                    "{target}: {href}"
                );
            }
            assert!(response.contains(">Agents</a>"), "{target}");
            assert!(!response.contains("href=\"/#mail\""), "{target}");
            for marker in markers {
                assert!(response.contains(marker), "{target}: {marker}");
            }
            for diagram in diagrams {
                assert!(
                    response.contains(&format!("src=\"{diagram}\"")),
                    "{target}: {diagram}"
                );
                assert!(
                    response.contains(&format!("href=\"{diagram}\"")),
                    "{target}: {diagram}"
                );
            }
            assert!(!response.contains("<script"), "{target}");
            assert!(!response.contains("target=\"_blank\""), "{target}");

            let head = request("HEAD", &target);
            assert!(head.starts_with("HTTP/1.1 200 OK"), "{target}");
            assert!(head.ends_with("\r\n\r\n"), "{target}");
            assert!(
                request("POST", &target).starts_with("HTTP/1.1 405 Method Not Allowed"),
                "{target}"
            );
        }
    }

    for svg in [
        "/design/coding-agents/ownership.svg",
        "/design/coding-agents/lifecycle.svg",
        "/design/coding-agents/completion.svg",
        "/design/coding-agents/takeover.svg",
        "/design/agent-usage/providers.svg",
        "/design/agent-usage/collection.svg",
    ] {
        let get = request("GET", svg);
        assert!(get.starts_with("HTTP/1.1 200 OK"), "{svg}");
        assert!(get.contains("Content-Type: image/svg+xml"), "{svg}");
        assert!(get.contains("Content-Security-Policy:"), "{svg}");
        let body = get.split_once("\r\n\r\n").unwrap().1.trim();
        assert!(body.starts_with("<svg"), "{svg}");
        assert!(body.ends_with("</svg>"), "{svg}");

        let head = request("HEAD", svg);
        assert!(head.starts_with("HTTP/1.1 200 OK"), "{svg}");
        assert!(head.contains("Content-Type: image/svg+xml"), "{svg}");
        assert!(head.contains("Content-Security-Policy:"), "{svg}");
        assert!(head.ends_with("\r\n\r\n"), "{svg}");

        assert!(
            request("POST", svg).starts_with("HTTP/1.1 405 Method Not Allowed"),
            "{svg}"
        );
    }

    for missing in [
        "/design/coding-agents/unknown",
        "/design/coding-agents/ownership.mmd",
        "/design/agent-usage/unknown",
        "/design/agent-usage/providers.mmd",
    ] {
        assert!(
            request("GET", missing).starts_with("HTTP/1.1 404 Not Found"),
            "{missing}"
        );
    }
}
