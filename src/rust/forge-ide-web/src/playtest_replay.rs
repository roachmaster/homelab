//! Bounded per-session journal of the inputs consumed by fixed ticks.

use super::{
    asset::{Asset, VerifiedManifest},
    level::{decode_replay_level, Loader},
};
use forge_game_runtime::{
    physics::{Horizontal, MotionInput},
    Action, EffectiveInput, Level, Room,
};
use serde::{Deserialize, Deserializer, Serialize};
use std::fmt::Write;

const MAX_TICKS: u32 = 36_000;
const MAX_SEGMENTS: usize = 128;
pub(super) const MAX_BYTES: usize = 2 * 1024 * 1024;
const VERSION: u32 = 1;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Session {
    version: u32,
    complete: bool,
    segments: Vec<Segment>,
    #[serde(skip)]
    ticks: u32,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Segment {
    epoch: u32,
    level_slot: u32,
    #[serde(deserialize_with = "explicit_option")]
    manifest_sha256: Option<String>,
    final_tick: u32,
    inputs: Vec<InputRun>,
    #[serde(deserialize_with = "explicit_option")]
    transition: Option<Transition>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct InputRun {
    input: Input,
    ticks: u32,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Transition {
    kind: TransitionKind,
    boundary_tick: u32,
    incoming_epoch: u32,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
enum TransitionKind {
    Level,
    Restart,
}

#[derive(Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum Input {
    Completed,
    TileIdle,
    TileUp,
    TileDown,
    TileLeft,
    TileRight,
    PlatformLeft,
    PlatformIdle,
    PlatformRight,
    JumpLeft,
    Jump,
    JumpRight,
}

pub(super) struct Import {
    session: Session,
    needs: Vec<Need>,
}

struct Need {
    slot: u32,
    digest: [u32; 8],
    manifest: Option<VerifiedManifest>,
    level: Option<Level>,
}

pub(super) struct Timeline {
    pub room: Room,
    segment: usize,
    run: usize,
    remaining: u32,
}

impl Session {
    pub(super) fn new(epoch: u32) -> Self {
        Self {
            version: VERSION,
            complete: true,
            segments: vec![Segment::new(epoch, 0, None)],
            ticks: 0,
        }
    }

    pub(super) fn record(&mut self, tick: u32, input: EffectiveInput) {
        if !self.complete {
            return;
        }
        let segment = self.segments.last_mut().expect("active replay segment");
        if self.ticks == MAX_TICKS || segment.final_tick.checked_add(1) != Some(tick) {
            self.complete = false;
            return;
        }
        let input = Input::from(input);
        if let Some(run) = segment.inputs.last_mut().filter(|run| run.input == input) {
            run.ticks += 1;
        } else {
            segment.inputs.push(InputRun { input, ticks: 1 });
        }
        segment.final_tick = tick;
        self.ticks += 1;
    }

    pub(super) fn activate(&mut self, tick: u32, epoch: u32, slot: u32, digest: Option<[u32; 8]>) {
        let Some(digest) = digest else {
            self.complete = false;
            return;
        };
        if self.complete {
            let mut hex = String::with_capacity(64);
            for byte in digest.into_iter().flat_map(u32::to_le_bytes) {
                write!(hex, "{byte:02x}").expect("hex writes to String");
            }
            self.transition(
                TransitionKind::Level,
                tick,
                Segment::new(epoch, slot, Some(hex)),
            );
        }
    }

    pub(super) fn restart(&mut self, tick: u32, epoch: u32) {
        if self.complete {
            let old = self.segments.last().expect("active replay segment");
            let next = Segment::new(epoch, old.level_slot, old.manifest_sha256.clone());
            self.transition(TransitionKind::Restart, tick, next);
        }
    }

    fn transition(&mut self, kind: TransitionKind, boundary_tick: u32, next: Segment) {
        if self.segments.len() == MAX_SEGMENTS {
            self.complete = false;
            return;
        }
        let old = self.segments.last_mut().expect("active replay segment");
        old.transition = Some(Transition {
            kind,
            boundary_tick,
            incoming_epoch: next.epoch,
        });
        self.segments.push(next);
    }
}

impl Import {
    pub(super) fn parse(bytes: &[u8], loader: &Loader) -> Option<Self> {
        if bytes.len() > MAX_BYTES {
            return None;
        }
        let session: Session = serde_json::from_slice(bytes).ok()?;
        if session.version != VERSION || !session.complete || session.segments.len() > MAX_SEGMENTS
        {
            return None;
        }
        let first = session.segments.first()?;
        if (first.epoch, first.level_slot, &first.manifest_sha256) != (1, 0, &None) {
            return None;
        }

        let mut total_ticks = 0u32;
        let mut needs: Vec<Need> = Vec::new();
        for (index, segment) in session.segments.iter().enumerate() {
            let ticks = segment.inputs.iter().try_fold(0u32, |sum, run| {
                sum.checked_add(run.ticks).filter(|_| run.ticks != 0)
            })?;
            total_ticks = total_ticks.checked_add(ticks)?;
            if ticks != segment.final_tick || total_ticks > MAX_TICKS {
                return None;
            }
            match (&segment.transition, session.segments.get(index + 1)) {
                (Some(transition), Some(next)) => {
                    if transition.incoming_epoch != next.epoch
                        || segment.epoch.checked_add(1) != Some(next.epoch)
                    {
                        return None;
                    }
                    match transition.kind {
                        TransitionKind::Level => {
                            if next.level_slot == segment.level_slot
                                || segment.final_tick.checked_add(1)
                                    != Some(transition.boundary_tick)
                            {
                                return None;
                            }
                            let slot = next.level_slot;
                            let digest = parse_digest(next.manifest_sha256.as_deref()?)?;
                            if !matches!(slot, 1 | 2) {
                                return None;
                            }
                            if !needs
                                .iter()
                                .any(|need| need.slot == slot && need.digest == digest)
                            {
                                needs.push(Need {
                                    slot,
                                    digest,
                                    manifest: None,
                                    level: loader.replay_level(slot, digest),
                                });
                            }
                        }
                        TransitionKind::Restart => {
                            if transition.boundary_tick != segment.final_tick
                                || next.level_slot != segment.level_slot
                                || next.manifest_sha256 != segment.manifest_sha256
                            {
                                return None;
                            }
                        }
                    }
                }
                (None, None) => (),
                _ => return None,
            }
        }
        Some(Self { session, needs })
    }

    pub(super) fn need_count(&self) -> u32 {
        self.needs.len() as u32
    }

    pub(super) fn missing_slot(&self, index: usize) -> u32 {
        self.needs
            .get(index)
            .filter(|need| need.level.is_none())
            .map_or(0, |need| need.slot)
    }

    pub(super) fn digest_word(&self, index: usize, word: usize) -> u32 {
        self.needs
            .get(index)
            .and_then(|need| need.digest.get(word))
            .copied()
            .unwrap_or(0)
    }

    pub(super) fn accept_asset(&mut self, index: usize, asset: Asset, bytes: &[u8]) -> bool {
        let Some(need) = self
            .needs
            .get_mut(index)
            .filter(|need| need.level.is_none())
        else {
            return false;
        };
        if asset == Asset::Manifest {
            need.manifest =
                VerifiedManifest::decode(bytes).filter(|item| item.words() == need.digest);
            return need.manifest.is_some();
        }
        if asset.slot() != Some(need.slot)
            || !need
                .manifest
                .as_ref()
                .is_some_and(|manifest| manifest.contains(asset, bytes))
        {
            return false;
        }
        need.level = decode_replay_level(bytes, need.slot);
        if need.level.is_some() {
            need.manifest = None;
        }
        need.level.is_some()
    }

    pub(super) fn validate(&self) -> Option<Timeline> {
        if self.needs.iter().any(|need| need.level.is_none()) {
            return None;
        }
        let mut timeline = Timeline::new();
        while timeline.step(self)? {}
        Some(timeline)
    }
}

impl Timeline {
    pub(super) fn new() -> Self {
        Self {
            room: Room::new(),
            segment: 0,
            run: 0,
            remaining: 0,
        }
    }

    pub(super) fn done(&self, import: &Import) -> bool {
        self.segment + 1 == import.session.segments.len()
            && self.room.tick() == import.session.segments[self.segment].final_tick
    }

    pub(super) fn step(&mut self, import: &Import) -> Option<bool> {
        let segment = import.session.segments.get(self.segment)?;
        if self.room.epoch() != segment.epoch {
            return None;
        }
        if let Some(run) = segment.inputs.get(self.run) {
            if self.remaining == 0 {
                self.remaining = run.ticks;
            }
            if !self.room.replay_tick(run.input.into()) {
                return None;
            }
            self.remaining -= 1;
            if self.remaining == 0 {
                self.run += 1;
            }
            return Some(true);
        }
        let Some(transition) = &segment.transition else {
            return Some(false);
        };
        self.segment += 1;
        self.run = 0;
        match transition.kind {
            TransitionKind::Level => {
                let target = &import.session.segments[self.segment];
                let digest = parse_digest(target.manifest_sha256.as_deref()?)?;
                let level = import
                    .needs
                    .iter()
                    .find(|need| need.slot == target.level_slot && need.digest == digest)?
                    .level
                    .as_ref()?
                    .clone();
                self.room = Room::from_validated(level, target.epoch, 0);
            }
            TransitionKind::Restart => self.room.restart(),
        }
        Some(true)
    }
}

impl Segment {
    fn new(epoch: u32, level_slot: u32, manifest_sha256: Option<String>) -> Self {
        Self {
            epoch,
            level_slot,
            manifest_sha256,
            final_tick: 0,
            inputs: Vec::new(),
            transition: None,
        }
    }
}

impl From<EffectiveInput> for Input {
    fn from(input: EffectiveInput) -> Self {
        match input {
            EffectiveInput::Completed => Self::Completed,
            EffectiveInput::Tile(None) => Self::TileIdle,
            EffectiveInput::Tile(Some(Action::Up)) => Self::TileUp,
            EffectiveInput::Tile(Some(Action::Down)) => Self::TileDown,
            EffectiveInput::Tile(Some(Action::Left)) => Self::TileLeft,
            EffectiveInput::Tile(Some(Action::Right)) => Self::TileRight,
            EffectiveInput::Tile(Some(Action::Jump)) => unreachable!("tile jump is filtered"),
            EffectiveInput::Platform(input) => match (input.horizontal, input.jump) {
                (Horizontal::Left, false) => Self::PlatformLeft,
                (Horizontal::Neutral, false) => Self::PlatformIdle,
                (Horizontal::Right, false) => Self::PlatformRight,
                (Horizontal::Left, true) => Self::JumpLeft,
                (Horizontal::Neutral, true) => Self::Jump,
                (Horizontal::Right, true) => Self::JumpRight,
            },
        }
    }
}

impl From<Input> for EffectiveInput {
    fn from(input: Input) -> Self {
        let (horizontal, jump) = match input {
            Input::Completed => return Self::Completed,
            Input::TileIdle => return Self::Tile(None),
            Input::TileUp => return Self::Tile(Some(Action::Up)),
            Input::TileDown => return Self::Tile(Some(Action::Down)),
            Input::TileLeft => return Self::Tile(Some(Action::Left)),
            Input::TileRight => return Self::Tile(Some(Action::Right)),
            Input::PlatformLeft => (Horizontal::Left, false),
            Input::PlatformIdle => (Horizontal::Neutral, false),
            Input::PlatformRight => (Horizontal::Right, false),
            Input::JumpLeft => (Horizontal::Left, true),
            Input::Jump => (Horizontal::Neutral, true),
            Input::JumpRight => (Horizontal::Right, true),
        };
        Self::Platform(MotionInput { horizontal, jump })
    }
}

fn parse_digest(hex: &str) -> Option<[u32; 8]> {
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        return None;
    }
    let mut words = [0; 8];
    for (word, offset) in words.iter_mut().zip((0..64).step_by(8)) {
        *word = u32::from_str_radix(&hex[offset..offset + 8], 16)
            .ok()?
            .swap_bytes();
    }
    Some(words)
}

fn explicit_option<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Option<T>, D::Error> {
    Option::deserialize(d)
}
