use super::{text, Attribute, Node, Tag};

pub(super) const BODY: &[Node] = &[
    element!(Div [Class = "intro"];
        element!(H1; text("Physics for the Forge Game Runtime")),
        element!(P;
            element!(Strong; text("Design proposal.")),
            text(" Give the Scout gravity, jumps and collision response while keeping fixed-tick replay and the existing v1 snapshot layout. The Room One playtest still uses tile movement."),
        ),
        element!(Nav [Class = "section-nav", AriaLabel = "Game physics design sections"];
            element!(A [Href = "#fixed-tick"]; text("Fixed tick")),
            element!(A [Href = "#animation"]; text("Animation and contacts")),
            element!(A [Href = "#authority"]; text("Transform ownership")),
            element!(A [Href = "#rollout"]; text("Rollout")),
        ),
    ),
    element!(Section [Id = "fixed-tick", Class = "design-section", AriaLabelledby = "fixed-tick-title"];
        element!(H2 [Id = "fixed-tick-title"]; text("One physics step per fixed tick")),
        element!(P; text("Room One starts with a kinematic character controller and axis-aligned boxes against the static tile grid. At 60 Hz, the controller integrates velocity before position. Bounded tile candidates are tested in a stable order, with a swept test for moves that exceed the discrete safety bound.")),
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
                    element!(Li; text("InputApply turns an input edge into a velocity target or jump impulse.")),
                    element!(Li; text("Move applies kinematic targets and integrates simulated motion in fixed-point tile units.")),
                    element!(Li; text("Collide resolves axis-aligned contacts and records the tick’s grounded state, velocity and new contact events.")),
                    element!(Li; text("Animate chooses a pose from those facts. Effects delivers queued events. Publication pairs the resolved root with the pose from the same tick.")),
                ),
            ),
        ),
    ),
    element!(Section [Id = "animation", Class = "design-section", AriaLabelledby = "animation-title"];
        element!(H2 [Id = "animation-title"]; text("Animation and contacts")),
        element!(P; text("The animation bridge reads grounded state and vertical motion from the same tick that resolved collisions. It selects rise, fall and landing poses, advances clip time from frame durations, and attaches sprites to the resolved root.")),
        element!(Div [Class = "gallery contract-sequences"];
            element!(Figure [Class = "wireframe"];
                element!(Figcaption;
                    element!(H3; text("Landing and pose publication sequence")),
                    element!(P; text("Follow a new landing contact through same-tick pose selection and the next rendered frame.")),
                ),
                element!(A [Class = "drawing", Href = "/design/game-physics/animation.svg"];
                    element!(Img [Src = "/design/game-physics/animation.svg", Width = "1050", Height = "1387", Loading = "lazy", Alt = "Collision queues a landing event. The renderer pairs the snapshot position with the proposed same-tick pose payload."]),
                    element!(Span [Class = "open-label"]; text("Open full-size animation diagram")),
                ),
            ),
        ),
        element!(P; text("A persistent contact continues resolving across ticks. Its first-seen identity controls one-time delivery of Landed, HeadBump or WallContact through the event ring. Action boxes query overlap only on their active frames.")),
        element!(P; text("Migrate the Scout’s jump first. Zero the preview’s airborne root offsets before export, preserve the limb poses, and validate the body box against the same logical pivot in each atlas. Ledge catches, ladders, ropes and barrel traversal need their own authored contact transitions before becoming playable.")),
    ),
    element!(Section [Id = "authority", Class = "design-section", AriaLabelledby = "authority-title"];
        element!(H2 [Id = "authority-title"]; text("Transform ownership and units")),
        element!(Dl [Class = "storage"];
            element!(Div;
                element!(Dt; text("Static terrain and body motion")),
                element!(Dd; text("Terrain comes from level data. Gameplay supplies kinematic targets before the step. The solver owns dynamic transforms.")),
            ),
            element!(Div;
                element!(Dt; text("World placement and pose")),
                    element!(Dd; text("The simulation resolves the root in Q16 tile units with positive Y down. Animation writes the local pose, and the collider is offset from the logical recipe pivot.")),
            ),
            element!(Div;
                element!(Dt; text("Snapshot and atlas")),
                    element!(Dd; text("Publication converts current and previous positions to the existing y-up Q8 snapshot. The v1 entity record has no pose field. A proposed companion payload pairs the discrete pose with the root by run epoch, tick and entity ID. Rendering interpolates the root, while atlas pixels select the artwork.")),
            ),
        ),
        element!(Div [Class = "gallery contract-sequences"];
            element!(Figure [Class = "wireframe"];
                element!(Figcaption; element!(H3; text("Transform and data authority"))),
                element!(A [Class = "drawing", Href = "/design/game-physics/authority.svg"];
                    element!(Img [Src = "/design/game-physics/authority.svg", Width = "864", Height = "522", Loading = "lazy", Alt = "Physics owns the resolved root and animation owns the local pose. The renderer joins the snapshot and proposed pose payload."]),
                    element!(Span [Class = "open-label"]; text("Open full-size authority diagram")),
                ),
            ),
        ),
    ),
    element!(Section [Id = "rollout", Class = "design-section", AriaLabelledby = "rollout-title"];
        element!(H2 [Id = "rollout-title"]; text("Rollout and acceptance")),
        element!(P; text("Validate tile contacts and fixed-tick replay first, then drive the migrated Scout’s jump from the runtime. Check that grounded state and pose share the same tick, a persistent contact emits once, and high-speed moves stop at the expected contact plane.")),
        element!(P; text("A later milestone can evaluate a physics middleware adapter when the game needs joints, stacking or non-tile shapes. The step, event and snapshot contracts define that seam.")),
        element!(Nav [Class = "section-nav", AriaLabel = "Related game designs"];
            element!(A [Href = "https://github.com/roachmaster/homelab/blob/main/docs/architecture/proposals/forge-game-physics.md"]; text("Full physics proposal")),
            element!(A [Href = "/design/game-studio"]; text("Game engine design")),
            element!(A [Href = "/playtest"]; text("Open Room One playtest")),
        ),
    ),
];
