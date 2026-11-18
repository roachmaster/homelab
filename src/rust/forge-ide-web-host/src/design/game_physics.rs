use super::{text, Attribute, Node, Tag};

pub(super) const BODY: &[Node] = &[
    element!(Div [Class = "intro"];
        element!(H1; text("Physics for the Forge Game Runtime")),
        element!(P;
            element!(Strong; text("Implemented repository slice.")),
            text(" Reviewed 18 November 2026. Traversal Yard has Q16 gravity, jumping, swept tile contacts, same-tick animation and collision queries. General rigid bodies and authored climbing remain future work."),
        ),
        element!(P; text("In the web playtest build, choose Traversal Yard to try the fixed-point controller. Room One is the four-direction puzzle.")),
        element!(Nav [Class = "section-nav", AriaLabel = "Game physics design sections"];
            element!(A [Href = "#fixed-tick"]; text("Fixed tick")),
            element!(A [Href = "#collision-world"]; text("Collision world")),
            element!(A [Href = "#animation"]; text("Animation and contacts")),
            element!(A [Href = "#authority"]; text("Transform ownership")),
            element!(A [Href = "#rollout"]; text("Rollout")),
        ),
    ),
    element!(Section [Id = "fixed-tick", Class = "design-section", AriaLabelledby = "fixed-tick-title"];
        element!(H2 [Id = "fixed-tick-title"]; text("One physics step per fixed tick")),
        element!(P; text("Traversal Yard uses a kinematic character controller and an axis-aligned body box against the static tile grid. At 60 Hz, the controller integrates velocity before position. It tests bounded tile candidates in a stable order and sweeps to the earliest blocking contact.")),
        element!(Div [Class = "gallery contract-sequences"];
            element!(Figure [Class = "wireframe"];
                element!(Figcaption;
                    element!(H3; text("Fixed tick sequence")),
                    element!(P; text("Apply input, resolve motion and publish one committed tick.")),
                ),
                element!(A [Class = "drawing", Href = "/design/game-physics/tick.svg"];
                    element!(Img [Src = "/design/game-physics/tick.svg", Width = "860", Height = "1212", Alt = "Fixed tick sequence from input and motion integration through collision, animation, effects and snapshot publication."]),
                    element!(Span [Class = "open-label"]; text("Open full-size fixed tick diagram")),
                ),
                element!(Ol [Class = "contract-steps"];
                    element!(Li; text("Room consumes ordered edges and held input. Character::step applies horizontal intent and a grounded jump edge.")),
                    element!(Li; text("The character controller integrates fixed-point motion, sweeps X then Y, and applies a bounded step-up rule.")),
                    element!(Li; text("MotionFrame retains the resolved root, velocity, grounded state and contacts. New landing, head and wall contacts feed the event ring.")),
                    element!(Li; text("Animator selects Idle, Walk, Rise, Fall or Land from those facts. Publication pairs FGS1 world bytes with FGP1 pose bytes from the same epoch and tick.")),
                ),
            ),
        ),
    ),
    element!(Section [Id = "collision-world", Class = "design-section", AriaLabelledby = "collision-world-title"];
        element!(H2 [Id = "collision-world-title"]; text("Collision world and diagnostics")),
        element!(P; text("Cooked levels validate solid tiles, the named surface and foot-relative collider before activation. Character::spawn rejects an invalid or penetrating start; Character::step uses axis-separated sweeps against the tile grid. The character has zero restitution. This is a character controller, not a dynamic-body solver.")),
        element!(P; text("The implemented APIs are sweep_tiles and cast_ray for terrain, plus Room::cast_ray, Room::collision_bodies and Room::phantom_overlap for filtered room entities. Ray ties use stable target order. Phantom overlap has bounded stable-ID results and counts dropped hits. Combat action boxes can use that query seam, but no combat controller is shipped.")),
        element!(P; text("Debug tools draw body boxes, current contact normals and penetration, and the forward ray hit from FGC1 diagnostics. Grounded and motion phase appear in the pose and inspector. There is no sleeping-body system; drawing diagnostics does not mutate the simulation.")),
    ),
    element!(Section [Id = "animation", Class = "design-section", AriaLabelledby = "animation-title"];
        element!(H2 [Id = "animation-title"]; text("Animation and contacts")),
        element!(P; text("The animation bridge reads grounded state and vertical motion from the same tick that resolved collisions. It selects rise, fall and landing poses, advances clip time from frame durations, and attaches sprites to the resolved root.")),
        element!(Div [Class = "gallery contract-sequences"];
            element!(Figure [Class = "wireframe"];
                element!(Figcaption;
                    element!(H3; text("Landing and pose publication sequence")),
                    element!(P; text("Follow a new landing contact through the implemented same-tick pose publication.")),
                ),
                element!(A [Class = "drawing", Href = "/design/game-physics/animation.svg"];
                    element!(Img [Src = "/design/game-physics/animation.svg", Width = "1050", Height = "1387", Loading = "lazy", Alt = "Character resolves a landing. Animator selects the pose and the renderer joins FGS1 world data with the implemented FGP1 payload."]),
                    element!(Span [Class = "open-label"]; text("Open full-size animation diagram")),
                ),
            ),
        ),
        element!(P; text("A persistent contact continues resolving across ticks. Contact-edge facts control delivery of Landed, HeadBump and WallContact through the event ring. The phantom overlap API is available for future action boxes; active combat-frame logic is not implemented.")),
        element!(P; text("The cooker already zeros the illustrated jump's authored airborne root offsets in a temporary export recipe. Runtime physics supplies travel while the atlas supplies limb poses. Ledge catches, ladders, ropes, combat and barrel traversal remain animation assets or future contact-state work; they are not playable mechanics yet.")),
    ),
    element!(Section [Id = "authority", Class = "design-section", AriaLabelledby = "authority-title"];
        element!(H2 [Id = "authority-title"]; text("Transform ownership and units")),
        element!(Dl [Class = "storage"];
            element!(Div;
                element!(Dt; text("Static terrain and body motion")),
                element!(Dd; text("Terrain and collider data come from the validated level. Character owns the platform player's resolved transform. Room One uses discrete puzzle movement; a general dynamic-body or moving-platform solver is not implemented.")),
            ),
            element!(Div;
                element!(Dt; text("World placement and pose")),
                    element!(Dd; text("The simulation resolves the root in Q16 tile units with positive Y down. Animation writes the local pose, and the collider is offset from the logical recipe pivot.")),
            ),
            element!(Div;
                element!(Dt; text("Snapshot and atlas")),
                    element!(Dd; text("Publication converts current and previous positions to the y-up Q8 FGS1 snapshot. Its entity record stays unchanged. The implemented 32-byte FGP1 companion carries epoch, tick, entity ID, phase, frame, facing, grounded and contact facts. The renderer verifies that identity before combining root and artwork.")),
            ),
        ),
        element!(Div [Class = "gallery contract-sequences"];
            element!(Figure [Class = "wireframe"];
                element!(Figcaption; element!(H3; text("Transform and data authority"))),
                element!(A [Class = "drawing", Href = "/design/game-physics/authority.svg"];
                    element!(Img [Src = "/design/game-physics/authority.svg", Width = "864", Height = "522", Loading = "lazy", Alt = "Character owns the resolved root and Animator selects PlayerPose. The renderer joins FGS1 and FGP1 by epoch, tick and player ID."]),
                    element!(Span [Class = "open-label"]; text("Open full-size authority diagram")),
                ),
            ),
        ),
    ),
    element!(Section [Id = "rollout", Class = "design-section", AriaLabelledby = "rollout-title"];
        element!(H2 [Id = "rollout-title"]; text("Rollout and acceptance")),
        element!(P; text("Implemented tests cover swept contacts, step-up bounds, room ray and phantom filtering, contact-edge events, same-tick pose identity and replay. The actual-WASM playtest checks the 29-tick jump and Traversal Yard completion. A fresh test run is recorded by the repository's QA gate.")),
        element!(P; text("A later milestone can evaluate a physics middleware adapter when the game needs joints, stacking or non-tile shapes. The step, event and snapshot contracts define that seam.")),
        element!(Nav [Class = "section-nav", AriaLabel = "Related game designs"];
            element!(A [Href = "https://github.com/roachmaster/homelab/blob/main/docs/architecture/proposals/forge-game-physics.md"]; text("Full physics implementation reference")),
            element!(A [Href = "/design/game-studio"]; text("Game engine design")),
            element!(A [Href = "/design/game-asset-pipeline"]; text("Game asset pipeline design")),
            element!(A [Href = "/playtest"]; text("Open Room One playtest")),
        ),
    ),
];
