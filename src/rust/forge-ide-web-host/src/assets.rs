use crate::HostError;
use forge_ide::filesystem::{FsRequest, FsResponse, NativeFiles};
use forge_io_client::IoClient;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::Path};
const NAMES: [(&str, &str); 5] = [
    ("index.html", "text/html; charset=utf-8"),
    ("style.css", "text/css; charset=utf-8"),
    ("main.js", "text/javascript; charset=utf-8"),
    ("worker.js", "text/javascript; charset=utf-8"),
    ("app.wasm", "application/wasm"),
];
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    format_version: u16,
    abi_version: u16,
    files: Vec<Asset>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Asset {
    path: String,
    sha256: String,
}
pub(crate) struct Assets(BTreeMap<String, (&'static str, Vec<u8>)>);
impl Assets {
    pub fn load(root: &Path) -> Result<Self, HostError> {
        let files = NativeFiles::open(root).map_err(|_| HostError::Configuration("asset root"))?;
        let read = |name: &str, max_bytes: usize| -> Result<Vec<u8>, HostError> {
            match files
                .execute(FsRequest::Read {
                    path: name.into(),
                    max_bytes,
                })
                .map_err(|_| HostError::Configuration("asset regular file"))?
            {
                FsResponse::Read(b) => Ok(b),
                _ => Err(HostError::Assembly),
            }
        };
        let m: Manifest = serde_json::from_slice(&read("manifest.json", 16384)?)
            .map_err(|_| HostError::Configuration("asset manifest"))?;
        if m.format_version != 1 || m.abi_version != 2 || m.files.len() != 5 {
            return Err(HostError::Configuration("asset versions/count"));
        }
        let mut loaded = BTreeMap::new();
        for f in m.files {
            let Some((_, mime)) = NAMES.iter().find(|(n, _)| *n == f.path) else {
                return Err(HostError::Configuration("asset path"));
            };
            let bytes = read(&f.path, 16 * 1024 * 1024)?;
            let digest = format!("{:x}", Sha256::digest(&bytes));
            if f.sha256 != digest || loaded.insert(f.path, (*mime, bytes)).is_some() {
                return Err(HostError::Configuration("asset hash/duplicate"));
            }
        }
        Ok(Self(loaded))
    }
    pub fn get(&self, path: &str) -> Option<(&'static str, &[u8])> {
        let name = if path == "/" {
            "index.html"
        } else {
            path.strip_prefix('/')?
        };
        self.0.get(name).map(|(mime, b)| (*mime, b.as_slice()))
    }
}
