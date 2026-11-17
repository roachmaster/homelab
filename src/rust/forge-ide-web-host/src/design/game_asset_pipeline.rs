use super::{text, Attribute, Node, Tag};

pub(super) const BODY: &[Node] = &[
    element!(Div [Class = "intro"];
        element!(H1; text("Asset Pipeline for the Forge Game Runtime")),
        element!(P;
            element!(Strong; text("Design proposal.")),
            text(" Cook Room One’s level, Scout atlas, and collision data into validated assets. Load them by identity and activate the level at a fixed tick boundary."),
        ),
        element!(Nav [Class = "section-nav", AriaLabel = "Asset pipeline design sections"];
            element!(A [Href = "#cooking"]; text("Cook and verify")),
            element!(A [Href = "#level-data"]; text("Level data and ownership")),
            element!(A [Href = "#loading"]; text("Load and activate")),
            element!(A [Href = "#acceptance"]; text("Rollout and acceptance")),
        ),
    ),
    element!(Section [Id = "cooking", Class = "design-section", AriaLabelledby = "cooking-title"];
        element!(H2 [Id = "cooking-title"]; text("Cook a reproducible build")),
        element!(P; text("The cook validates authored levels and sprite recipes, resolves their dependency IDs, and runs the exporters with recorded conversion settings. The resulting manifest indexes cooked bytes by ID and records their hashes and dependency graph.")),
        element!(Div [Class = "gallery contract-sequences"];
            element!(Figure [Class = "wireframe"];
                element!(Figcaption;
                    element!(H3; text("Cook and publish sequence")),
                    element!(P; text("Condition source files, verify the cooked outputs, and publish the build manifest.")),
                ),
                element!(A [Class = "drawing", Href = "/design/game-asset-pipeline/cook.svg"];
                    element!(Img [Src = "/design/game-asset-pipeline/cook.svg", Width = "859", Height = "1195", Alt = "The cook validates source assets and dependencies, exports cooked content, then publishes a verified manifest."]),
                    element!(Span [Class = "open-label"]; text("Open full-size cook diagram")),
                ),
            ),
        ),
        element!(P; text("CI compares clean and incremental output bytes and runs the sprite tool tests. A validation error or digest mismatch stops publication.")),
    ),
    element!(Section [Id = "level-data", Class = "design-section", AriaLabelledby = "level-data-title"];
        element!(H2 [Id = "level-data-title"]; text("Level data and ownership")),
        element!(P; text("The first cooked chunk describes the Room One grid, stable spawn descriptors, win target, and referenced asset IDs. The cook records the schema version and materializes defaults so older cooked levels retain their behavior.")),
        element!(Dl [Class = "storage"];
            element!(Div;
                element!(Dt; text("Identity")),
                element!(Dd; text("Chunks, saves, traces, and the registry use a stable asset ID.")),
            ),
            element!(Div;
                element!(Dt; text("Address")),
                element!(Dd; text("The manifest locates bytes in the active cooked build. A loaded buffer owns the memory referenced by the runtime object.")),
            ),
            element!(Div;
                element!(Dt; text("Readiness")),
                element!(Dd; text("A resolved asset becomes available to the simulation when initialization finishes and its generation is published at a tick boundary.")),
            ),
        ),
        element!(Div [Class = "gallery contract-sequences"];
            element!(Figure [Class = "wireframe"];
                element!(Figcaption; element!(H3; text("Asset identity, readiness, and lifetime"))),
                element!(A [Class = "drawing", Href = "/design/game-asset-pipeline/contract.svg"];
                    element!(Img [Src = "/design/game-asset-pipeline/contract.svg", Width = "670", Height = "522", Loading = "lazy", Alt = "A manifest locates an asset by ID, while the registry tracks its readiness and buffer lifetime."]),
                    element!(Span [Class = "open-label"]; text("Open full-size asset contract diagram")),
                ),
            ),
        ),
        element!(P; text("Level-owned assets are released together on unload. Shared resident assets stay for the session. Runtime handles check the owning generation before use.")),
    ),
    element!(Section [Id = "loading", Class = "design-section", AriaLabelledby = "loading-title"];
        element!(H2 [Id = "loading-title"]; text("Load and activate at a tick boundary")),
        element!(P; text("Resolve IDs through a recorded search order. Use the active cooked output directory first and an optional configured alternate location second. Verify fetched bytes against the active manifest digest. Changed development assets require a separate manifest. Asynchronous work initializes the candidate outside the simulation step.")),
        element!(Div [Class = "gallery contract-sequences"];
            element!(Figure [Class = "wireframe"];
                element!(Figcaption;
                    element!(H3; text("Runtime load and activation sequence")),
                    element!(P; text("Follow a room request through byte ownership, dependency validation, and tick-boundary activation.")),
                ),
                element!(A [Class = "drawing", Href = "/design/game-asset-pipeline/load.svg"];
                    element!(Img [Src = "/design/game-asset-pipeline/load.svg", Width = "1017", Height = "1420", Loading = "lazy", Alt = "The loader validates cooked assets outside the tick, tracks pending buffers, and publishes a ready generation at a fixed tick boundary."]),
                    element!(Span [Class = "open-label"]; text("Open full-size load diagram")),
                ),
            ),
        ),
        element!(Ol [Class = "contract-steps"];
            element!(Li; text("An asynchronous read owns its buffer until completion. A result for an unloaded generation is dropped.")),
            element!(Li; text("Validate the level and its required collision dependencies before activation. Keep the active level if a required asset fails. An unavailable cosmetic atlas uses the primitive renderer.")),
            element!(Li; text("Activate the ready generation at a tick boundary. Record the cooked manifest identity and activation tick for replay.")),
        ),
    ),
    element!(Section [Id = "acceptance", Class = "design-section", AriaLabelledby = "acceptance-title"];
        element!(H2 [Id = "acceptance-title"]; text("Rollout and acceptance")),
        element!(P; text("Start with cooked loose files, one active level, and one request in flight. Check reproducible cooking, required dependency failures, late completion after unload, and replay across a level activation.")),
        element!(P; text("Load the atlas using its manifest and logical pivot. Physics consumes validated tile geometry, body collider mappings, and surface IDs. The 4× atlas scale changes the rendered artwork while collision stays in logical units.")),
        element!(Nav [Class = "section-nav", AriaLabel = "Related game designs"];
            element!(A [Href = "https://github.com/roachmaster/homelab/blob/main/docs/architecture/proposals/forge-game-asset-pipeline.md"]; text("Full asset pipeline proposal")),
            element!(A [Href = "/design/game-studio"]; text("Game engine design")),
            element!(A [Href = "/design/game-physics"]; text("Game physics design")),
            element!(A [Href = "/playtest"]; text("Open Room One playtest")),
        ),
    ),
];
