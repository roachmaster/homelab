use super::{markup, text, Attribute, Node, Tag};

macro_rules! art_stage {
    ($title:literal, $caption:literal, $src:literal, $class:literal, $width:literal, $height:literal) => {
        element!(Li;
            element!(Figure [Class = "character-stage"];
                element!(Div [Class = "character-stage-art"];
                    element!(Img [Src = $src, Class = $class, Width = $width, Height = $height, Loading = "lazy", Alt = $caption]),
                ),
                element!(Figcaption;
                    element!(H3; text($title)),
                    element!(P; text($caption)),
                ),
            ),
        )
    };
}

macro_rules! view {
    ($title:literal, $caption:literal, $src:literal) => {
        element!(Figure;
            element!(Div [Class = "character-view-art"];
                element!(Img [Src = $src, Width = "128", Height = "176", Loading = "lazy", Alt = $caption]),
            ),
            element!(Figcaption;
                element!(H3; text($title)),
                element!(P; text($caption)),
            ),
        )
    };
}

macro_rules! clip {
    ($title:literal, $timing:literal, $description:literal, $gif:literal, $strip:literal, $width:literal) => {
        element!(Details [Class = "character-clip"];
            element!(Summary;
                element!(Span;
                    element!(Strong; text($title)),
                    element!(Small; text($timing)),
                ),
            ),
            element!(Div [Class = "character-clip-content"];
                element!(Div [Class = "character-motion-frame"];
                    element!(Img [Class = "character-motion", Src = $gif, Width = "128", Height = "176", Loading = "lazy", Alt = $description]),
                    element!(Img [Class = "character-motion-still", Src = $strip, Width = "128", Height = "176", Loading = "lazy", Alt = $description]),
                ),
                element!(Div [Class = "character-frame-reference"];
                    element!(P; text("Frame-by-frame reference")),
                    element!(Div [Class = "character-strip", Tabindex = "0", Role = "region", AriaLabel = $title];
                        element!(Img [Src = $strip, Width = $width, Height = "176", Loading = "lazy", Alt = $description]),
                    ),
                    element!(A [Href = $strip, Class = "character-frame-link"]; text("Open full-size frames →")),
                ),
            ),
        )
    };
}

