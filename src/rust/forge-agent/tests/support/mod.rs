//! Ephemeral credentials and guarded real-process fixtures. Nothing is installed.
use rcgen::{
    BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};
use serde_json::{json, Value};
use std::{
    fs,
    io::{BufRead, BufReader, Read},
    net::SocketAddr,
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Output, Stdio},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const MAX_CAPTURE: u64 = 8 * 1024 * 1024;
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub struct Fixture {
    pub root: PathBuf,
    pub project: PathBuf,
}
impl Fixture {
    pub fn new() -> Self {
        let id = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "forge-agent-process-{}-{id}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("create unique owned fixture");
        let fixture = Self {
            project: root.join("workspace"),
            root,
        };
        fs::create_dir(&fixture.project).unwrap();
        fs::create_dir(fixture.project.join("src")).unwrap();
        fs::write(fixture.project.join("src/lib.rs"), b"pub struct Widget;\npub trait View { fn show(&self); }\nimpl View for Widget { fn show(&self) {} }\npub fn inspect_me() {}\n").unwrap();
        fs::write(
            fixture.project.join("README.md"),
            b"A trusted process-test project.\n",
        )
        .unwrap();
        fs::create_dir(fixture.project.join("target")).unwrap();
        fs::write(fixture.project.join("target/excluded.rs"), b"not Rust").unwrap();
        fixture.certificates();
        fixture
    }
    fn certificates(&self) {
        let mut ca = CertificateParams::new(Vec::<String>::new()).unwrap();
        ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca.key_usages = vec![
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::DigitalSignature,
        ];
        let key = KeyPair::generate().unwrap();
        let certificate = ca.self_signed(&key).unwrap();
        self.write("ca.der", certificate.der());
        let issuer = Issuer::new(ca, key);
        for (name, usage, dns) in [
            ("server", ExtendedKeyUsagePurpose::ServerAuth, "localhost"),
            ("client", ExtendedKeyUsagePurpose::ClientAuth, "reader"),
            (
                "stranger",
                ExtendedKeyUsagePurpose::ClientAuth,
                "other-reader",
            ),
        ] {
            let mut params = CertificateParams::new(vec![dns.into()]).unwrap();
            params.extended_key_usages = vec![usage];
            params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
            let key = KeyPair::generate().unwrap();
            let certificate = params.signed_by(&key, &issuer).unwrap();
            self.write(&format!("{name}.der"), certificate.der());
            let path = self.write(&format!("{name}.key.der"), &key.serialize_der());
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
            }
            #[cfg(not(unix))]
            let _ = path;
        }
    }
    pub fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.root.join(name);
        fs::write(&path, bytes).unwrap();
        path
    }
    pub fn config(&self, name: &str, value: &Value) -> PathBuf {
        self.write(name, &serde_json::to_vec(value).unwrap())
    }
    pub fn credentials(&self, name: &str) -> Value {
        json!({"chain":[self.root.join(format!("{name}.der"))],
               "key":self.root.join(format!("{name}.key.der")),
               "roots":[self.root.join("ca.der")]})
    }
    pub fn limits(&self) -> Value {
        // Match production defaults for happy paths; dedicated deadline tests
        // override this explicitly instead of racing normal work against 1s.
        json!({"request_bytes":4096,"response_bytes":4*1024*1024,"timeout_ms":5000,"poll_ms":50})
    }
    pub fn server_config(&self) -> Value {
        json!({"mode":"server", "bind":"127.0.0.1:0", "node":"test-node", "incarnation":"test-host",
            "allow_non_loopback":false, "credentials":self.credentials("server"),
            "peers":[{"certificate":self.root.join("client.der"),"subject":"reader","node_info":true,"projects":["workspace"]}],
            "projects":[{"alias":"workspace","root":self.project}], "limits":self.limits()})
    }
    pub fn client_config(&self, address: SocketAddr) -> Value {
        json!({"mode":"client", "credentials":self.credentials("client"),
            "nodes":[{"name":"lab","address":address,"server_name":"localhost","node":"test-node","incarnation":"test-host"}],
            "limits":self.limits()})
    }
    pub fn start(&self, config: &Value, max_connections: Option<usize>) -> Server {
        let path = self.config("server.json", config);
        Server::start(&path, max_connections)
    }
    pub fn client(&self, config: &Value, operation: &str, alias: Option<&str>) -> Output {
        let path = self.config("client.json", config);
        let mut command = binary();
        command
            .arg(operation)
            .arg("--config")
            .arg(path)
            .args(["--node", "lab"]);
        if let Some(alias) = alias {
            command.args(["--project", alias]);
        }
        run(command, Duration::from_secs(10))
    }
    #[cfg(unix)]
    pub fn tls(
        &self,
        address: SocketAddr,
    ) -> rustls::StreamOwned<rustls::ClientConnection, std::net::TcpStream> {
        use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer, ServerName};
        use std::sync::Arc;
        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(CertificateDer::from(
                fs::read(self.root.join("ca.der")).unwrap(),
            ))
            .unwrap();
        let config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_client_auth_cert(
            vec![CertificateDer::from(
                fs::read(self.root.join("client.der")).unwrap(),
            )],
            PrivatePkcs8KeyDer::from(fs::read(self.root.join("client.key.der")).unwrap()).into(),
        )
        .unwrap();
        let connection = rustls::ClientConnection::new(
            Arc::new(config),
            ServerName::try_from("localhost").unwrap(),
        )
        .unwrap();
        let socket =
            std::net::TcpStream::connect_timeout(&address, Duration::from_secs(2)).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        socket
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut stream = rustls::StreamOwned::new(connection, socket);
        while stream.conn.is_handshaking() {
            stream.conn.complete_io(&mut stream.sock).unwrap();
        }
        stream
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        // Only this newly created, uniquely named fixture is removed.
        let _ = fs::remove_dir_all(&self.root);
    }
}

