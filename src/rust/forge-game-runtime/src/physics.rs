//! Deterministic, fixed-step tile collision for the side-view character.

use std::cmp::Ordering;

/// One tile in signed Q16 units. X increases rightward and Y downward.
pub const TILE: i32 = 65_536;
const MAX_GRID: u32 = 64;
const MAX_BODY_SIZE: i32 = 4 * TILE;
const MAX_DISPLACEMENT: i32 = MAX_GRID as i32 * TILE;
const GRAVITY: i32 = 583;
const JUMP_IMPULSE: i32 = -8_738;
const RUN_SPEED: i32 = 4_369;
const FALL_SPEED: i32 = 13_107;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Vector {
    pub x: i32,
    pub y: i32,
}

/// The solver treats the exterior perimeter as solid and never calls a grid
/// with coordinates outside the declared dimensions.
pub trait TileGrid {
    fn width(&self) -> u32;
    fn height(&self) -> u32;
    fn is_solid(&self, x: u32, y: u32) -> bool;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Collider {
    bounds: Bounds,
}

impl Collider {
    /// Forms a centered foot-anchored collider.
    pub fn new(half_width: i32, height: i32) -> Option<Self> {
        Self::from_edges(
            half_width.checked_neg()?,
            height.checked_neg()?,
            half_width,
            0,
        )
    }

    /// Forms an AABB using cooked offsets relative to the authored pivot.
    pub fn from_edges(left: i32, top: i32, right: i32, bottom: i32) -> Option<Self> {
        if ![left, top, right, bottom]
            .into_iter()
            .all(|edge| (-MAX_BODY_SIZE..=MAX_BODY_SIZE).contains(&edge))
            || !(1..=MAX_BODY_SIZE).contains(&(right - left))
            || !(1..=MAX_BODY_SIZE).contains(&(bottom - top))
        {
            return None;
        }
        Some(Self {
            bounds: Bounds {
                left,
                top,
                right,
                bottom,
            },
        })
    }

    pub fn edges(self) -> [i32; 4] {
        [
            self.bounds.left,
            self.bounds.top,
            self.bounds.right,
            self.bounds.bottom,
        ]
    }