pub(super) const BODY: &[Node] = &[
    element!(Section [Class = "character-hero", AriaLabelledby = "character-title"];
        element!(Div [Class = "character-hero-copy"];
            element!(P [Class = "character-eyebrow"]; text("Room One / Scout")),
            element!(H1 [Id = "character-title"]; text("Scout character creation")),
            element!(P [Class = "character-lede"]; text("Build the pose first. Layer a pixel finish or a clean illustrated skin over the same character rig.")),
            element!(Div [Class = "character-actions"];
                element!(A [Href = "#construction", Class = "character-primary"]; text("See the creation steps ↓")),
                element!(A [Href = "#animations"]; text("Explore 16 clips")),
            ),
        ),
        element!(Figure [Class = "character-hero-art"];
            element!(A [Href = "/design/characters/scout-illustrated-idle.svg", AriaLabel = "Open the Scout illustration"];
                element!(Img [Src = "/design/characters/scout-illustrated-idle.svg", Width = "128", Height = "176", Alt = "Scout in a gold helmet and blue suit, with a tan pack, cyan visor and handheld probe."]),
            ),
            element!(Figcaption; text("Illustrated skin · 4× export")),
        ),
    ),
    element!(Section [Id = "construction", Class = "character-section", AriaLabelledby = "character-construction-title"];
        element!(H2 [Id = "character-construction-title"]; text("One rig, three finishes")),
        element!(P [Class = "character-section-intro"]; text("The body parts, poses, timing and contact points stay together. The drawing layers change.")),
        element!(Ol [Class = "character-progression"];
            art_stage!("Mannequin", "Block out the head, torso and limbs. Test proportions and joint placement before adding costume details.", "/design/characters/mannequin-idle.png", "character-pixel", "32", "44"),
            art_stage!("Pixel finish", "Add the Scout silhouette, color palette, equipment and lighting over the approved base.", "/design/characters/pixel-idle.png", "character-pixel", "32", "44"),
            art_stage!("Illustrated skin", "Replace pixel clusters with editable vector parts, smooth joint overlaps, material shading and rim highlights.", "/design/characters/scout-illustrated-idle.svg", "character-illustrated", "128", "176"),
        ),
    ),
    element!(Section [Id = "views", Class = "character-section", AriaLabelledby = "character-views-title"];
        element!(H2 [Id = "character-views-title"]; text("Designed to turn and reach")),
        element!(P [Class = "character-section-intro"]; text("Dedicated views keep the visor, chest strap and pack readable through spins and climbing poses.")),
        element!(Div [Class = "character-views"];
            view!("Front", "The visor, chest strap and equipment are drawn for the front view.", "/design/characters/scout-illustrated-front.svg"),
            view!("Back", "The pack and rear harness take over as Scout turns away.", "/design/characters/scout-illustrated-back.svg"),
            view!("Ladder", "A rear climbing view supports alternating grips and overhead reach.", "/design/characters/scout-illustrated-ladder.svg"),
        ),
    ),
    element!(Section [Id = "animations", Class = "character-section", AriaLabelledby = "character-animation-title"];
        element!(H2 [Id = "character-animation-title"]; text("Animation library")),
        element!(P [Class = "character-section-intro"]; text("Open a clip to watch the motion and inspect its drawing frames. Close the clip to hide the preview. Reduced-motion mode shows a still pose.")),
        element!(Div [Class = "character-library"];
            element!(Div [Class = "character-category"];
                element!(H3; text("Movement")),
                element!(Div [Class = "character-clips"];
                    clip!("Idle", "1 frame · 240 ms", "Scout's neutral standing pose.", "/design/characters/clips/idle.gif", "/design/characters/clips/idle.png", "128"),
                    clip!("Walk", "4 frames · 380 ms", "Scout's walk cycle with opposing arm and leg movement.", "/design/characters/clips/walk.gif", "/design/characters/clips/walk.png", "512"),
                    clip!("Crouch", "4 frames · 545 ms", "Scout lowers the hips and bends the knees over planted feet.", "/design/characters/clips/crouch.gif", "/design/characters/clips/crouch.png", "512"),
                    clip!("Jump", "12 frames · 765 ms", "Compression, arm drive, knee tuck, descent and landing in Scout's jump.", "/design/characters/clips/jump.gif", "/design/characters/clips/jump.png", "1536"),
                    clip!("Spin", "4 frames · 295 ms", "Scout turns through side, front and back views.", "/design/characters/clips/spin.gif", "/design/characters/clips/spin.png", "512"),
                    clip!("Roll", "9 frames · 650 ms", "Scout tucks and rotates while the pack and helmet follow the pose.", "/design/characters/clips/roll.gif", "/design/characters/clips/roll.png", "1152"),
                ),
            ),
            element!(Div [Class = "character-category"];
                element!(H3; text("Combat poses")),
                element!(Div [Class = "character-clips"];
                    clip!("Probe attack", "3 frames · 315 ms", "Scout's original probe attack poses.", "/design/characters/clips/attack.gif", "/design/characters/clips/attack.png", "384"),
                    clip!("Punch", "4 frames · 325 ms", "Scout extends the fist through the punch and recovery.", "/design/characters/clips/punch.gif", "/design/characters/clips/punch.png", "512"),
                    clip!("Uppercut", "5 frames · 405 ms", "Scout drives an uppercut from the lowered stance.", "/design/characters/clips/uppercut.gif", "/design/characters/clips/uppercut.png", "640"),
                    clip!("Kick", "6 frames · 520 ms", "Scout lifts and extends the kicking leg, then recovers.", "/design/characters/clips/kick.gif", "/design/characters/clips/kick.png", "768"),
                    clip!("Roundhouse kick", "7 frames · 530 ms", "Scout turns the body through the roundhouse kick.", "/design/characters/clips/roundhouse_kick.gif", "/design/characters/clips/roundhouse_kick.png", "896"),
                ),
            ),
            element!(Div [Class = "character-category"];
                element!(H3; text("Climbing and parkour")),
                element!(Div [Class = "character-clips"];
                    clip!("Ledge pull-up", "9 frames · 900 ms", "Scout catches an edge, hangs and shifts weight onto the platform.", "/design/characters/clips/ledge_pull_up.gif", "/design/characters/clips/ledge_pull_up.png", "1152"),
                    clip!("Rope climb", "8 frames · 610 ms", "Scout alternates grips and braces against a rope.", "/design/characters/clips/rope_climb.gif", "/design/characters/clips/rope_climb.png", "1024"),
                    clip!("Ladder climb", "8 frames · 610 ms", "Scout alternates hands and feet through the ladder climb.", "/design/characters/clips/ladder_climb.gif", "/design/characters/clips/ladder_climb.png", "1024"),
                    clip!("Barrel vault", "9 frames · 770 ms", "Scout reaches across a barrel and tucks past it.", "/design/characters/clips/barrel_vault.gif", "/design/characters/clips/barrel_vault.png", "1152"),
                    clip!("Barrel jump-off", "10 frames · 824 ms", "Scout pushes from a barrel top and lands on the floor.", "/design/characters/clips/barrel_jump_off.gif", "/design/characters/clips/barrel_jump_off.png", "1280"),
                ),
            ),
        ),
        element!(P [Class = "character-footnote"]; text("16 authored clips · 103 frames. GIF previews round timing to centiseconds; the exported manifest retains the exact frame durations. Walls, ropes, ladders and barrels belong to the level, outside the character atlas.")),
    ),
    element!(Section [Id = "handoff", Class = "character-section", AriaLabelledby = "character-handoff-title"];
        element!(H2 [Id = "character-handoff-title"]; text("Ready for the same game footprint")),
        element!(Dl [Class = "character-contract"];
            element!(Div; element!(Dt; text("Logical canvas")), element!(Dd; text("32 × 44"))),
            element!(Div; element!(Dt; text("Illustrated frame")), element!(Dd; text("128 × 176"))),
            element!(Div; element!(Dt; text("Atlas scale")), element!(Dd; text("4"))),
            element!(Div; element!(Dt; text("Logical pivot")), element!(Dd; text("13, 41"))),
        ),
        element!(P [Class = "character-section-intro"]; text("The atlas changes the drawing resolution while pivots and hitboxes stay in logical units. Room One currently selects idle, walk and jump phases; combat and climbing clips are authored artwork awaiting their playable mechanics.")),
        element!(P [Class = "character-footnote"]; text("This full atlas is the authoring pose set. Production cooking removes the jump’s baked root displacement so runtime physics owns its world motion.")),
        element!(Div [Class = "character-actions"];
            element!(A [Href = "/playtest", Class = "character-primary"]; text("Try Scout in Room One →")),
            element!(A [Href = "/design/characters/illustrated-sheet.png"]; text("Open full atlas")),
            element!(A [Href = "/design/characters/illustrated-manifest.json"]; text("Export manifest")),
        ),
    ),
    element!(Section [Class = "character-section", AriaLabelledby = "character-workflow-title"];
        element!(H2 [Id = "character-workflow-title"]; text("The creation workflow")),
        element!(Ol [Class = "character-workflow"];
            element!(Li;
                element!(A [Href = "https://github.com/roachmaster/homelab/blob/main/.agents/skills/build-pixel-character-base/SKILL.md"]; text("Build the character base")),
                element!(P; text("Use editable body parts, layered drawing operations, joint transforms and hitboxes to establish each pose.")),
            ),
            element!(Li;
                element!(A [Href = "https://github.com/roachmaster/homelab/blob/main/.agents/skills/finish-pixel-character-sprites/SKILL.md"]; text("Finish the pixel skin")),
                element!(P; text("Approve silhouette, palette and lighting while retaining the rig and clip timing.")),
            ),
            element!(Li;
                element!(A [Href = "https://github.com/roachmaster/homelab/blob/main/.agents/skills/illustrate-character-skin/SKILL.md"]; text("Illustrate the 4× skin")),
                element!(P; text("Draw smooth vector parts and view overrides, supersample the frame, then export the shared scale contract.")),
            ),
        ),
        element!(Div [Class = "character-source-links"];
            element!(A [Href = "https://github.com/roachmaster/homelab/tree/main/src/python/pixel-art/examples"]; text("Editable Scout rig and skin sources →")),
            element!(A [Href = "/design/game-asset-pipeline"]; text("Cooking and game asset design →")),
        ),
    ),
];