pub fn binary() -> Command {
    Command::new(env!("CARGO_BIN_EXE_forge-agent"))
}

fn reader(mut pipe: impl Read + Send + 'static) -> JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        pipe.by_ref()
            .take(MAX_CAPTURE + 1)
            .read_to_end(&mut bytes)
            .unwrap();
        bytes
    })
}

struct Guard(Child);
impl Guard {
    fn wait(&mut self, timeout: Duration) -> Option<ExitStatus> {
        let limit = Instant::now() + timeout;
        loop {
            if let Some(status) = self.0.try_wait().expect("poll owned child") {
                return Some(status);
            }
            if Instant::now() >= limit {
                return None;
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
    fn stop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        self.stop();
    }
}

pub fn run(mut command: Command, timeout: Duration) -> Output {
    let mut child = Guard(
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn real agent executable"),
    );
    let stdout = reader(child.0.stdout.take().unwrap());
    let stderr = reader(child.0.stderr.take().unwrap());
    let status = child.wait(timeout);
    if status.is_none() {
        child.stop();
    }
    let stdout = stdout.join().unwrap();
    let stderr = stderr.join().unwrap();
    assert!(
        stdout.len() <= MAX_CAPTURE as usize && stderr.len() <= MAX_CAPTURE as usize,
        "bounded test capture exceeded"
    );
    let status = status.unwrap_or_else(|| {
        panic!(
            "owned child exceeded test deadline: {}",
            String::from_utf8_lossy(&stderr)
        )
    });
    Output {
        status,
        stdout,
        stderr,
    }
}

pub fn success(output: &Output) -> &str {
    assert!(
        output.status.success(),
        "exit {:?}: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    std::str::from_utf8(&output.stdout).expect("CLI output is UTF-8 YAML/help")
}
pub fn failure(output: &Output) -> &str {
    assert!(
        !output.status.success(),
        "unexpected success: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        output.stdout.is_empty(),
        "failure emitted partial success output"
    );
    let stderr = std::str::from_utf8(&output.stderr).unwrap();
    assert!(!stderr.is_empty(), "failure needs a diagnostic");
    assert!(!stderr.contains("PRIVATE KEY"));
    stderr
}

pub struct Server {
    child: Guard,
    stdout: Option<JoinHandle<Vec<u8>>>,
    stderr: Option<JoinHandle<Vec<u8>>>,
    #[cfg(unix)]
    diagnostics: mpsc::Receiver<Vec<u8>>,
    pub address: SocketAddr,
}
impl Server {
    fn start(config: &Path, max_connections: Option<usize>) -> Self {
        let mut command = binary();
        command.arg("serve").arg("--config").arg(config);
        if let Some(max) = max_connections {
            command.arg("--max-connections").arg(max.to_string());
        }
        let mut child = Guard(
            command
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let stderr = child.0.stderr.take().unwrap();
        let (notices, diagnostics) = mpsc::channel();
        let stderr = thread::spawn(move || {
            let mut input = BufReader::new(stderr.take(MAX_CAPTURE + 1));
            let mut captured = Vec::new();
            loop {
                let mut line = Vec::new();
                if input.read_until(b'\n', &mut line).unwrap() == 0 {
                    return captured;
                }
                captured.extend_from_slice(&line);
                let _ = notices.send(line);
            }
        });
        #[cfg(not(unix))]
        let _ = diagnostics;
        let stdout = child.0.stdout.take().unwrap();
        let (sender, receiver) = mpsc::sync_channel(1);
        let stdout = thread::spawn(move || {
            let mut input = BufReader::new(stdout);
            let mut first = String::new();
            let result = input.read_line(&mut first).map(|_| first.clone());
            let _ = sender.send(result);
            let mut rest = first.into_bytes();
            input.take(MAX_CAPTURE).read_to_end(&mut rest).unwrap();
            rest
        });
        let ready = receiver.recv_timeout(Duration::from_secs(10));
        let first = match ready {
            Ok(Ok(first)) if first.starts_with("READY ") => first,
            other => {
                child.stop();
                let diagnostic = stderr.join().unwrap();
                let _ = stdout.join();
                panic!(
                    "agent did not publish readiness ({other:?}): {}",
                    String::from_utf8_lossy(&diagnostic)
                );
            }
        };
        let address = first
            .trim()
            .strip_prefix("READY ")
            .unwrap()
            .parse()
            .expect("ready address");
        Self {
            child,
            stdout: Some(stdout),
            stderr: Some(stderr),
            #[cfg(unix)]
            diagnostics,
            address,
        }
    }
    pub fn wait_success(&mut self) {
        let status = self.child.wait(Duration::from_secs(10));
        if status.is_none() {
            self.child.stop();
        }
        let stdout = self.stdout.take().unwrap().join().unwrap();
        let stderr = self.stderr.take().unwrap().join().unwrap();
        assert!(stdout.len() <= MAX_CAPTURE as usize && stderr.len() <= MAX_CAPTURE as usize);
        let status = status.expect("server did not stop within bounded test wait");
        assert!(
            status.success(),
            "server failed: {}",
            String::from_utf8_lossy(&stderr)
        );
    }
    #[cfg(unix)]
    pub fn signal(&self, signal: nix::sys::signal::Signal) {
        let pid = nix::unistd::Pid::from_raw(self.child.0.id().try_into().unwrap());
        nix::sys::signal::kill(pid, signal).expect("signal this test's child only");
    }
    #[cfg(unix)]
    pub fn wait_stop_requested(&self) {
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            let remaining = end.saturating_duration_since(Instant::now());
            let line = self
                .diagnostics
                .recv_timeout(remaining)
                .expect("signal handler must acknowledge its stored stop flag");
            if line == b"STOP REQUESTED\n" {
                return;
            }
        }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.child.stop();
        if let Some(reader) = self.stdout.take() {
            let _ = reader.join();
        }
        if let Some(reader) = self.stderr.take() {
            let _ = reader.join();
        }
    }
}
