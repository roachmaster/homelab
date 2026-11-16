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
const DESIGN_HTML: &[u8] = include_bytes!("../design/index.html");
const DESIGN_CSS: &[u8] = include_bytes!("../design/style.css");
const PREVIOUS_CHATS_JS: &[u8] = include_bytes!("../design/previous-chats.js");
const DESIGN_IMAGES: [(&str, &[u8]); 5] = [
    (
        "forge-ide-chat-desktop.png",
        include_bytes!("../design/forge-ide-chat-desktop.png"),
    ),
    (
        "forge-ide-chat-phone.png",
        include_bytes!("../design/forge-ide-chat-phone.png"),
    ),
    (
        "forge-ide-workbench-desktop.png",
        include_bytes!("../design/forge-ide-workbench-desktop.png"),
    ),
    (
        "forge-ide-workbench-phone.png",
        include_bytes!("../design/forge-ide-workbench-phone.png"),
    ),
    (
        "forge-ide-workbench-project-options.png",
        include_bytes!("../design/forge-ide-workbench-project-options.png"),
    ),
];

fn design_asset(path: &str) -> Option<(&'static str, &'static [u8])> {
    match path {
        "/design" | "/design/" => Some(("text/html; charset=utf-8", DESIGN_HTML)),
        "/design/style.css" => Some(("text/css; charset=utf-8", DESIGN_CSS)),
        "/design/previous-chats.js" => Some(("text/javascript; charset=utf-8", PREVIOUS_CHATS_JS)),
        _ => {
            let name = path.strip_prefix("/design/")?;
            DESIGN_IMAGES
                .iter()
                .find(|(filename, _)| *filename == name)
                .map(|(_, data)| ("image/png", *data))
        }
    }
}
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
        if m.format_version != 1 || m.abi_version != 4 || m.files.len() != 5 {
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
        self.0
            .get(name)
            .map(|(mime, b)| (*mime, b.as_slice()))
            .or_else(|| design_asset(path))
    }
}