    /// Tests positive-area overlap. Touching faces do not trigger an overlap.
    pub fn overlaps(self, root: Vector, other: Self, other_root: Vector) -> bool {
        let Some(bounds) = self.bounds.at(root) else {
            return false;
        };
        other
            .bounds
            .at(other_root)
            .is_some_and(|target| bounds.overlaps(target))
    }
}

impl Default for Collider {
    fn default() -> Self {
        Self::from_edges(-TILE / 4, -TILE * 3 / 4, TILE / 4, 0).expect("default collider fits")
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Horizontal {
    Left,
    #[default]
    Neutral,
    Right,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MotionInput {
    pub horizontal: Horizontal,
    /// A pressed edge. The input adapter owns de-duplication.
    pub jump: bool,
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MotionPhase {
    #[default]
    Idle = 0,
    Walk = 1,
    Rise = 2,
    Fall = 3,
    Land = 4,
}

/// Outward normal on the surface encountered by the moving body.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Normal {
    Left = 1,
    Right = 2,
    Up = 3,
    Down = 4,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Contact {
    pub tile_x: i32,
    pub tile_y: i32,
    pub normal: Normal,
    /// Fraction of the candidate movement in Q16. The endpoint is 65,536.
    pub time_of_impact: u32,
    /// Speed into the plane along its normal, in Q16 per tick.
    pub impact_speed: i32,
    /// Q16 depth if the query began embedded.
    pub penetration: i32,
    /// Tick of the contact's first occurrence on this character.
    /// Read-only queries have no tick and return zero.
    pub first_seen_tick: u32,
}

impl Contact {
    fn same_surface(self, prior: Option<Self>) -> bool {
        prior.is_some_and(|prior| {
            (self.tile_x, self.tile_y, self.normal) == (prior.tile_x, prior.tile_y, prior.normal)
        })
    }

    fn observe(mut self, prior: Option<Self>, tick: u32) -> Self {
        self.first_seen_tick = prior
            .filter(|_| self.same_surface(prior))
            .map_or(tick, |contact| contact.first_seen_tick);
        self
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Contacts {
    pub ground: Option<Contact>,
    pub head: Option<Contact>,
    pub wall: Option<Contact>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MotionFrame {
    pub previous: Vector,
    pub position: Vector,
    pub velocity: Vector,
    pub grounded: bool,
    pub phase: MotionPhase,
    pub contacts: Contacts,
    pub landed: Option<Contact>,
    pub head_bump: Option<Contact>,
    pub wall_contact: Option<Contact>,
}

/// The character owns the resolved root. Consumers select and attach local
/// animation poses from the motion frame.
pub struct Character {
    collider: Collider,
    frame: MotionFrame,
    tick: u32,
}

impl Character {
    pub fn spawn(grid: &impl TileGrid, foot: Vector, collider: Collider) -> Option<Self> {
        let size = Dimensions::read(grid)?;
        let bounds = collider.bounds.at(foot)?;
        if !size.contains(bounds) || overlaps_terrain(grid, size, bounds) {
            return None;
        }
        let ground = sweep_tiles(grid, foot, collider, Vector { x: 0, y: 1 })
            .filter(|hit| hit.contact.time_of_impact == 0)
            .map(|hit| hit.contact);
        Some(Self {
            collider,
            frame: MotionFrame {
                previous: foot,
                position: foot,
                grounded: ground.is_some(),
                phase: if ground.is_some() {
                    MotionPhase::Idle
                } else {
                    MotionPhase::Fall
                },
                contacts: Contacts {
                    ground,
                    ..Contacts::default()
                },
                ..MotionFrame::default()
            },
            tick: 0,
        })
    }

    pub fn frame(&self) -> MotionFrame {
        self.frame
    }

    pub fn collider(&self) -> Collider {
        self.collider
    }

    pub fn step(&mut self, grid: &impl TileGrid, input: MotionInput) -> MotionFrame {
        let Some(tick) = self.tick.checked_add(1) else {
            return self.frame;
        };
        self.tick = tick;
        let prior = self.frame;
        let mut position = prior.position;
        let x = match input.horizontal {
            Horizontal::Left => -RUN_SPEED,
            Horizontal::Neutral => 0,
            Horizontal::Right => RUN_SPEED,
        };
        let jumping = input.jump && prior.grounded;
        let mut velocity = Vector {
            x,
            y: if jumping {
                JUMP_IMPULSE
            } else {
                prior.velocity.y
            },
        };
        velocity.y = (velocity.y + GRAVITY).min(FALL_SPEED);

        let horizontal = Vector { x, y: 0 };
        let mut wall = sweep_tiles(grid, position, self.collider, horizontal);
        if prior.grounded && !jumping {
            if let Some(hit) = wall {
                if let Some(raised) =
                    step_up(grid, position, self.collider, horizontal, hit.contact)
                {
                    position = raised;
                    wall = None;
                }
            }
        }
        if let Some(hit) = wall {
            position = hit.position;
            velocity.x = 0;
        } else {
            position.x += x;
        }

        let vertical = Vector {
            x: 0,
            y: velocity.y,
        };
        let impact = sweep_tiles(grid, position, self.collider, vertical);
        let mut contacts = Contacts {
            wall: wall.map(|hit| hit.contact.observe(prior.contacts.wall, tick)),
            ..Contacts::default()
        };
        if let Some(hit) = impact {
            position = hit.position;
            velocity.y = 0;
            match hit.contact.normal {
                Normal::Up => {
                    contacts.ground = Some(hit.contact.observe(prior.contacts.ground, tick))
                }
                Normal::Down => {
                    contacts.head = Some(hit.contact.observe(prior.contacts.head, tick))
                }
                _ => {}
            }
        } else {
            position.y += vertical.y;
        }
        let grounded = contacts.ground.is_some();
        let landed = contacts.ground.filter(|_| !prior.grounded);
        let head_bump = contacts
            .head
            .filter(|contact| contact.first_seen_tick == tick);
        let wall_contact = contacts
            .wall
            .filter(|contact| contact.first_seen_tick == tick);
        self.frame = MotionFrame {
            previous: prior.position,
            position,
            velocity,
            grounded,
            phase: if landed.is_some() {
                MotionPhase::Land
            } else if grounded {
                if velocity.x == 0 {
                    MotionPhase::Idle
                } else {
                    MotionPhase::Walk
                }
            } else if velocity.y < 0 {
                MotionPhase::Rise
            } else {
                MotionPhase::Fall
            },
            contacts,
            landed,
            head_bump,
            wall_contact,
        };
        self.frame
    }
}

fn step_up(
    grid: &impl TileGrid,
    root: Vector,
    collider: Collider,
    movement: Vector,
    contact: Contact,
) -> Option<Vector> {
    let feet = root.y + collider.bounds.bottom;
    if contact.tile_y * TILE != feet - TILE
        || !matches!(contact.normal, Normal::Left | Normal::Right)
    {
        return None;
    }
    let lift = Vector { x: 0, y: -TILE };
    if sweep_tiles(grid, root, collider, lift)
        .is_some_and(|hit| hit.contact.time_of_impact < TILE as u32)
    {
        return None;
    }
    let raised = Vector {
        x: root.x,
        y: root.y - TILE,
    };
    if sweep_tiles(grid, raised, collider, movement).is_some() {
        return None;
    }
    // Leave X advancement to the caller so both paths publish the same
    // horizontal velocity. The downward sweep checks support at the new X.
    Some(raised)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SweepHit {
    pub position: Vector,
    pub contact: Contact,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RayHit {
    pub point: Vector,
    pub distance: i32,
    pub contact: Contact,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum RayTarget {
    Tile { x: i32, y: i32 },
    Entity(u16),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RoomRayHit {
    pub point: Vector,
    /// Q16 Euclidean distance from the start to the reported hit.
    pub distance: i32,
    pub normal: Normal,
    pub target: RayTarget,
}

/// Cast a bounded AABB over a full motion, even when both endpoints are clear.
/// Ties choose row then column, and X before Y within a tile.
pub fn sweep_tiles(
    grid: &impl TileGrid,
    root: Vector,
    collider: Collider,
    delta: Vector,
) -> Option<SweepHit> {
    sweep(grid, root, collider.bounds, delta, false).map(|hit| hit.public(root, delta))
}

/// Walk the same tile entry planes as the swept AABB using a point shape.
/// Distance is the Q16 Euclidean distance from the start to the reported hit.
pub fn cast_ray(grid: &impl TileGrid, origin: Vector, delta: Vector) -> Option<RayHit> {
    let hit = sweep(grid, origin, Bounds::default(), delta, true)?.public(origin, delta);
    let point = if hit.contact.penetration == 0 {
        hit.position
    } else {
        origin
    };
    Some(RayHit {
        point,
        distance: ray_distance(origin, point),
        contact: hit.contact,
    })
}

pub(super) fn cast_room_ray(
    grid: &impl TileGrid,
    origin: Vector,
    delta: Vector,
    bodies: impl Iterator<Item = (u16, Collider, Vector)>,
) -> Option<RoomRayHit> {
    let size = Dimensions::read(grid)?;
    let point = Bounds::default().at(origin)?;
    let swept = bounded_sweep(size, point, delta)?;
    let tile = sweep_tiles_in(grid, size, point, swept, delta, true).map(|hit| {
        (
            hit.plane,
            RayTarget::Tile {
                x: hit.tile_x,
                y: hit.tile_y,
            },
        )
    });
    let (plane, target) = tile
        .into_iter()
        .chain(bodies.filter_map(|(id, collider, root)| {
            let plane = PlaneHit::against(point, collider.bounds.at(root)?, delta, true)?;
            Some((plane, RayTarget::Entity(id)))
        }))
        .min_by(|(a, target_a), (b, target_b)| {
            a.fraction
                .compare(b.fraction)
                .then_with(|| target_a.cmp(target_b))
        })?;
    let point = plane.position(origin, delta);
    Some(RoomRayHit {
        point,
        distance: ray_distance(origin, point),
        normal: plane.normal,
        target,
    })
}

fn ray_distance(origin: Vector, point: Vector) -> i32 {
    let x = i64::from(point.x - origin.x);
    let y = i64::from(point.y - origin.y);
    ((x * x + y * y) as u64).isqrt() as i32
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct Bounds {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

impl Bounds {
    fn at(self, root: Vector) -> Option<Self> {
        Some(Self {
            left: self.left.checked_add(root.x)?,
            top: self.top.checked_add(root.y)?,
            right: self.right.checked_add(root.x)?,
            bottom: self.bottom.checked_add(root.y)?,
        })
    }

    fn overlaps(self, other: Self) -> bool {
        self.left < other.right
            && self.right > other.left
            && self.top < other.bottom
            && self.bottom > other.top
    }

    fn tile(x: i32, y: i32) -> Self {
        Self {
            left: x * TILE,
            top: y * TILE,
            right: (x + 1) * TILE,
            bottom: (y + 1) * TILE,
        }
    }
}

#[derive(Clone, Copy)]
struct Dimensions {
    width: i32,
    height: i32,
}

impl Dimensions {
    fn read(grid: &impl TileGrid) -> Option<Self> {
        let (width, height) = (grid.width(), grid.height());
        ((1..=MAX_GRID).contains(&width) && (1..=MAX_GRID).contains(&height)).then_some(Self {
            width: width as i32,
            height: height as i32,
        })
    }

    fn contains(self, bounds: Bounds) -> bool {
        bounds.left >= 0
            && bounds.top >= 0
            && bounds.right <= self.width * TILE
            && bounds.bottom <= self.height * TILE
    }

    fn solid(self, grid: &impl TileGrid, x: i32, y: i32) -> bool {
        !(0..self.width).contains(&x)
            || !(0..self.height).contains(&y)
            || grid.is_solid(x as u32, y as u32)
    }

    fn tiles(self, bounds: Bounds) -> impl Iterator<Item = (i32, i32)> {
        // Include the previous cell at an aligned leading edge so contacts
        // exactly at the beginning or end of the motion are observable.
        let x_range = (bounds.left.div_euclid(TILE) - 1).max(-1)
            ..=bounds.right.div_euclid(TILE).min(self.width);
        let y_range = (bounds.top.div_euclid(TILE) - 1).max(-1)
            ..=bounds.bottom.div_euclid(TILE).min(self.height);
        y_range.flat_map(move |y| x_range.clone().map(move |x| (x, y)))
    }
}

fn overlaps_terrain(grid: &impl TileGrid, size: Dimensions, bounds: Bounds) -> bool {
    size.tiles(bounds)
        .any(|(x, y)| size.solid(grid, x, y) && bounds.overlaps(Bounds::tile(x, y)))
}

#[derive(Clone, Copy)]
struct Ratio {
    num: i64,
    den: i64,
}

impl Ratio {
    const ZERO: Self = Self { num: 0, den: 1 };
    const ONE: Self = Self { num: 1, den: 1 };

    fn new(num: i32, den: i32) -> Self {
        Self {
            num: i64::from(num),
            den: i64::from(den),
        }
    }

    fn compare(self, other: Self) -> Ordering {
        (self.num * other.den).cmp(&(other.num * self.den))
    }

    fn scale(self, delta: i32) -> i32 {
        (i64::from(delta) * self.num / self.den) as i32
    }
}

#[derive(Clone, Copy)]
struct PlaneHit {
    fraction: Ratio,
    normal: Normal,
    penetration: i32,
}

impl PlaneHit {
    fn axis(
        min: i32,
        max: i32,
        low: i32,
        high: i32,
        delta: i32,
        point: bool,
        normals: [Normal; 2],
    ) -> Option<(Self, Ratio)> {
        let (entry, exit, normal) = match delta.cmp(&0) {
            Ordering::Greater => (
                Ratio::new(low - max, delta),
                Ratio::new(high - min, delta),
                normals[0],
            ),
            Ordering::Less => (
                Ratio::new(min - high, -delta),
                Ratio::new(max - low, -delta),
                normals[1],
            ),
            Ordering::Equal if min < high && max > low || point && low <= min && min <= high => {
                (Ratio::new(-2, 1), Ratio::new(2, 1), normals[0])
            }
            Ordering::Equal => return None,
        };
        Some((
            Self {
                fraction: entry,
                normal,
                penetration: 0,
            },
            exit,
        ))
    }

    fn against(bounds: Bounds, obstacle: Bounds, delta: Vector, point: bool) -> Option<Self> {
        if bounds.overlaps(obstacle) {
            let candidates = [
                (bounds.right - obstacle.left, Normal::Left),
                (obstacle.right - bounds.left, Normal::Right),
                (bounds.bottom - obstacle.top, Normal::Up),
                (obstacle.bottom - bounds.top, Normal::Down),
            ];
            let (penetration, normal) = candidates.into_iter().min_by_key(|&(depth, _)| depth)?;
            return Some(Self {
                fraction: Ratio::ZERO,
                normal,
                penetration,
            });
        }
        let (x, x_exit) = Self::axis(
            bounds.left,
            bounds.right,
            obstacle.left,
            obstacle.right,
            delta.x,
            point,
            [Normal::Left, Normal::Right],
        )?;
        let (y, y_exit) = Self::axis(
            bounds.top,
            bounds.bottom,
            obstacle.top,
            obstacle.bottom,
            delta.y,
            point,
            [Normal::Up, Normal::Down],
        )?;
        let entry = if x.fraction.compare(y.fraction).is_lt() {
            y
        } else {
            x
        };
        (entry.fraction.compare(Ratio::ZERO).is_ge()
            && entry.fraction.compare(Ratio::ONE).is_le()
            && entry.fraction.compare(x_exit).is_le()
            && entry.fraction.compare(y_exit).is_le())
        .then_some(entry)
    }

    fn position(self, origin: Vector, delta: Vector) -> Vector {
        Vector {
            x: origin.x + self.fraction.scale(delta.x),
            y: origin.y + self.fraction.scale(delta.y),
        }
    }
}

struct TileHit {
    plane: PlaneHit,
    tile_x: i32,
    tile_y: i32,
}

impl TileHit {
    fn public(self, origin: Vector, delta: Vector) -> SweepHit {
        let plane = self.plane;
        let mut position = plane.position(origin, delta);
        let impact_speed = match plane.normal {
            Normal::Left => {
                position.x -= plane.penetration;
                delta.x.max(0)
            }
            Normal::Right => {
                position.x += plane.penetration;
                (-delta.x).max(0)
            }
            Normal::Up => {
                position.y -= plane.penetration;
                delta.y.max(0)
            }
            Normal::Down => {
                position.y += plane.penetration;
                (-delta.y).max(0)
            }
        };
        SweepHit {
            position,
            contact: Contact {
                tile_x: self.tile_x,
                tile_y: self.tile_y,
                normal: plane.normal,
                time_of_impact: plane.fraction.scale(TILE) as u32,
                impact_speed,
                penetration: plane.penetration,
                first_seen_tick: 0,
            },
        }
    }
}

fn sweep(
    grid: &impl TileGrid,
    origin: Vector,
    shape: Bounds,
    delta: Vector,
    point: bool,
) -> Option<TileHit> {
    let size = Dimensions::read(grid)?;
    let bounds = shape.at(origin)?;
    let swept = bounded_sweep(size, bounds, delta)?;
    sweep_tiles_in(grid, size, bounds, swept, delta, point)
}

fn bounded_sweep(size: Dimensions, bounds: Bounds, delta: Vector) -> Option<Bounds> {
    if delta == Vector::default()
        || ![
            -MAX_DISPLACEMENT..=MAX_DISPLACEMENT,
            -MAX_DISPLACEMENT..=MAX_DISPLACEMENT,
        ]
        .into_iter()
        .zip([delta.x, delta.y])
        .all(|(range, value)| range.contains(&value))
        || bounds.left < -TILE
        || bounds.top < -TILE
        || bounds.right > (size.width + 1) * TILE
        || bounds.bottom > (size.height + 1) * TILE
    {
        return None;
    }
    Some(Bounds {
        left: bounds.left + delta.x.min(0),
        top: bounds.top + delta.y.min(0),
        right: bounds.right + delta.x.max(0),
        bottom: bounds.bottom + delta.y.max(0),
    })
}

fn sweep_tiles_in(
    grid: &impl TileGrid,
    size: Dimensions,
    bounds: Bounds,
    swept: Bounds,
    delta: Vector,
    point: bool,
) -> Option<TileHit> {
    size.tiles(swept)
        .filter(|&(x, y)| size.solid(grid, x, y))
        .filter_map(|(x, y)| {
            PlaneHit::against(bounds, Bounds::tile(x, y), delta, point).map(|plane| TileHit {
                plane,
                tile_x: x,
                tile_y: y,
            })
        })
        .min_by(|a, b| {
            a.plane
                .fraction
                .compare(b.plane.fraction)
                .then_with(|| (a.tile_y, a.tile_x).cmp(&(b.tile_y, b.tile_x)))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Grid {
        added: &'static [(u32, u32)],
    }

    impl TileGrid for Grid {
        fn width(&self) -> u32 {
            10
        }

        fn height(&self) -> u32 {
            8
        }

        fn is_solid(&self, x: u32, y: u32) -> bool {
            assert!(x < self.width() && y < self.height());
            y >= 6 || self.added.contains(&(x, y))
        }
    }

    fn ground() -> Grid {
        Grid { added: &[] }
    }

    fn at(x: i32, y: i32) -> Vector {
        Vector { x, y }
    }

    #[test]
    fn jump_apex_and_landing_are_fixed_tick_and_a_single_event() {
        let grid = ground();
        let floor = at(3 * TILE + TILE / 2, 6 * TILE);
        let mut character = Character::spawn(&grid, floor, Collider::default()).unwrap();
        assert!(character.frame().grounded);
        let run: Vec<_> = (1..=35)
            .map(|tick| {
                character.step(
                    &grid,
                    MotionInput {
                        jump: tick == 1,
                        ..MotionInput::default()
                    },
                )
            })
            .collect();
        let apex = run
            .iter()
            .enumerate()
            .min_by_key(|(_, frame)| frame.position.y)
            .unwrap();
        assert_eq!(apex.0 + 1, 14);
        assert_eq!(floor.y - apex.1.position.y, 61_117);
        assert_eq!(run[0].phase, MotionPhase::Rise);
        assert_eq!(run[14].phase, MotionPhase::Fall);
        let landings: Vec<_> = run
            .iter()
            .enumerate()
            .filter(|(_, frame)| frame.landed.is_some())
            .collect();
        assert_eq!(landings.len(), 1);
        let (index, landed) = landings[0];
        assert_eq!(index + 1, 29);
        assert_eq!(landed.phase, MotionPhase::Land);
        assert_eq!(landed.position, floor);
        assert_eq!(landed.previous, run[index - 1].position);
        let event = landed.landed.unwrap();
        assert_eq!(event.normal, Normal::Up);
        assert_eq!(event.first_seen_tick, 29);
        assert!(event.impact_speed > 0);
        assert_eq!(run[34].contacts.ground.unwrap().first_seen_tick, 29);
    }

    #[test]
    fn sweep_stops_at_first_plane_even_when_endpoints_are_clear() {
        let grid = Grid { added: &[(3, 4)] };
        let root = at(2 * TILE + TILE * 5 / 8, 5 * TILE);
        let collider = Collider::new(TILE / 8, TILE * 3 / 4).unwrap();
        let delta = at(TILE * 3 / 2, 0);
        assert!(Character::spawn(&grid, root, collider).is_some());
        assert!(Character::spawn(&grid, at(root.x + delta.x, root.y), collider).is_some());
        let hit = sweep_tiles(&grid, root, collider, delta).unwrap();
        assert_eq!((hit.contact.tile_x, hit.contact.tile_y), (3, 4));
        assert_eq!(hit.contact.normal, Normal::Left);
        assert_eq!(hit.contact.time_of_impact, TILE as u32 / 6);
        assert_eq!(hit.position, at(3 * TILE - TILE / 8, 5 * TILE));
        let threshold_hit = sweep_tiles(&grid, root, collider, at(TILE, 0)).unwrap();
        assert_eq!(threshold_hit.contact.time_of_impact, TILE as u32 / 4);
    }

    #[test]
    fn step_up_requires_head_clearance_and_wall_events_are_stable() {
        let root = at(4 * TILE - TILE / 4, 6 * TILE);
        let motion = MotionInput {
            horizontal: Horizontal::Right,
            jump: false,
        };
        let step = Grid { added: &[(4, 5)] };
        let mut character = Character::spawn(&step, root, Collider::default()).unwrap();
        let raised = character.step(&step, motion);
        assert_eq!(raised.position, at(root.x + RUN_SPEED, root.y - TILE));
        assert!(raised.grounded);
        assert!(raised.wall_contact.is_none());
        assert!(raised.landed.is_none());

        let blocked = Grid {
            added: &[(4, 4), (4, 5)],
        };
        let mut character = Character::spawn(&blocked, root, Collider::default()).unwrap();
        let first = character.step(&blocked, motion);
        assert_eq!(first.position, root);
        assert_eq!(first.wall_contact.unwrap().first_seen_tick, 1);
        let second = character.step(&blocked, motion);
        assert!(second.wall_contact.is_none());
        assert_eq!(second.contacts.wall.unwrap().first_seen_tick, 1);
        character.step(&blocked, MotionInput::default());
        assert_eq!(
            character
                .step(&blocked, motion)
                .wall_contact
                .unwrap()
                .first_seen_tick,
            4
        );
    }

    #[test]
    fn upward_motion_resolves_ceiling_once_and_returns_to_fall() {
        let grid = Grid { added: &[(3, 4)] };
        let root = at(3 * TILE + TILE / 2, 6 * TILE);
        let mut character = Character::spawn(&grid, root, Collider::default()).unwrap();
        let mut head = None;
        for tick in 1..20 {
            let frame = character.step(
                &grid,
                MotionInput {
                    jump: tick == 1,
                    ..MotionInput::default()
                },
            );
            if let Some(bump) = frame.head_bump {
                assert!(head.replace(bump).is_none());
                assert_eq!(frame.phase, MotionPhase::Fall);
                assert_eq!(frame.position.y, 5 * TILE + TILE * 3 / 4);
                assert_eq!(frame.velocity.y, 0);
            }
        }
        assert_eq!(head.unwrap().normal, Normal::Down);
    }

    #[test]
    fn queries_are_read_only_and_report_solid_boundary_planes() {
        let grid = ground();
        let origin = at(2 * TILE + TILE / 2, 5 * TILE);
        let hit = cast_ray(&grid, origin, at(0, 2 * TILE)).unwrap();
        assert_eq!(hit.contact.normal, Normal::Up);
        assert_eq!(hit.distance, TILE);
        assert_eq!(hit.point, at(origin.x, 6 * TILE));
        assert!(cast_ray(&grid, origin, at(0, -TILE)).is_none());
        let boundary = cast_ray(&grid, origin, at(-4 * TILE, 0)).unwrap();
        assert_eq!(boundary.point.x, 0);
        assert_eq!(boundary.contact.tile_x, -1);
        assert_eq!(boundary.contact.normal, Normal::Right);

        let shape = Collider::default();
        let moving = Character::spawn(&grid, origin, shape).unwrap();
        let before = moving.frame();
        assert!(shape.overlaps(origin, shape, at(origin.x + 1, origin.y)));
        assert!(!shape.overlaps(origin, shape, at(origin.x + TILE / 2, origin.y)));
        assert_eq!(moving.frame(), before);
        assert!(Collider::new(i32::MIN, TILE).is_none());
        assert!(Character::spawn(&grid, at(origin.x, 7 * TILE), shape).is_none());
    }
}
