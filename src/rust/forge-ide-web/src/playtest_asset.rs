//! Rust admission for cooked bytes delivered by the browser's I/O worker.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Ord, PartialOrd)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Asset {
    Manifest,
    RoomOne,
    TraversalYard,
    ScoutAtlas,
    ScoutAtlasImage,
}

impl Asset {
    pub(super) fn from_wire(value: u32) -> Option<Self> {
        Some(match value {
            0 => Self::Manifest,
            1 => Self::RoomOne,
            2 => Self::TraversalYard,
            3 => Self::ScoutAtlas,
            4 => Self::ScoutAtlasImage,
            _ => return None,
        })
    }

    pub(super) fn slot(self) -> Option<u32> {
        match self {
            Self::RoomOne => Some(1),
            Self::TraversalYard => Some(2),
            _ => None,
        }
    }

    fn limit(self) -> usize {
        match self {
            Self::Manifest | Self::RoomOne | Self::TraversalYard => 16 * 1024,
            Self::ScoutAtlas => 256 * 1024,
            Self::ScoutAtlasImage => 8 * 1024 * 1024,
        }
    }

    fn binding(self) -> Option<(&'static str, &'static str, &'static [Self], &'static [Self])> {
        Some(match self {
            Self::Manifest => return None,
            Self::RoomOne => ("levels/room-one.json", "level", &[], &[Self::ScoutAtlas]),
            Self::TraversalYard => (
                "levels/traversal-yard.json",
                "level",
                &[],
                &[Self::ScoutAtlas],
            ),
            Self::ScoutAtlas => (
                "atlas/manifest.json",
                "atlas",
                &[Self::ScoutAtlasImage],
                &[],
            ),
            Self::ScoutAtlasImage => ("atlas/sheet.png", "image", &[], &[]),
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u16,
    entries: BTreeMap<Asset, Entry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    kind: String,
    path: String,
    sha256: String,
    dependencies: Vec<Asset>,
    optional_dependencies: Vec<Asset>,
}

pub(super) struct VerifiedManifest {
    digest: [u8; 32],
    entries: BTreeMap<Asset, [u8; 32]>,
}

impl VerifiedManifest {
    pub(super) fn decode(data: &[u8]) -> Option<Self> {
        let manifest: Manifest = serde_json::from_slice(data).ok()?;
        if manifest.version != 1 || manifest.entries.is_empty() {
            return None;
        }
        let mut entries = BTreeMap::new();
        for (asset, entry) in manifest.entries {
            let (path, kind, required, optional) = asset.binding()?;
            if entry.path != path
                || entry.kind != kind
                || entry.dependencies != required
                || entry.optional_dependencies != optional
            {
                return None;
            }
            entries.insert(asset, hex_digest(&entry.sha256)?);
        }
        Some(Self {
            digest: Sha256::digest(data).into(),
            entries,
        })
    }

    pub(super) fn contains(&self, asset: Asset, data: &[u8]) -> bool {
        self.entries
            .get(&asset)
            .is_some_and(|expected| *expected == <[u8; 32]>::from(Sha256::digest(data)))
    }

    pub(super) fn words(&self) -> [u32; 8] {
        std::array::from_fn(|index| {
            let start = index * 4;
            u32::from_le_bytes(
                self.digest[start..start + 4]
                    .try_into()
                    .expect("digest word"),
            )
        })
    }
}

fn hex_digest(value: &str) -> Option<[u8; 32]> {
    let mut digest = [0; 32];
    if value.len() != digest.len() * 2 {
        return None;
    }
    fn digit(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            _ => None,
        }
    }
    for (part, pair) in digest.iter_mut().zip(value.as_bytes().chunks_exact(2)) {
        *part = (digit(pair[0])? << 4) | digit(pair[1])?;
    }
    Some(digest)
}

#[derive(Deserialize)]
struct Atlas {
    version: u16,
    image: String,
    atlas_scale: u32,
    logical_frame_size: [u32; 2],
    frame_size: [u32; 2],
    pivot: [u32; 2],
    clips: BTreeMap<String, Vec<Frame>>,
}

#[derive(Deserialize)]
struct Frame {
    rect: [u32; 4],
}

#[derive(Serialize)]
struct AtlasView {
    dimensions: [u32; 2],
    size: [u32; 2],
    pivot: [u32; 2],
    clips: BTreeMap<&'static str, Vec<[u32; 4]>>,
}

impl Atlas {
    fn view(self, image: &[u8]) -> Option<AtlasView> {
        let dimensions = png_dimensions(image)?;
        if self.version != 2
            || self.image != "sheet.png"
            || !(1..=8).contains(&self.atlas_scale)
            || (0..2).any(|axis| {
                !(1..=128).contains(&self.logical_frame_size[axis])
                    || self.frame_size[axis] != self.logical_frame_size[axis] * self.atlas_scale
                    || self.pivot[axis] >= self.logical_frame_size[axis]
            })
        {
            return None;
        }
        let mut clips = BTreeMap::new();
        for name in ["idle", "walk", "jump", "crouch"] {
            let frames = self.clips.get(name)?;
            if !(1..=512).contains(&frames.len())
                || frames.iter().any(|frame| {
                    let [x, y, width, height] = frame.rect;
                    [width, height] != self.frame_size
                        || x.checked_add(width).is_none_or(|end| end > dimensions[0])
                        || y.checked_add(height).is_none_or(|end| end > dimensions[1])
                })
            {
                return None;
            }
            clips.insert(name, frames.iter().map(|frame| frame.rect).collect());
        }
        Some(AtlasView {
            dimensions,
            size: self.logical_frame_size,
            pivot: self.pivot,
            clips,
        })
    }
}

fn png_dimensions(data: &[u8]) -> Option<[u32; 2]> {
    if data.get(..16)? != b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR" {
        return None;
    }
    let width = u32::from_be_bytes(data.get(16..20)?.try_into().ok()?);
    let height = u32::from_be_bytes(data.get(20..24)?.try_into().ok()?);
    ((1..=4096).contains(&width) && (1..=4096).contains(&height)).then_some([width, height])
}

#[derive(Default)]
pub(super) struct Assets {
    upload: Option<(Asset, Vec<u8>)>,
    manifest: Option<VerifiedManifest>,
    atlas: Option<Atlas>,
    atlas_view: Vec<u8>,
}

impl Assets {
    pub(super) fn reserve(&mut self, asset: Asset, length: usize) -> u32 {
        self.upload = None;
        if !(1..=asset.limit()).contains(&length) {
            return 0;
        }
        let mut data = vec![0; length];
        let offset = data.as_mut_ptr() as u32;
        self.upload = Some((asset, data));
        offset
    }

