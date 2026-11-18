use super::{text, Attribute, Node, Tag};

pub(super) const BODY: &[Node] = &[
    element!(Div [Class = "intro"];
        element!(H1; text("Asset Pipeline for the Forge Game Runtime")),
        element!(P;
            element!(Strong; text("Implemented repository slice.")),
            text(" Reviewed 18 November 2026. The offline cooker, digest manifest, generated I/O worker, Rust validation, level activation and replay archive paths are implemented."),
        ),
        element!(P; text("The web playtest build now includes cooked Room One and Traversal Yard levels. It can switch between them after the requested level is ready.")),
        element!(Nav [Class = "section-nav", AriaLabel = "Asset pipeline design sections"];
            element!(A [Href = "#cooking"]; text("Cook and verify")),
            element!(A [Href = "#level-data"]; text("Level data and ownership")),
            element!(A [Href = "#loading"]; text("Load and activate")),
            element!(A [Href = "#acceptance"]; text("Rollout and acceptance")),
        ),
    ),
    element!(Section [Id = "cooking", Class = "design-section", AriaLabelledby = "cooking-title"];
        element!(H2 [Id = "cooking-title"]; text("Cook a reproducible build")),
        element!(P; text("src/python/game-assets/cook.py validates both authored levels and the Scout recipe/skin, exports canonical level JSON and a 4x atlas, and builds the version-1 manifest. The manifest records stable IDs, relative paths, SHA-256, required dependencies and optional dependencies. cook-state.json records authoring fingerprints for incremental reuse.")),
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
        element!(P; text("The cooker tests compare clean and incremental bytes under the same rendering toolchain. Publication verifies the complete generation and atomically changes its relative output symlink. make ui-build copies one resolved generation into the browser bundle. Test contracts are evidence, not a claim of an unobserved run.")),
    ),
    element!(Section [Id = "level-data", Class = "design-section", AriaLabelledby = "level-data-title"];
        element!(H2 [Id = "level-data-title"]; text("Level data and ownership")),
        element!(P; text("The two supported chunk IDs are Room One and Traversal Yard. Each records its tile/platform mode, grid, stable spawns, surface, collider, atlas identity and idle/walk/jump/crouch timing. The cooker materializes validated defaults; Rust checks version, bounds and references again before constructing a Room.")),
        element!(Dl [Class = "storage"];
            element!(Div;
                element!(Dt; text("Identity")),
                element!(Dd; text("The manifest and loader use fixed supported IDs for both levels, the Scout atlas and its PNG. Replay segments also record the exact manifest digest. A general game-save system is not implemented.")),
            ),
            element!(Div;
                element!(Dt; text("Address")),
                element!(Dd; text("The native host resolves the active cooked directory once at startup and verifies its bytes. The generated worker reads fixed same-origin routes; Rust-owned bounded upload buffers hold bytes for digest and format validation.")),
            ),
            element!(Div;
                element!(Dt; text("Readiness")),
                element!(Dd; text("Assets verifies the manifest and dependency bindings. Loader retains the newest prepared Room and publishes it only at an activation boundary. Identity, verification and active-room readiness are separate facts.")),
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
        element!(P; text("Loader replaces the active Room and keeps bounded verified level copies for replay. Assets retains the verified atlas for the page-scoped session. This slice does not implement a general streaming arena or arbitrary asset registry.")),
    ),
    element!(Section [Id = "loading", Class = "design-section", AriaLabelledby = "loading-title"];
        element!(H2 [Id = "loading-title"]; text("Load and activate at a tick boundary")),
        element!(P; text("The native host serves a verified primary cooked generation and can fill missing approved paths from a configured alternate. A configured replay archive exposes historical manifests and level chunks by digest. The worker performs bounded I/O; Rust checks manifest bindings, SHA-256, request generation and chunk format before staging a candidate outside the fixed step.")),
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
        element!(P; text("Implemented checks cover reproducible cooking, host path/hash admission, stale request generations, required-asset rejection, optional-art fallback, level replacement and replay across both levels. A deployment must separately configure any alternate source or retained replay archive it intends to use.")),
        element!(P; text("Load the atlas using its manifest and logical pivot. Physics consumes validated tile geometry, body collider mappings, and surface IDs. The 4× atlas scale changes the rendered artwork while collision stays in logical units.")),
        element!(Nav [Class = "section-nav", AriaLabel = "Related game designs"];
            element!(A [Href = "https://github.com/roachmaster/homelab/blob/main/docs/architecture/proposals/forge-game-asset-pipeline.md"]; text("Full asset pipeline implementation reference")),
            element!(A [Href = "/design/game-studio"]; text("Game engine design")),
            element!(A [Href = "/design/game-physics"]; text("Game physics design")),
            element!(A [Href = "/design/forge-workers/playtest"]; text("Playtest worker flow")),
            element!(A [Href = "/playtest"]; text("Open Room One playtest")),
        ),
    ),
];