const STYLES: markup::Styles = &[
    (
        ".character-hero",
        &[
            ("display", "grid"),
            (
                "grid-template-columns",
                "minmax(0, 1fr) minmax(240px, .85fr)",
            ),
            ("align-items", "center"),
            ("gap", "32px"),
            ("padding-block", "12px 32px"),
        ],
    ),
    (".character-hero-copy", &[("min-inline-size", "0")]),
    (
        ".character-eyebrow",
        &[
            ("color", "var(--accent)"),
            ("font-size", ".75rem"),
            ("font-weight", "650"),
            ("letter-spacing", ".09em"),
            ("text-transform", "uppercase"),
            ("margin-block-end", "12px"),
        ],
    ),
    (
        ".character-hero h1",
        &[
            ("font-size", "clamp(1.75rem, 4vw, 3rem)"),
            ("line-height", "1.08"),
            ("letter-spacing", "-.035em"),
            ("margin", "0"),
            ("max-inline-size", "15ch"),
        ],
    ),
    (
        ".character-lede",
        &[
            ("color", "var(--muted)"),
            ("font-size", "1rem"),
            ("line-height", "1.65"),
            ("max-inline-size", "45ch"),
            ("margin-block-start", "16px"),
        ],
    ),
    (
        ".character-hero-art",
        &[
            ("display", "grid"),
            ("justify-items", "center"),
            ("gap", "12px"),
            ("padding", "32px 24px 20px"),
            ("border-radius", "20px"),
            (
                "background",
                "radial-gradient(ellipse at 50% 36%, #223c50, #121721 72%)",
            ),
            ("min-inline-size", "0"),
        ],
    ),
    (
        ".character-hero-art a",
        &[("display", "block"), ("border-radius", "12px")],
    ),
    (
        ".character-hero-art img",
        &[
            ("inline-size", "clamp(176px, 23vw, 288px)"),
            ("block-size", "auto"),
            ("filter", "drop-shadow(0 14px 14px rgba(0, 0, 0, .3))"),
        ],
    ),
    (
        ".character-hero-art figcaption",
        &[("font-size", ".75rem"), ("color", "var(--muted)")],
    ),
    (
        ".character-actions",
        &[
            ("display", "flex"),
            ("flex-wrap", "wrap"),
            ("gap", "10px"),
            ("margin-block-start", "20px"),
        ],
    ),
    (
        ".character-actions a",
        &[
            ("display", "inline-flex"),
            ("align-items", "center"),
            ("justify-content", "center"),
            ("min-block-size", "44px"),
            ("padding", "10px 14px"),
            ("border", "1px solid var(--border)"),
            ("border-radius", "8px"),
            ("font-size", ".875rem"),
            ("font-weight", "600"),
            ("text-decoration", "none"),
            (
                "transition",
                "background 150ms ease, border-color 150ms ease",
            ),
        ],
    ),
    (
        ".character-actions a:hover",
        &[("background", "var(--raised)"), ("border-color", "#53627b")],
    ),
    (
        ".character-actions .character-primary",
        &[
            ("color", "#0a1527"),
            ("background", "#92baff"),
            ("border-color", "#92baff"),
        ],
    ),
    (
        ".character-actions .character-primary:hover",
        &[("background", "#b4d0ff"), ("border-color", "#b4d0ff")],
    ),
    (
        ".character-section",
        &[("margin-block", "24px 44px"), ("min-inline-size", "0")],
    ),
    (
        ".character-section > h2",
        &[
            ("font-size", "clamp(1.35rem, 3vw, 1.75rem)"),
            ("line-height", "1.2"),
        ],
    ),
    (
        ".character-section-intro",
        &[
            ("margin-block", "12px 20px"),
            ("color", "var(--muted)"),
            ("max-inline-size", "72ch"),
            ("line-height", "1.65"),
        ],
    ),
    (
        ".character-progression",
        &[
            ("display", "grid"),
            ("grid-template-columns", "repeat(3, minmax(0, 1fr))"),
            ("gap", "20px"),
            ("list-style", "none"),
            ("padding", "0"),
            ("margin", "0"),
        ],
    ),
    (".character-stage", &[("min-inline-size", "0")]),
    (
        ".character-stage-art",
        &[
            ("display", "grid"),
            ("place-items", "center"),
            ("block-size", "224px"),
            ("border-radius", "12px"),
            ("background", "var(--surface)"),
            ("margin-block-end", "14px"),
        ],
    ),
    (
        ".character-stage-art img",
        &[
            ("block-size", "176px"),
            ("inline-size", "128px"),
            ("object-fit", "contain"),
        ],
    ),
    (".character-pixel", &[("image-rendering", "pixelated")]),
    (
        ".character-stage h3, .character-views h3",
        &[("margin", "0 0 8px"), ("font-size", "1rem")],
    ),
    (
        ".character-stage p, .character-views p",
        &[
            ("font-size", ".875rem"),
            ("color", "var(--muted)"),
            ("line-height", "1.6"),
        ],
    ),
    (
        ".character-views",
        &[
            ("display", "grid"),
            ("grid-template-columns", "repeat(3, minmax(0, 1fr))"),
            ("gap", "20px"),
        ],
    ),
    (".character-views figure", &[("min-inline-size", "0")]),
    (
        ".character-view-art",
        &[
            ("display", "grid"),
            ("place-items", "center"),
            ("block-size", "208px"),
            ("background", "var(--surface)"),
            ("border-radius", "12px"),
            ("margin-block-end", "14px"),
        ],
    ),
    (
        ".character-view-art img",
        &[("inline-size", "128px"), ("block-size", "176px")],
    ),
    (
        ".character-category + .character-category",
        &[("margin-block-start", "28px")],
    ),
    (
        ".character-category > h3",
        &[
            ("font-size", ".75rem"),
            ("font-weight", "650"),
            ("letter-spacing", ".08em"),
            ("text-transform", "uppercase"),
            ("color", "var(--muted)"),
            ("margin", "0 0 10px"),
        ],
    ),
    (
        ".character-clips",
        &[
            ("display", "grid"),
            (
                "grid-template-columns",
                "repeat(auto-fit, minmax(min(100%, 29rem), 1fr))",
            ),
            ("gap", "0 24px"),
        ],
    ),
    (
        ".character-clip",
        &[
            ("border-block-end", "1px solid var(--border)"),
            ("min-inline-size", "0"),
        ],
    ),
    (
        ".character-clip > summary",
        &[
            ("display", "flex"),
            ("align-items", "center"),
            ("justify-content", "space-between"),
            ("gap", "12px"),
            ("list-style", "none"),
            ("padding", "14px 8px"),
            ("min-block-size", "64px"),
            ("cursor", "pointer"),
            ("border-radius", "8px"),
            ("transition", "background 150ms ease"),
        ],
    ),
    (
        ".character-clip > summary::-webkit-details-marker",
        &[("display", "none")],
    ),
    (
        ".character-clip > summary::after",
        &[
            ("content", "'+'"),
            ("color", "var(--accent)"),
            ("font-size", "1.25rem"),
            ("flex", "0 0 auto"),
        ],
    ),
    (
        ".character-clip[open] > summary::after",
        &[("content", "'−'")],
    ),
    (
        ".character-clip > summary:hover",
        &[("background", "var(--raised)")],
    ),
    (
        ".character-clip summary strong",
        &[("display", "block"), ("font-size", ".9375rem")],
    ),
    (
        ".character-clip summary small",
        &[
            ("display", "block"),
            ("margin-block-start", "4px"),
            ("font-size", ".75rem"),
            ("color", "var(--muted)"),
        ],
    ),
    (
        ".character-clip-content",
        &[
            ("display", "grid"),
            ("grid-template-columns", "128px minmax(0, 1fr)"),
            ("gap", "16px"),
            ("padding", "8px 0 16px"),
            ("min-inline-size", "0"),
        ],
    ),
    (
        ".character-motion-frame",
        &[
            ("display", "grid"),
            ("place-items", "center"),
            ("inline-size", "128px"),
            ("block-size", "176px"),
            ("background", "#121721"),
            ("border-radius", "8px"),
            ("overflow", "hidden"),
        ],
    ),
    (
        ".character-motion",
        &[
            ("display", "block"),
            ("inline-size", "128px"),
            ("block-size", "176px"),
        ],
    ),
    (
        ".character-motion-still",
        &[
            ("display", "none"),
            ("inline-size", "128px"),
            ("block-size", "176px"),
            ("object-fit", "none"),
            ("object-position", "left top"),
        ],
    ),
    (".character-frame-reference", &[("min-inline-size", "0")]),
    (
        ".character-frame-reference > p",
        &[
            ("font-size", ".75rem"),
            ("color", "var(--muted)"),
            ("margin-block-end", "8px"),
        ],
    ),
    (
        ".character-strip",
        &[
            ("overflow-x", "auto"),
            ("max-inline-size", "100%"),
            ("min-inline-size", "0"),
            ("padding", "8px"),
            ("background", "var(--surface)"),
            ("border-radius", "8px"),
        ],
    ),
    (
        ".character-strip img",
        &[
            ("max-inline-size", "none"),
            ("inline-size", "auto"),
            ("block-size", "176px"),
        ],
    ),
    (
        ".character-frame-link",
        &[
            ("display", "inline-flex"),
            ("align-items", "center"),
            ("min-block-size", "44px"),
            ("padding-inline", "8px"),
            ("font-size", ".8125rem"),
            ("text-underline-offset", "3px"),
        ],
    ),
    (
        ".character-footnote",
        &[
            ("margin-block-start", "16px"),
            ("color", "var(--muted)"),
            ("font-size", ".8125rem"),
            ("line-height", "1.6"),
        ],
    ),
    (
        ".character-contract",
        &[
            ("display", "grid"),
            ("grid-template-columns", "repeat(4, minmax(0, 1fr))"),
            ("gap", "16px"),
            ("margin-block", "20px"),
        ],
    ),
    (
        ".character-contract > div",
        &[
            ("padding-block", "12px"),
            ("border-block", "1px solid var(--border)"),
        ],
    ),
    (
        ".character-contract dt",
        &[
            ("font-size", ".75rem"),
            ("font-weight", "500"),
            ("color", "var(--muted)"),
        ],
    ),
    (
        ".character-contract dd",
        &[
            ("margin-block-start", "6px"),
            ("font-size", "1.125rem"),
            ("font-weight", "650"),
            ("color", "var(--text)"),
        ],
    ),
    (
        ".character-workflow",
        &[
            ("padding-inline-start", "24px"),
            ("margin-block", "20px"),
            ("max-inline-size", "72ch"),
        ],
    ),
    (".character-workflow li", &[("padding-block", "8px")]),
    (
        ".character-workflow li::marker",
        &[("color", "var(--accent)"), ("font-weight", "650")],
    ),
    (
        ".character-workflow a",
        &[("font-weight", "650"), ("text-underline-offset", "3px")],
    ),
    (
        ".character-workflow p",
        &[
            ("font-size", ".875rem"),
            ("line-height", "1.6"),
            ("color", "var(--muted)"),
            ("margin-block-start", "6px"),
        ],
    ),
    (
        ".character-source-links",
        &[
            ("display", "flex"),
            ("flex-wrap", "wrap"),
            ("gap", "12px 24px"),
        ],
    ),
    (
        ".character-source-links a",
        &[
            ("display", "inline-flex"),
            ("align-items", "center"),
            ("min-block-size", "44px"),
            ("font-size", ".875rem"),
            ("text-underline-offset", "3px"),
        ],
    ),
    (
        ".character-hero a:focus-visible, .character-section :focus-visible",
        &[
            ("outline", "2px solid var(--accent)"),
            ("outline-offset", "3px"),
        ],
    ),
];

