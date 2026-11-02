//! Disposable two-process browser fixture. Creates temporary test credentials,
//! starts the real agent, and hosts only an owned temporary inspection project.
//! No system trust stores, external hosts or operational identities are changed.
use forge_bootstrap::Registry;
use forge_ide_web_host::{load_config, register_host, ServeOutcome};
use forge_provider::Factory;
use rcgen::{
    BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};
use serde_json::json;
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
struct Fixture {
    root: PathBuf,
    child: Option<Child>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(c) = &mut self.child {
            let _ = c.kill();
            let _ = c.wait();
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if !(args.len() == 2 || (args.len() == 3 && args[2] == "--faults")) {
        return Err(
            "usage: loopback_fixture GENERATED_ASSETS_DIRECTORY FORGE_AGENT_BINARY [--faults]"
                .into(),
        );
    }
    let faults = args.len() == 3;
    let assets = PathBuf::from(&args[0]).canonicalize()?;
    let agent = PathBuf::from(&args[1]).canonicalize()?;
    let root = std::env::temp_dir().join(format!(
        "forge-ui-browser-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    ));
    fs::create_dir(&root)?;
    let mut fixture = Fixture { root, child: None };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&fixture.root, fs::Permissions::from_mode(0o700))?;
    }
    let project = fixture.root.join("workspace");
    fs::create_dir(&project)?;
    fs::create_dir(project.join("src"))?;
    fs::write(project.join("src/lib.rs"),b"pub struct Widget { pub value: u32 }\npub trait View { fn show(&self); }\nimpl View for Widget { fn show(&self) {} }\npub fn inspect_me() {}\n")?;
    fs::write(
        project.join("README <safe>.md"),
        b"Fixture <script> text is never HTML.\n",
    )?;
    fs::write(project.join("broken.rs"), b"pub fn broken(\n")?;
    fs::write(
        project.join("<svg onload=alert(1)>.txt"),
        b"safe metadata label\n",
    )?;
    fs::create_dir(project.join("target"))?;
    fs::write(project.join("target/ignored.rs"), b"not Rust")?;
    #[cfg(unix)]
    {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};
        fs::write(
            project.join(OsString::from_vec(vec![b'n', 0xff, b'.', b't', b'x', b't'])),
            b"native name\n",
        )?;
    }
    let mut ca = CertificateParams::new(Vec::<String>::new())?;
    ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    let key = KeyPair::generate()?;
    let cert = ca.self_signed(&key)?;
    fs::write(fixture.root.join("ca.der"), cert.der())?;
    let issuer = Issuer::new(ca, key);
    for (name, usage, dns) in [
        ("server", ExtendedKeyUsagePurpose::ServerAuth, "localhost"),
        ("client", ExtendedKeyUsagePurpose::ClientAuth, "reader"),
    ] {
        let mut p = CertificateParams::new(vec![dns.into()])?;
        p.extended_key_usages = vec![usage];
        p.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        let k = KeyPair::generate()?;
        let c = p.signed_by(&k, &issuer)?;
        fs::write(fixture.root.join(format!("{name}.der")), c.der())?;
        let path = fixture.root.join(format!("{name}.key.der"));
        fs::write(&path, k.serialize_der())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }
    }
    let credentials = |name: &str| json!({"chain":[format!("{name}.der")],"key":format!("{name}.key.der"),"roots":["ca.der"]});
    let server = json!({"mode":"server","bind":"127.0.0.1:0","node":"fixture-node","incarnation":"fixture-host","allow_non_loopback":false,"credentials":credentials("server"),"peers":[{"certificate":"client.der","subject":"reader","node_info":true,"projects":["workspace"]}],"projects":[{"alias":"workspace","root":project}],"limits":{"request_bytes":4096,"response_bytes":4194304,"timeout_ms":5000,"poll_ms":50}});
    let server_path = fixture.root.join("server.json");
    fs::write(&server_path, serde_json::to_vec(&server)?)?;
    let mut child = Command::new(agent)
        .args(["serve", "--config"])
        .arg(server_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;
    let mut reader = BufReader::new(child.stdout.take().ok_or("agent stdout")?);
    fixture.child = Some(child);
    let mut ready = String::new();
    reader.read_line(&mut ready)?;
    let address = ready
        .trim()
        .strip_prefix("READY ")
        .ok_or("agent readiness")?;
    let projects = if faults {
        vec!["workspace", "slow", "timeout"]
    } else {
        vec!["workspace", "denied"]
    };
    let host_config = json!({"bind":"127.0.0.1:0","assets":assets,"credentials":credentials("client"),"nodes":[{"key":"lab","label":"Local fixture","projects":projects,"address":address,"server_name":"localhost","node":"fixture-node","incarnation":"fixture-host"}],"timeout_ms":5000});
    let config_path = fixture.root.join("host.json");
    fs::write(&config_path, serde_json::to_vec(&host_config)?)?;
    let group = register_host("web", load_config(&config_path)?)?;
    let ((), factory) = Registry::new()
        .register(group)
        .map_err(|_| "registration")?
        .freeze()?
        .into_factories();
    let host = if faults {
        let ports: std::collections::BTreeMap<String, Box<dyn forge_ide_web_host::InspectionPort>> =
            std::collections::BTreeMap::from([(
                "lab".into(),
                Box::new(FaultPort) as Box<dyn forge_ide_web_host::InspectionPort>,
            )]);
        factory.create_with_ports(ports)?
    } else {
        factory.create()?
    };
    let stop = Arc::new(AtomicBool::new(false));
    let signal = Arc::clone(&stop);
    ctrlc::set_handler(move || signal.store(true, Ordering::Release))?;
    println!("READY {}", host.origin());
    println!("PAIRING {}", host.pairing_code().ok_or("pairing")?);
    std::io::stdout().flush()?;
    loop {
        match host.serve_next(&stop)? {
            ServeOutcome::Stopped => break,
            ServeOutcome::Idle => std::thread::sleep(Duration::from_millis(10)),
            ServeOutcome::Served => {}
        }
    }
    Ok(())
}

/// Explicit test-only injected fault port; never selected by normal host config.
/// This mode tests browser lifecycle semantics, not remote transport correctness.
struct FaultPort;
impl forge_ide_web_host::InspectionPort for FaultPort {
    fn inspect(
        &self,
        r: forge_ide::remote::RemoteInspectInput,
    ) -> Result<forge_ide::remote::InspectReply, forge_remote::RemoteError> {
        use forge_ide::model::{Entry, FileContent, Name, ProjectReport, TreeNode};
        use forge_ingress_contract::{Header, Response, RouteKey};
        match r.project.as_str() {
            "slow" => std::thread::sleep(Duration::from_millis(700)),
            "timeout" => std::thread::sleep(Duration::from_millis(5500)),
            "workspace" => (),
            _ => return Err(forge_remote::RemoteError::Forbidden),
        }
        let header = Header::new(RouteKey::new("ide.inspect", 1).unwrap(), r.request_id).unwrap();
        Ok(forge_ide::remote::InspectReply {
            responder: r.target,
            response: Response {
                header: (&header).into(),
                payload: ProjectReport {
                    format_version: 1,
                    complete: true,
                    entries: 2,
                    files: 1,
                    bytes_read: 0,
                    tree: TreeNode {
                        name: Name::Utf8(".".into()),
                        detail: Entry::Directory {
                            children: vec![TreeNode {
                                name: Name::Utf8("fault-fixture.txt".into()),
                                detail: Entry::File {
                                    size_bytes: 0,
                                    content: FileContent::Text {
                                        lines: 0,
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
}