    pub(super) fn take(&mut self, asset: Asset) -> Option<Vec<u8>> {
        let (uploaded, bytes) = self.upload.take()?;
        (uploaded == asset).then_some(bytes)
    }

    pub(super) fn install_manifest(&mut self, bytes: &[u8]) -> bool {
        let Some(candidate) = VerifiedManifest::decode(bytes) else {
            return false;
        };
        if let Some(active) = &self.manifest {
            return active.digest == candidate.digest;
        }
        self.manifest = Some(candidate);
        true
    }

    pub(super) fn verify(&self, asset: Asset, bytes: &[u8]) -> Option<[u32; 8]> {
        let manifest = self.manifest.as_ref()?;
        manifest.contains(asset, bytes).then(|| manifest.words())
    }

    pub(super) fn prepare_atlas(&mut self, bytes: &[u8]) -> bool {
        self.atlas_view.clear();
        self.atlas = serde_json::from_slice(bytes).ok();
        self.atlas.is_some()
    }

    pub(super) fn finish_atlas(&mut self, bytes: &[u8]) -> bool {
        let Some(view) = self.atlas.take().and_then(|atlas| atlas.view(bytes)) else {
            return false;
        };
        let Ok(encoded) = serde_json::to_vec(&view) else {
            return false;
        };
        self.atlas_view = encoded;
        true
    }

    pub(super) fn atlas_view(&self) -> &[u8] {
        &self.atlas_view
    }
}
