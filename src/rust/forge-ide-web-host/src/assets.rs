use crate::HostError;
use forge_core::io_client::IoClient;
use forge_core::provider::Factory;
use forge_ide::filesystem::{FsError, FsRequest, FsResponse, NativeFiles};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
};
const NAMES: [(&str, &str); 5] = [
    ("index.html", "text/html; charset=utf-8"),
    ("style.css", "text/css; charset=utf-8"),
    ("main.js", "text/javascript; charset=utf-8"),
    ("worker.js", "text/javascript; charset=utf-8"),
    ("app.wasm", "application/wasm"),
];
const HOME_HTML: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/home.html"));
const HOME_CSS: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/home.css"));
const DESIGN_HTML: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/design.html"));
const PORTAL_NAVIGATION_SEQUENCE: &[u8] =
    include_bytes!("../../../../docs/architecture/diagrams/mermaid/images/m29-ui-docs-handoff.svg");
const CHARACTERS_HTML: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/characters.html"));
const CHARACTERS_CSS: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/characters.css"));
const IMPLEMENTATION_HTML: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/implementation.html"));
const PREVIOUS_CHATS_HTML: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/previous-chats.html"));
const WIREFRAMES_HTML: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/wireframes.html"));
const WORKSPACE_HTML: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/workspace.html"));
const DOCS_CSS: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/docs.css"));
const DESIGN_CSS: &[u8] = include_bytes!("../design/style.css");
const DESIGN_INDEX_CSS: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/design-index.css"));
const FORGE_PROVIDERS_HTML: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/forge-providers.html"));
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
const STUDIO_HTML: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/studio.html"));
const STUDIO_CSS: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/studio.css"));
const STUDIO_JS: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/studio.js"));
const STUDIO_WORKER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/studio-worker.js"));
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
const GAME_ASSET_PIPELINE_HTML: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/game-asset-pipeline.html"));
const GAME_ASSET_PIPELINE_COOK: &[u8] = include_bytes!(
    "../../../../docs/architecture/diagrams/mermaid/images/game-asset-pipeline-cook.svg"
);
const GAME_ASSET_PIPELINE_LOAD: &[u8] = include_bytes!(
    "../../../../docs/architecture/diagrams/mermaid/images/game-asset-pipeline-load.svg"
);
const GAME_ASSET_PIPELINE_CONTRACT: &[u8] = include_bytes!(
    "../../../../docs/architecture/diagrams/mermaid/images/game-asset-pipeline-contract.svg"
);
const FORGE_WORKERS_HTML: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/forge-workers.html"));
const FORGE_WORKERS_PLAYTEST_HTML: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/forge-workers-playtest.html"));
const FORGE_WORKERS_STUDIO_HTML: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/forge-workers-studio.html"));
const FORGE_WORKERS_OWNERSHIP: &[u8] = include_bytes!(
    "../../../../docs/architecture/diagrams/mermaid/images/forge-worker-ownership.svg"
);
const FORGE_WORKERS_PLAYTEST: &[u8] = include_bytes!(
    "../../../../docs/architecture/diagrams/mermaid/images/forge-game-worker-handoff.svg"
);
const FORGE_WORKERS_STUDIO: &[u8] = include_bytes!(
    "../../../../docs/architecture/diagrams/mermaid/images/forge-studio-worker-lease.svg"
);
const CODING_AGENTS_HTML: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/coding-agents.html"));
const AGENT_FEEDBACK_HTML: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/agent-feedback-loop.html"));
const AGENT_AUTONOMY_HTML: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/autonomous-agents.html"));
const AGENT_USAGE_HTML: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/agent-usage.html"));
const CONTENT_IDENTITY_HTML: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/content-identity.html"));
const CONTENT_IDENTITY_DATA: &[u8] = include_bytes!(
    "../../../../docs/architecture/diagrams/mermaid/images/content-identity-data.svg"
);
const CONTENT_IDENTITY_COMPONENTS: &[u8] = include_bytes!(
    "../../../../docs/architecture/diagrams/mermaid/images/content-identity-components.svg"
);
const CONTENT_IDENTITY_NATIVE: &[u8] = include_bytes!(
    "../../../../docs/architecture/diagrams/mermaid/images/content-identity-native.svg"
);
const CONTENT_IDENTITY_EXTERNAL: &[u8] = include_bytes!(
    "../../../../docs/architecture/diagrams/mermaid/images/content-identity-external.svg"
);
const CODING_AGENT_OWNERSHIP: &[u8] = include_bytes!(
    "../../../../docs/architecture/diagrams/mermaid/images/coding-agent-ownership.svg"
);
const CODING_AGENT_LIFECYCLE: &[u8] = include_bytes!(
    "../../../../docs/architecture/diagrams/mermaid/images/coding-agent-lifecycle.svg"
);
const CODING_AGENT_COMPLETION: &[u8] = include_bytes!(
    "../../../../docs/architecture/diagrams/mermaid/images/coding-agent-completion.svg"
);
const CODING_AGENT_TAKEOVER: &[u8] = include_bytes!(
    "../../../../docs/architecture/diagrams/mermaid/images/coding-agent-takeover.svg"
);
const AGENT_USAGE_PROVIDERS: &[u8] = include_bytes!(
    "../../../../docs/architecture/diagrams/mermaid/images/agent-usage-providers.svg"
);
const AGENT_USAGE_COLLECTION: &[u8] = include_bytes!(
    "../../../../docs/architecture/diagrams/mermaid/images/agent-usage-collection.svg"
);
const PLAYTEST_HTML: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/playtest.html"));
const PLAYTEST_CSS: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/playtest.css"));
const PLAYTEST_JS: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/playtest.js"));
const PLAYTEST_ASSETS_WORKER_JS: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/playtest-assets-worker.js"));
const DESIGN_IMAGES: [(&str, &[u8]); 15] = [
    (
        "forge-ide-chat-history-phone.png",
        include_bytes!("../design/forge-ide-chat-history-phone.png"),
    ),
    (
        "forge-ide-workbench-explorer-phone.png",
        include_bytes!("../design/forge-ide-workbench-explorer-phone.png"),
    ),
    (
        "forge-ide-mail-inbox-phone.png",
        include_bytes!("../design/forge-ide-mail-inbox-phone.png"),
    ),
    (
        "forge-ide-mail-thread-phone.png",
        include_bytes!("../design/forge-ide-mail-thread-phone.png"),
    ),
    (
        "forge-ide-mail-thread-desktop.png",
        include_bytes!("../design/forge-ide-mail-thread-desktop.png"),
    ),
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
        "/home" | "/home/" => Some(("text/html; charset=utf-8", HOME_HTML)),
        "/design/home.css" => Some(("text/css; charset=utf-8", HOME_CSS)),
        "/design" | "/design/" => Some(("text/html; charset=utf-8", DESIGN_HTML)),
        "/design/portal-navigation-sequence.svg" => {
            Some(("image/svg+xml", PORTAL_NAVIGATION_SEQUENCE))
        }
        "/design/characters" | "/design/characters/" => {
            Some(("text/html; charset=utf-8", CHARACTERS_HTML))
        }
        "/design/characters.css" => Some(("text/css; charset=utf-8", CHARACTERS_CSS)),
        "/design/implementation" | "/design/implementation/" => {
            Some(("text/html; charset=utf-8", IMPLEMENTATION_HTML))
        }
        "/design/previous-chats" | "/design/previous-chats/" => {
            Some(("text/html; charset=utf-8", PREVIOUS_CHATS_HTML))
        }
        "/design/wireframes" | "/design/wireframes/" => {
            Some(("text/html; charset=utf-8", WIREFRAMES_HTML))
        }
        "/design/workspace" | "/design/workspace/" => {
            Some(("text/html; charset=utf-8", WORKSPACE_HTML))
        }
        "/design/docs.css" => Some(("text/css; charset=utf-8", DOCS_CSS)),
        "/design/style.css" => Some(("text/css; charset=utf-8", DESIGN_CSS)),
        "/design/index.css" => Some(("text/css; charset=utf-8", DESIGN_INDEX_CSS)),
        "/design/forge-providers" | "/design/forge-providers/" => {
            Some(("text/html; charset=utf-8", FORGE_PROVIDERS_HTML))
        }
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
        "/studio" | "/studio/" => Some(("text/html; charset=utf-8", STUDIO_HTML)),
        "/studio/style.css" => Some(("text/css; charset=utf-8", STUDIO_CSS)),
        "/studio/main.js" => Some(("text/javascript; charset=utf-8", STUDIO_JS)),
        "/studio/worker.js" => Some(("text/javascript; charset=utf-8", STUDIO_WORKER)),
        "/design/game-engine-architecture.svg" => Some(("image/svg+xml", GAME_ENGINE_ARCHITECTURE)),
        "/design/game-engine-build-sequence.svg" => Some(("image/svg+xml", GAME_ENGINE_BUILD)),
        "/design/game-physics" | "/design/game-physics/" => {
            Some(("text/html; charset=utf-8", GAME_PHYSICS_HTML))
        }
        "/design/game-physics/tick.svg" => Some(("image/svg+xml", GAME_PHYSICS_TICK)),
        "/design/game-physics/animation.svg" => Some(("image/svg+xml", GAME_PHYSICS_ANIMATION)),
        "/design/game-physics/authority.svg" => Some(("image/svg+xml", GAME_PHYSICS_AUTHORITY)),
        "/design/game-asset-pipeline" | "/design/game-asset-pipeline/" => {
            Some(("text/html; charset=utf-8", GAME_ASSET_PIPELINE_HTML))
        }
        "/design/game-asset-pipeline/cook.svg" => Some(("image/svg+xml", GAME_ASSET_PIPELINE_COOK)),
        "/design/game-asset-pipeline/load.svg" => Some(("image/svg+xml", GAME_ASSET_PIPELINE_LOAD)),
        "/design/game-asset-pipeline/contract.svg" => {
            Some(("image/svg+xml", GAME_ASSET_PIPELINE_CONTRACT))
        }
        "/design/forge-workers" | "/design/forge-workers/" => {
            Some(("text/html; charset=utf-8", FORGE_WORKERS_HTML))
        }
        "/design/forge-workers/playtest" | "/design/forge-workers/playtest/" => {
            Some(("text/html; charset=utf-8", FORGE_WORKERS_PLAYTEST_HTML))
        }
        "/design/forge-workers/studio" | "/design/forge-workers/studio/" => {
            Some(("text/html; charset=utf-8", FORGE_WORKERS_STUDIO_HTML))
        }
        "/design/forge-workers/ownership.svg" => Some(("image/svg+xml", FORGE_WORKERS_OWNERSHIP)),
        "/design/forge-workers/playtest.svg" => Some(("image/svg+xml", FORGE_WORKERS_PLAYTEST)),
        "/design/forge-workers/studio.svg" => Some(("image/svg+xml", FORGE_WORKERS_STUDIO)),
        "/design/coding-agents" | "/design/coding-agents/" => {
            Some(("text/html; charset=utf-8", CODING_AGENTS_HTML))
        }
        "/design/coding-agents/ownership.svg" => Some(("image/svg+xml", CODING_AGENT_OWNERSHIP)),
        "/design/coding-agents/lifecycle.svg" => Some(("image/svg+xml", CODING_AGENT_LIFECYCLE)),
        "/design/coding-agents/completion.svg" => Some(("image/svg+xml", CODING_AGENT_COMPLETION)),
        "/design/coding-agents/takeover.svg" => Some(("image/svg+xml", CODING_AGENT_TAKEOVER)),
        "/design/agent-feedback-loop" | "/design/agent-feedback-loop/" => {
            Some(("text/html; charset=utf-8", AGENT_FEEDBACK_HTML))
        }
        "/design/autonomous-agents" | "/design/autonomous-agents/" => {
            Some(("text/html; charset=utf-8", AGENT_AUTONOMY_HTML))
        }
        "/design/agent-usage" | "/design/agent-usage/" => {
            Some(("text/html; charset=utf-8", AGENT_USAGE_HTML))
        }
        "/design/content-identity" | "/design/content-identity/" => {
            Some(("text/html; charset=utf-8", CONTENT_IDENTITY_HTML))
        }
        "/design/content-identity/data.svg" => Some(("image/svg+xml", CONTENT_IDENTITY_DATA)),
        "/design/content-identity/components.svg" => {
            Some(("image/svg+xml", CONTENT_IDENTITY_COMPONENTS))
        }
        "/design/content-identity/native.svg" => Some(("image/svg+xml", CONTENT_IDENTITY_NATIVE)),
        "/design/content-identity/external.svg" => {
            Some(("image/svg+xml", CONTENT_IDENTITY_EXTERNAL))
        }
        "/design/agent-usage/providers.svg" => Some(("image/svg+xml", AGENT_USAGE_PROVIDERS)),
        "/design/agent-usage/collection.svg" => Some(("image/svg+xml", AGENT_USAGE_COLLECTION)),
        "/playtest" | "/playtest/" => Some(("text/html; charset=utf-8", PLAYTEST_HTML)),
        "/playtest/style.css" => Some(("text/css; charset=utf-8", PLAYTEST_CSS)),
        "/playtest/main.js" => Some(("text/javascript; charset=utf-8", PLAYTEST_JS)),
        "/playtest/assets-worker.js" => {
            Some(("text/javascript; charset=utf-8", PLAYTEST_ASSETS_WORKER_JS))
        }
        path if path.starts_with("/design/characters/") => {
            let name = path.strip_prefix("/design/characters/")?;
            crate::character_assets::ASSETS
                .iter()
                .find(|(file, _, _)| *file == name)
                .map(|(_, content_type, bytes)| (*content_type, *bytes))
        }
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CookedManifest {
    version: u16,
    entries: CookedEntries,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CookedEntries {
    #[serde(rename = "room-one")]
    room_one: Option<CookedAsset>,
    #[serde(rename = "traversal-yard")]
    traversal_yard: Option<CookedAsset>,
    #[serde(rename = "scout-atlas")]
    atlas: Option<CookedAsset>,
    #[serde(rename = "scout-atlas-image")]
    image: Option<CookedAsset>,
}

#[derive(Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum CookedKind {
    Level,
    Atlas,
    Image,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CookedAsset {
    kind: CookedKind,
    path: String,
    sha256: String,
    dependencies: Vec<String>,
    optional_dependencies: Vec<String>,
}

impl CookedAsset {
    fn validate(
        &self,
        path: &str,
        kind: CookedKind,
        dependencies: &[&str],
        optional: &[&str],
    ) -> Result<(), HostError> {
        if self.path != path
            || self.kind != kind
            || self.dependencies != dependencies
            || self.optional_dependencies != optional
            || self.sha256.len() != 64
            || !self
                .sha256
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        {
            return Err(HostError::Configuration("cooked asset manifest entry"));
        }
        Ok(())
    }

    fn load(
        &self,
        files: &NativeFiles,
        alternate: Option<&NativeFiles>,
        limit: usize,
    ) -> Result<Vec<u8>, HostError> {
        let primary = read_file(files, &format!("game-assets/{}", self.path), limit)?;
        let bytes = match (primary, alternate) {
            (None, Some(files)) => read_file(files, &self.path, limit)?,
            (primary, _) => primary,
        }
        .ok_or(HostError::Configuration("cooked asset missing"))?;
        if !matches_digest(&bytes, &self.sha256) {
            return Err(HostError::Configuration("cooked asset hash"));
        }
        Ok(bytes)
    }
}

fn read_file(files: &NativeFiles, name: &str, limit: usize) -> Result<Option<Vec<u8>>, HostError> {
    match files.execute(FsRequest::Read {
        path: name.into(),
        max_bytes: limit,
    }) {
        Ok(FsResponse::Read(bytes)) => Ok(Some(bytes)),
        Ok(FsResponse::Listed(_)) => Err(HostError::Assembly),
        Err(FsError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(HostError::Configuration("asset regular file")),
    }
}

fn matches_digest(bytes: &[u8], expected: &str) -> bool {
    format!("{:x}", Sha256::digest(bytes)) == expected
}

const JSON_MIME: &str = "application/json";
const GAME_MANIFEST: &str = "game-assets/manifest.json";
const GAME_ROUTE: &str = "playtest/assets/";
const REPLAY_ROUTE: &str = "/playtest/assets/archive/";
const MAX_LEVEL_BYTES: usize = 16 * 1024;
const MAX_ATLAS_BYTES: usize = 1024 * 1024;
const MAX_PNG_BYTES: usize = 8 * 1024 * 1024;

pub(crate) struct Assets {
    files: BTreeMap<String, (&'static str, Vec<u8>)>,
    replay_archive: Option<NativeFiles>,
}

#[derive(Clone)]
pub(crate) struct AssetFactory {
    pub root: PathBuf,
    pub alternate_cooked: Option<PathBuf>,
    pub replay_archive: Option<PathBuf>,
}

impl Factory<Assets> for AssetFactory {
    type Error = HostError;

    fn create(&self) -> Result<Assets, HostError> {
        Assets::load(&self.root, self.alternate_cooked.as_deref())?
            .with_replay_archive(self.replay_archive.as_deref())
    }
}

impl Assets {
    pub fn load(root: &Path, alternate_cooked: Option<&Path>) -> Result<Self, HostError> {
        let files = NativeFiles::open(root).map_err(|_| HostError::Configuration("asset root"))?;
        let read = |name: &str, limit| {
            read_file(&files, name, limit)?.ok_or(HostError::Configuration("asset regular file"))
        };
        let m: Manifest = serde_json::from_slice(&read("manifest.json", 16384)?)
            .map_err(|_| HostError::Configuration("asset manifest"))?;
        if m.format_version != 1 || m.abi_version != 19 || m.files.len() != 5 {
            return Err(HostError::Configuration("asset versions/count"));
        }
        let mut loaded = BTreeMap::new();
        for f in m.files {
            let Some((_, mime)) = NAMES.iter().find(|(n, _)| *n == f.path) else {
                return Err(HostError::Configuration("asset path"));
            };
            let bytes = read(&f.path, 16 * 1024 * 1024)?;
            if !matches_digest(&bytes, &f.sha256) || loaded.insert(f.path, (*mime, bytes)).is_some()
            {
                return Err(HostError::Configuration("asset hash/duplicate"));
            }
        }
        // A game bundle is independent of the required Forge UI assets.
        // A bad sidecar cannot prevent the host's other capabilities starting.
        if let Ok(game) = Self::load_game(&files, alternate_cooked) {
            loaded.extend(game.files);
        }
        Ok(Self {
            files: loaded,
            replay_archive: None,
        })
    }

    fn load_game(files: &NativeFiles, alternate: Option<&Path>) -> Result<Self, HostError> {
        let mut game = Self {
            files: BTreeMap::new(),
            replay_archive: None,
        };
        let Some(bytes) = read_file(files, GAME_MANIFEST, 16_384)? else {
            return Ok(game);
        };
        let manifest: CookedManifest = serde_json::from_slice(&bytes)
            .map_err(|_| HostError::Configuration("cooked asset manifest"))?;
        if manifest.version != 1 {
            return Err(HostError::Configuration("cooked asset version"));
        }
        let CookedEntries {
            room_one,
            traversal_yard,
            atlas,
            image,
        } = manifest.entries;
        if room_one.is_none() && traversal_yard.is_none() {
            return Err(HostError::Configuration("cooked asset levels"));
        }
        let alternate = alternate.and_then(|root| NativeFiles::open(root).ok());
        for (level, path) in [
            (room_one, "levels/room-one.json"),
            (traversal_yard, "levels/traversal-yard.json"),
        ] {
            if let Some(level) = level {
                level.validate(path, CookedKind::Level, &[], &["scout-atlas"])?;
                if let Ok(bytes) = level.load(files, alternate.as_ref(), MAX_LEVEL_BYTES) {
                    game.add_game(path, JSON_MIME, bytes);
                }
            }
        }
        if let Some(atlas) = &atlas {
            atlas.validate(
                "atlas/manifest.json",
                CookedKind::Atlas,
                &["scout-atlas-image"],
                &[],
            )?;
        }
        if let Some(image) = &image {
            image.validate("atlas/sheet.png", CookedKind::Image, &[], &[])?;
        }
        if let (Some(atlas), Some(image)) = (atlas, image) {
            // Expose the visual dependency only as a verified pair. A missing
            // or damaged optional file leaves the primitive renderer usable.
            let atlas_bytes = atlas.load(files, alternate.as_ref(), MAX_ATLAS_BYTES);
            let image_bytes = image.load(files, alternate.as_ref(), MAX_PNG_BYTES);
            if let (Ok(atlas_bytes), Ok(image_bytes)) = (atlas_bytes, image_bytes) {
                game.add_game(&atlas.path, JSON_MIME, atlas_bytes);
                game.add_game(&image.path, "image/png", image_bytes);
            }
        }
        game.add_game("manifest.json", JSON_MIME, bytes);
        Ok(game)
    }

    fn add_game(&mut self, path: &str, mime: &'static str, bytes: Vec<u8>) {
        self.files
            .insert(format!("{GAME_ROUTE}{path}"), (mime, bytes));
    }
    pub fn get(&self, path: &str) -> Option<(&'static str, &[u8])> {
        let name = if path == "/" {
            "index.html"
        } else {
            path.strip_prefix('/')?
        };
        self.files
            .get(name)
            .map(|(mime, b)| (*mime, b.as_slice()))
            .or_else(|| embedded_asset(path))
    }

    pub fn with_replay_archive(mut self, root: Option<&Path>) -> Result<Self, HostError> {
        self.replay_archive = root
            .map(NativeFiles::open)
            .transpose()
            .map_err(|_| HostError::Configuration("cooked replay archive root"))?;
        Ok(self)
    }

    pub fn archived_level(&self, path: &str) -> Option<Vec<u8>> {
        let (digest, name) = path.strip_prefix(REPLAY_ROUTE)?.split_once('/')?;
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
        {
            return None;
        }
        if !matches!(
            name,
            "manifest.json" | "levels/room-one.json" | "levels/traversal-yard.json"
        ) {
            return None;
        }
        if let Some((_, manifest)) = self.files.get(&format!("{GAME_ROUTE}manifest.json")) {
            if matches_digest(manifest, digest) {
                if let Some((_, data)) = self.files.get(&format!("{GAME_ROUTE}{name}")) {
                    return Some(data.clone());
                }
            }
        }
        let root = self.replay_archive.as_ref()?;
        let data = read_file(root, &format!("{digest}/manifest.json"), 16_384).ok()??;
        if !matches_digest(&data, digest) {
            return None;
        }
        let manifest: CookedManifest = serde_json::from_slice(&data).ok()?;
        if manifest.version != 1 {
            return None;
        }
        let entry = match name {
            "manifest.json" => return Some(data),
            "levels/room-one.json" => manifest.entries.room_one?,
            "levels/traversal-yard.json" => manifest.entries.traversal_yard?,
            _ => return None,
        };
        entry
            .validate(name, CookedKind::Level, &[], &["scout-atlas"])
            .ok()?;
        let level = read_file(root, &format!("{digest}/{name}"), MAX_LEVEL_BYTES).ok()??;
        matches_digest(&level, &entry.sha256).then_some(level)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicUsize, Ordering},
    };

    struct Bundle(PathBuf);

    impl Bundle {
        fn new() -> Self {
            static SERIAL: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "forge-cooked-test-{}-{}",
                std::process::id(),
                SERIAL.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            let files: Vec<_> = NAMES
                .iter()
                .map(|(name, _)| {
                    fs::write(path.join(name), name).unwrap();
                    json!({"path": name, "sha256": format!("{:x}", Sha256::digest(name))})
                })
                .collect();
            fs::write(
                path.join("manifest.json"),
                serde_json::to_vec(
                    &json!({"format_version": 1, "abi_version": 19, "files": files}),
                )
                .unwrap(),
            )
            .unwrap();
            Self(path)
        }

        fn write(&self, path: &str, bytes: &[u8]) {
            let path = self.0.join("game-assets").join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }

        fn entry(
            &self,
            path: &str,
            kind: &str,
            bytes: &[u8],
            dependencies: &[&str],
            optional_dependencies: &[&str],
        ) -> Value {
            self.write(path, bytes);
            json!({
                "path": path,
                "kind": kind,
                "sha256": format!("{:x}", Sha256::digest(bytes)),
                "dependencies": dependencies,
                "optional_dependencies": optional_dependencies,
            })
        }

        fn populate(&self) -> Value {
            let manifest = json!({
                "version": 1,
                "entries": {
                    "room-one": self.entry(
                        "levels/room-one.json", "level", b"room one", &[], &["scout-atlas"],
                    ),
                    "traversal-yard": self.entry(
                        "levels/traversal-yard.json", "level", b"yard", &[], &["scout-atlas"],
                    ),
                    "scout-atlas": self.entry(
                        "atlas/manifest.json", "atlas", b"atlas", &["scout-atlas-image"], &[],
                    ),
                    "scout-atlas-image": self.entry(
                        "atlas/sheet.png", "image", b"image bytes", &[], &[],
                    ),
                }
            });
            self.write("manifest.json", &serde_json::to_vec(&manifest).unwrap());
            manifest
        }

        fn load(&self) -> Result<Assets, HostError> {
            Assets::load(&self.0, None)
        }
    }

    impl Drop for Bundle {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn current_ui_abi_assets_are_admitted_with_their_hashes_verified() {
        let bundle = Bundle::new();
        assert!(Assets::load(&bundle.0, None).is_ok());
        fs::write(bundle.0.join("app.wasm"), b"modified bytes").unwrap();
        assert!(matches!(
            Assets::load(&bundle.0, None),
            Err(HostError::Configuration("asset hash/duplicate"))
        ));
    }

    #[test]
    fn cooked_routes_serve_only_verified_startup_bytes() {
        let bundle = Bundle::new();
        assert!(bundle
            .load()
            .unwrap()
            .get("/playtest/assets/manifest.json")
            .is_none());
        bundle.populate();
        let assets = bundle.load().unwrap();
        bundle.write("levels/room-one.json", b"modified after startup");
        assert_eq!(
            assets.get("/playtest/assets/levels/room-one.json"),
            Some((JSON_MIME, b"room one".as_slice()))
        );
        assert_eq!(
            assets.get("/playtest/assets/atlas/sheet.png"),
            Some(("image/png", b"image bytes".as_slice()))
        );
        for route in [
            "/playtest/assets/levels/../atlas/sheet.png",
            "/playtest/assets/cook-state.json",
            "/game-assets/manifest.json",
        ] {
            assert!(assets.get(route).is_none(), "{route}");
        }
        let after = bundle.load().unwrap();
        assert!(after.get("/playtest/assets/levels/room-one.json").is_none());
        assert!(after
            .get("/playtest/assets/levels/traversal-yard.json")
            .is_some());
        assert!(after.get("/playtest/assets/manifest.json").is_some());
        // The required Forge application assets still reject damage.
        fs::write(bundle.0.join("index.html"), b"bad app bytes").unwrap();
        assert!(bundle.load().is_err());
    }

    #[test]
    fn replay_archive_requires_matching_manifest_and_level_digests() {
        let bundle = Bundle::new();
        let mut current = bundle.populate();
        let archive = bundle.0.join("archive");
        let previous = serde_json::to_vec(&current).unwrap();
        let digest = format!("{:x}", Sha256::digest(&previous));
        let old = archive.join(&digest);
        fs::create_dir_all(old.join("levels")).unwrap();
        fs::write(old.join("manifest.json"), &previous).unwrap();
        fs::write(old.join("levels/room-one.json"), b"room one").unwrap();
        current["entries"]["room-one"] = bundle.entry(
            "levels/room-one.json",
            "level",
            b"updated room",
            &[],
            &["scout-atlas"],
        );
        bundle.write("manifest.json", &serde_json::to_vec(&current).unwrap());
        let assets = bundle
            .load()
            .unwrap()
            .with_replay_archive(Some(&archive))
            .unwrap();
        let route = |name| format!("{REPLAY_ROUTE}{digest}/{name}");
        assert_eq!(
            assets.archived_level(&route("manifest.json")),
            Some(previous)
        );
        let level = route("levels/room-one.json");
        assert_eq!(
            assets.archived_level(&level).as_deref(),
            Some(b"room one".as_slice())
        );
        assert!(assets
            .archived_level(&route("levels/../manifest.json"))
            .is_none());
        fs::write(old.join("levels/room-one.json"), b"corrupt").unwrap();
        assert!(assets.archived_level(&level).is_none());
        fs::write(old.join("manifest.json"), b"{}").unwrap();
        assert!(assets.archived_level(&route("manifest.json")).is_none());
    }

    #[test]
    fn invalid_optional_art_is_hidden_and_bad_manifests_omit_only_game_routes() {
        let bundle = Bundle::new();
        let mut manifest = bundle.populate();
        bundle.write("atlas/sheet.png", b"different bytes");
        let fallback = bundle.load().unwrap();
        assert!(fallback.get("/playtest/assets/manifest.json").is_some());
        assert!(fallback
            .get("/playtest/assets/levels/traversal-yard.json")
            .is_some());
        assert!(fallback
            .get("/playtest/assets/atlas/manifest.json")
            .is_none());
        assert!(fallback.get("/playtest/assets/atlas/sheet.png").is_none());

        manifest["entries"]["room-one"]["path"] = json!("../index.html");
        bundle.write("manifest.json", &serde_json::to_vec(&manifest).unwrap());
        let fallback = bundle.load().unwrap();
        assert!(fallback.get("/").is_some());
        assert!(fallback.get("/playtest/assets/manifest.json").is_none());
        assert!(fallback
            .get("/playtest/assets/levels/traversal-yard.json")
            .is_none());
    }

    #[test]
    fn alternate_cooked_assets_fill_only_verified_missing_files() {
        let primary = Bundle::new();
        let alternate = Bundle::new();
        let manifest = primary.populate();
        let alternate_root = alternate.0.join("game-assets");
        let load = || Assets::load(&primary.0, Some(&alternate_root)).unwrap();
        alternate.write("manifest.json", b"ignore the alternate manifest");
        alternate.write("levels/room-one.json", b"room one");
        alternate.write("levels/traversal-yard.json", b"different yard");
        alternate.write("atlas/sheet.png", b"image bytes");
        fs::remove_file(primary.0.join("game-assets/levels/room-one.json")).unwrap();
        fs::remove_file(primary.0.join("game-assets/atlas/sheet.png")).unwrap();

        let assets = load();
        assert_eq!(
            assets.get("/playtest/assets/manifest.json"),
            Some((JSON_MIME, serde_json::to_vec(&manifest).unwrap().as_slice()))
        );
        assert_eq!(
            assets.get("/playtest/assets/levels/room-one.json"),
            Some((JSON_MIME, b"room one".as_slice()))
        );
        assert_eq!(
            assets.get("/playtest/assets/levels/traversal-yard.json"),
            Some((JSON_MIME, b"yard".as_slice()))
        );
        assert_eq!(
            assets.get("/playtest/assets/atlas/sheet.png"),
            Some(("image/png", b"image bytes".as_slice()))
        );

        primary.write("levels/room-one.json", b"damaged primary");
        alternate.write("atlas/sheet.png", b"wrong alternate digest");
        let assets = load();
        assert!(assets
            .get("/playtest/assets/levels/room-one.json")
            .is_none());
        assert!(assets.get("/playtest/assets/atlas/manifest.json").is_none());
        assert!(assets.get("/playtest/assets/atlas/sheet.png").is_none());

        let alternate_manifest = alternate.populate();
        fs::remove_file(primary.0.join("game-assets/manifest.json")).unwrap();
        assert_eq!(alternate_manifest, manifest);
        let assets = load();
        assert!(assets.get("/").is_some());
        assert!(assets.get("/playtest/assets/manifest.json").is_none());
        assert!(assets
            .get("/playtest/assets/levels/room-one.json")
            .is_none());
    }

    #[test]
    fn unavailable_alternate_preserves_primary_routes() {
        let primary = Bundle::new();
        primary.populate();
        let alternate_root = primary.0.join("no-alternate");
        let assets = Assets::load(&primary.0, Some(&alternate_root)).unwrap();
        assert!(assets.get("/").is_some());
        assert!(assets
            .get("/playtest/assets/levels/room-one.json")
            .is_some());
    }
}
