use forge_remote::Target;
use forge_remote_https::{CertificateIdentity, ClientConfig, Credentials, Limits, ServerConfig};
use rcgen::{
    BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};
use std::net::SocketAddr;

/// Test-only ephemeral material. No key/certificate is written or installed.
pub struct Certificates {
    pub root: Vec<u8>,
    pub server: Credentials,
    pub client: Credentials,
}
impl Certificates {
    pub fn new() -> Self {
        let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::DigitalSignature,
        ];
        let key = KeyPair::generate().unwrap();
        let root = params.self_signed(&key).unwrap().der().to_vec();
        let issuer = Issuer::new(params, key);
        let leaf = |name: &str, usage| {
            let mut params = CertificateParams::new(vec![name.into()]).unwrap();
            params.extended_key_usages = vec![usage];
            params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
            let key = KeyPair::generate().unwrap();
            let certificate = params.signed_by(&key, &issuer).unwrap();
            Credentials {
                certificate_chain: vec![certificate.der().to_vec()],
                private_key_pkcs8: key.serialize_der(),
            }
        };
        Self {
            root,
            server: leaf("localhost", ExtendedKeyUsagePurpose::ServerAuth),
            client: leaf("client.test", ExtendedKeyUsagePurpose::ClientAuth),
        }
    }
    pub fn server_config(&self, limits: Limits) -> ServerConfig {
        ServerConfig::LoopbackMutualTls {
            address: "127.0.0.1:0".parse().unwrap(),
            client_roots: vec![self.root.clone()],
            credentials: self.server.clone(),
            identities: vec![CertificateIdentity {
                certificate_der: self.client.certificate_chain[0].clone(),
                subject: "operator".into(),
            }],
            limits,
        }
    }
    pub fn client_config(&self, address: SocketAddr, limits: Limits) -> ClientConfig {
        ClientConfig::MutualTls {
            address,
            server_name: "localhost".into(),
            target: target(),
            roots: vec![self.root.clone()],
            credentials: self.client.clone(),
            limits,
        }
    }
}
pub fn target() -> Target {
    Target::new("local-node", "test-host-1").unwrap()
}
