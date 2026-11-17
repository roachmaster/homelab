use crate::HostError;
use forge_ide::filesystem::{FsError, FsRequest, FsResponse, NativeFiles};
use forge_io_client::IoClient;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, io, path::Path};
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
        "/design/game-asset-pipeline" | "/design/game-asset-pipeline/" => {
            Some(("text/html; charset=utf-8", GAME_ASSET_PIPELINE_HTML))
        }
        "/design/game-asset-pipeline/cook.svg" => Some(("image/svg+xml", GAME_ASSET_PIPELINE_COOK)),
        "/design/game-asset-pipeline/load.svg" => Some(("image/svg+xml", GAME_ASSET_PIPELINE_LOAD)),
        "/design/game-asset-pipeline/contract.svg" => {
            Some(("image/svg+xml", GAME_ASSET_PIPELINE_CONTRACT))
        }
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

    fn load(&self, files: &NativeFiles, limit: usize) -> Result<Vec<u8>, HostError> {
        let bytes = read_file(files, &format!("game-assets/{}", self.path), limit)?
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
const MAX_LEVEL_BYTES: usize = 16 * 1024;
const MAX_ATLAS_BYTES: usize = 1024 * 1024;
const MAX_PNG_BYTES: usize = 8 * 1024 * 1024;

pub(crate) struct Assets(BTreeMap<String, (&'static str, Vec<u8>)>);
impl Assets {
    pub fn load(root: &Path) -> Result<Self, HostError> {
        let files = NativeFiles::open(root).map_err(|_| HostError::Configuration("asset root"))?;
        let read = |name: &str, limit| {
            read_file(&files, name, limit)?.ok_or(HostError::Configuration("asset regular file"))
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
            if !matches_digest(&bytes, &f.sha256) || loaded.insert(f.path, (*mime, bytes)).is_some()
            {
                return Err(HostError::Configuration("asset hash/duplicate"));
            }
        }
        // A game bundle is independent of the required Forge UI assets.
        // A bad sidecar cannot prevent the host's other capabilities starting.
        if let Ok(game) = Self::load_game(&files) {
            loaded.extend(game.0);
        }
        Ok(Self(loaded))
    }

    fn load_game(files: &NativeFiles) -> Result<Self, HostError> {
        let mut game = Self(BTreeMap::new());
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
        for (level, path) in [
            (room_one, "levels/room-one.json"),
            (traversal_yard, "levels/traversal-yard.json"),
        ] {
            if let Some(level) = level {
                level.validate(path, CookedKind::Level, &[], &["scout-atlas"])?;
                if let Ok(bytes) = level.load(files, MAX_LEVEL_BYTES) {
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
            let atlas_bytes = atlas.load(files, MAX_ATLAS_BYTES);
            let image_bytes = image.load(files, MAX_PNG_BYTES);
            if let (Ok(atlas_bytes), Ok(image_bytes)) = (atlas_bytes, image_bytes) {
                game.add_game(&atlas.path, JSON_MIME, atlas_bytes);
                game.add_game(&image.path, "image/png", image_bytes);
            }
        }
        game.add_game("manifest.json", JSON_MIME, bytes);
        Ok(game)
    }

    fn add_game(&mut self, path: &str, mime: &'static str, bytes: Vec<u8>) {
        self.0.insert(format!("{GAME_ROUTE}{path}"), (mime, bytes));
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
                serde_json::to_vec(&json!({"format_version": 1, "abi_version": 4, "files": files}))
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
            Assets::load(&self.0)
        }
    }

    impl Drop for Bundle {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
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
}
