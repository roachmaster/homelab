//! Transient local demonstration, not a persistent daemon or credential setup.
use forge_bootstrap::Registry;
use forge_ingress_contract::Header;
use forge_io_client::IoClient;
use forge_kernel::node::{self, NodeClient, NodeCommand, NodeConfig, NodeResponse, NodeTask};
use forge_provider::{Factory, InstanceProvider, Singleton};
use forge_remote::{
    now_unix_ms, Destination, NodeCall, NodeInfo, NodeReply, ReadNodeInfo, ReadNodeInput,
    ReadNodeTask, RemoteError, Target,
};
use forge_remote_https::{
    register_client, register_server, CertificateIdentity, ClientConfig, Credentials, HttpsClient,
    Limits, ServerConfig,
};
use forge_task::Task;
use rcgen::{
    BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};
use std::{error::Error, sync::mpsc, thread, time::Duration};

struct Observation(NodeTask<NodeClient>);
impl IoClient for Observation {
    type Request = ReadNodeInfo;
    type Response = NodeInfo;
    type Error = RemoteError;
    fn execute(&self, _: ReadNodeInfo) -> Result<NodeInfo, RemoteError> {
        match self
            .0
            .run(NodeCommand::Info)
            .map_err(|_| RemoteError::Unavailable)?
        {
            NodeResponse::Info(value) => Ok(NodeInfo {
                node: value.id,
                target_os: value.target_os.into(),
                target_arch: value.target_arch.into(),
            }),
            NodeResponse::Status(_) => Err(RemoteError::InvalidResponse),
        }
    }
}
// Application-owned acquisition wrapper; the generic task does not know the
// provider or concrete HTTPS construction policy.
struct ClientHandle<'a>(&'a Singleton<HttpsClient>);
impl IoClient for ClientHandle<'_> {
    type Request = NodeCall;
    type Response = NodeReply;
    type Error = RemoteError;
    fn execute(&self, request: NodeCall) -> Result<NodeReply, RemoteError> {
        self.0
            .acquire()
            .expect("infallible singleton lease")
            .execute(request)
    }
}
fn main() -> Result<(), Box<dyn Error>> {
    // Ephemeral test/demo trust only: never written, installed or printed.
    let mut ca = CertificateParams::new(Vec::<String>::new())?;
    ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    let key = KeyPair::generate()?;
    let root = ca.self_signed(&key)?.der().to_vec();
    let issuer = Issuer::new(ca, key);
    let leaf = |name: &str, usage| -> Result<Credentials, Box<dyn Error>> {
        let mut params = CertificateParams::new(vec![name.into()])?;
        params.extended_key_usages = vec![usage];
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        let key = KeyPair::generate()?;
        let cert = params.signed_by(&key, &issuer)?;
        Ok(Credentials {
            certificate_chain: vec![cert.der().to_vec()],
            private_key_pkcs8: key.serialize_der(),
        })
    };
    let server_credentials = leaf("localhost", ExtendedKeyUsagePurpose::ServerAuth)?;
    let client_credentials = leaf("demo-client", ExtendedKeyUsagePurpose::ClientAuth)?;
    let target = Target::new("demo-node", "ephemeral-host")?;
    let server_target = target.clone();
    let server_config = ServerConfig::LoopbackMutualTls {
        address: "127.0.0.1:0".parse()?,
        client_roots: vec![root.clone()],
        credentials: server_credentials,
        identities: vec![CertificateIdentity {
            certificate_der: client_credentials.certificate_chain[0].clone(),
            subject: "demo-operator".into(),
        }],
        limits: Limits::default(),
    };
    let (sender, receiver) = mpsc::channel();
    let server = thread::spawn(move || -> Result<(), String> {
        let plan = Registry::new()
            .register(
                node::register(NodeConfig::Local {
                    id: server_target.node().into(),
                })
                .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?
            .register(
                register_server("remote.https.server", server_config).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
        let (((), node), server) = plan.freeze().map_err(|e| e.to_string())?.into_factories();
        let observation = Observation(NodeTask::new(
            node.client.create().map_err(|e| e.to_string())?,
        ));
        let provider = Singleton::new(server.create().map_err(|e| e.to_string())?);
        let service = provider.acquire().map_err(|e| e.to_string())?;
        let destination = Destination::new(
            server_target,
            observation,
            |subject: &str, _: &Target, header: &Header| {
                subject == "demo-operator" && header.route().destination() == "node.info"
            },
        );
        sender
            .send(service.local_addr().map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        service.serve_one(&destination).map_err(|e| e.to_string())
    });
    let result = (|| -> Result<NodeReply, Box<dyn Error>> {
        let address = receiver.recv_timeout(Duration::from_secs(5))?;
        let config = ClientConfig::MutualTls {
            address,
            server_name: "localhost".into(),
            target: target.clone(),
            roots: vec![root],
            credentials: client_credentials,
            limits: Limits::default(),
        };
        let ((), factory) = Registry::new()
            .register(register_client("remote.https.client", config)?)
            .map_err(|e| e.to_string())?
            .freeze()
            .map_err(|e| e.to_string())?
            .into_factories();
        let provider = Singleton::new(factory.create()?);
        let task = ReadNodeTask::new(ClientHandle(&provider));
        Ok(task.run(ReadNodeInput {
            target,
            request_id: "demo-read-1".into(),
            correlation_id: None,
            expires_unix_ms: now_unix_ms()? + 2000,
        })?)
    })();
    // Join on both success and failure; no detached serving owner is left behind.
    let server_result = server.join().map_err(|_| "server panicked");
    let reply = result?;
    server_result??;
    println!(
        "Authenticated node.info: node={} os={} arch={} request={}",
        reply.responder.node(),
        reply.response.payload.target_os,
        reply.response.payload.target_arch,
        reply.response.header.request_id()
    );
    println!("Transient loopback demonstration ended; no credentials saved or listener retained.");
    Ok(())
}