const PHONE_STYLES: markup::Styles = &[
    (
        ".character-hero",
        &[("grid-template-columns", "minmax(0, 1fr)"), ("gap", "20px")],
    ),
    (
        ".character-hero-art",
        &[
            ("order", "-1"),
            ("padding", "16px 16px 12px"),
            ("gap", "8px"),
        ],
    ),
    (".character-hero-art img", &[("inline-size", "176px")]),
    (".character-hero h1", &[("max-inline-size", "20ch")]),
    (
        ".character-progression, .character-views",
        &[("grid-template-columns", "minmax(0, 1fr)"), ("gap", "24px")],
    ),
    (
        ".character-stage",
        &[
            ("display", "grid"),
            ("grid-template-columns", "112px minmax(0, 1fr)"),
            ("align-items", "center"),
            ("gap", "16px"),
        ],
    ),
    (
        ".character-stage-art",
        &[("block-size", "176px"), ("margin", "0")],
    ),
    (
        ".character-stage-art img",
        &[("inline-size", "96px"), ("block-size", "132px")],
    ),
    (
        ".character-views figure",
        &[
            ("display", "grid"),
            ("grid-template-columns", "112px minmax(0, 1fr)"),
            ("align-items", "center"),
            ("gap", "16px"),
        ],
    ),
    (
        ".character-view-art",
        &[("block-size", "176px"), ("margin", "0")],
    ),
    (
        ".character-view-art img",
        &[("inline-size", "96px"), ("block-size", "132px")],
    ),
    (
        ".character-clip-content",
        &[("grid-template-columns", "minmax(0, 1fr)")],
    ),
    (".character-motion-frame", &[("margin-inline", "auto")]),
    (
        ".character-contract",
        &[("grid-template-columns", "repeat(2, minmax(0, 1fr))")],
    ),
];

const REDUCED_MOTION_STYLES: markup::Styles = &[
    (".character-motion", &[("display", "none")]),
    (".character-motion-still", &[("display", "block")]),
    (
        ".character-actions a, .character-clip > summary",
        &[("transition", "none")],
    ),
];

pub(super) fn stylesheet() -> String {
    let mut css = markup::stylesheet(STYLES);
    css.push_str(&markup::media_stylesheet(
        "(max-width: 639px)",
        PHONE_STYLES,
    ));
    css.push_str(&markup::media_stylesheet(
        "(prefers-reduced-motion: reduce)",
        REDUCED_MOTION_STYLES,
    ));
    css
}
