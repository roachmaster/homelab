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
const DESIGN_HTML: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/design.html"));
const DESIGN_CSS: &[u8] = include_bytes!("../design/style.css");
const PREVIOUS_CHATS_JS: &[u8] = include_bytes!("../design/previous-chats.js");
const CHAT_HISTORY_HTML: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/chat-history.html"));
const CHAT_HISTORY_CSS: &[u8] = include_bytes!("../design/chat-history.css");
const CHAT_DISPATCH_HTML: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/chat-dispatch.html"));
const CHAT_DISPATCH_SEQUENCE: &[u8] = include_bytes!("../design/chat-dispatch-sequence.svg");
const MAIL_HTML: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/mail.html"));
const MAIL_WIREFRAME: &[u8] = include_bytes!("../design/forge-ide-mail-wireframe.svg");
const MAIL_SEQUENCE: &[u8] = include_bytes!("../design/mail-dispatch-sequence.svg");
const GAME_STUDIO_HTML: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/game-studio.html"));
const GAME_STUDIO_CSS: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/game-studio.css"));
const GAME_ENGINE_ARCHITECTURE: &[u8] = include_bytes!("../design/game-engine-architecture.svg");
const GAME_ENGINE_BUILD: &[u8] = include_bytes!("../design/game-engine-build-sequence.svg");
const GAME_PHYSICS_HTML: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/game-physics.html"));
const GAME_PHYSICS_TICK: &[u8] =
    include_bytes!("../../../../docs/architecture/diagrams/mermaid/images/game-physics-tick.svg");
const GAME_PHYSICS_ANIMATION: &[u8] = include_bytes!(
    "../../../../docs/architecture/diagrams/mermaid/images/game-physics-animation.svg"
);
const GAME_PHYSICS_AUTHORITY: &[u8] = include_bytes!(
    "../../../../docs/architecture/diagrams/mermaid/images/game-physics-authority.svg"
);
const PLAYTEST_HTML: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/playtest.html"));
const PLAYTEST_CSS: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/playtest.css"));
const PLAYTEST_JS: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/playtest.js"));
const DESIGN_IMAGES: [(&str, &[u8]); 10] = [
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
    (
        "chat-history-open.png",
        include_bytes!("../design/chat-history-open.png"),
    ),
    (
        "chat-history-retention.png",
        include_bytes!("../design/chat-history-retention.png"),
    ),
    (
        "chat-history-delete.png",
        include_bytes!("../design/chat-history-delete.png"),
    ),
    (
        "chat-history-expiry.png",
        include_bytes!("../design/chat-history-expiry.png"),
    ),
    (
        "forge-ide-mail-wireframe.png",
        include_bytes!("../design/forge-ide-mail-wireframe.png"),
    ),
];

fn embedded_asset(path: &str) -> Option<(&'static str, &'static [u8])> {
    match path {
        "/design" | "/design/" => Some(("text/html; charset=utf-8", DESIGN_HTML)),
        "/design/style.css" => Some(("text/css; charset=utf-8", DESIGN_CSS)),
        "/design/previous-chats.js" => Some(("text/javascript; charset=utf-8", PREVIOUS_CHATS_JS)),
        "/design/chat-history" | "/design/chat-history/" => {
            Some(("text/html; charset=utf-8", CHAT_HISTORY_HTML))
        }
        "/design/chat-history.css" => Some(("text/css; charset=utf-8", CHAT_HISTORY_CSS)),
        "/design/chat-dispatch" | "/design/chat-dispatch/" => {
            Some(("text/html; charset=utf-8", CHAT_DISPATCH_HTML))
        }
        "/design/chat-dispatch-sequence.svg" => Some(("image/svg+xml", CHAT_DISPATCH_SEQUENCE)),
        "/design/mail" | "/design/mail/" => Some(("text/html; charset=utf-8", MAIL_HTML)),
        "/design/forge-ide-mail-wireframe.svg" => Some(("image/svg+xml", MAIL_WIREFRAME)),
        "/design/mail-dispatch-sequence.svg" => Some(("image/svg+xml", MAIL_SEQUENCE)),
        "/design/game-studio" | "/design/game-studio/" => {
            Some(("text/html; charset=utf-8", GAME_STUDIO_HTML))
        }
        "/design/game-studio.css" => Some(("text/css; charset=utf-8", GAME_STUDIO_CSS)),
        "/design/game-engine-architecture.svg" => Some(("image/svg+xml", GAME_ENGINE_ARCHITECTURE)),
        "/design/game-engine-build-sequence.svg" => Some(("image/svg+xml", GAME_ENGINE_BUILD)),
        "/design/game-physics" | "/design/game-physics/" => {
            Some(("text/html; charset=utf-8", GAME_PHYSICS_HTML))
        }
        "/design/game-physics/tick.svg" => Some(("image/svg+xml", GAME_PHYSICS_TICK)),
        "/design/game-physics/animation.svg" => Some(("image/svg+xml", GAME_PHYSICS_ANIMATION)),
        "/design/game-physics/authority.svg" => Some(("image/svg+xml", GAME_PHYSICS_AUTHORITY)),
        "/playtest" | "/playtest/" => Some(("text/html; charset=utf-8", PLAYTEST_HTML)),
        "/playtest/style.css" => Some(("text/css; charset=utf-8", PLAYTEST_CSS)),
        "/playtest/main.js" => Some(("text/javascript; charset=utf-8", PLAYTEST_JS)),
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
            .or_else(|| embedded_asset(path))
    }
}
