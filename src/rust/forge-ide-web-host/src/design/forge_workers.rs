use super::{text, Attribute, Node, Tag};

pub(super) const OWNERSHIP: &[Node] = &[
    element!(Div [Class = "intro"];
        element!(H1; text("Forge worker and main-thread ownership")),
        element!(P;
            element!(Strong; text("Implemented repository slice.")),
            text(" Reviewed 18 November 2026. IDE state runs in Rust/WASM inside its worker; the playtest runs Rust/WASM on the page with a separate I/O worker. Studio has a legacy JavaScript client and an implemented native admission/backend path."),
        ),
        element!(Nav [Class = "section-nav", AriaLabel = "Worker flows"];
            element!(A [Href = "/design/forge-workers/playtest"]; text("Playtest worker flow")),
            element!(A [Href = "/design/forge-workers/studio"]; text("Studio build worker flow")),
        ),
    ),
    element!(Section [Class = "design-section", AriaLabelledby = "ownership-title"];
        element!(H2 [Id = "ownership-title"]; text("Execution contexts and messages")),
        element!(Dl [Class = "storage"];
            element!(Div;
                element!(Dt; text("Main JavaScript")),
                element!(Dd; text("Capture input, own browser focus and lifecycle, and draw committed DOM or Canvas views. IDE intent goes to its Rust/WASM worker; playtest input calls its page-hosted Rust instance.")),
            ),
            element!(Div;
                element!(Dt; text("Rust main")),
                element!(Dd; text("Own the playtest fixed clock, simulation, level activation, contacts, animation and replay. IDE Rust separately owns application state, request preparation and stale-result acceptance. Rust authority is a role, not a claim that every page uses the same OS thread.")),
            ),
            element!(Div;
                element!(Dt; text("JavaScript I/O workers")),
                element!(Dd; text("The playtest I/O worker fetches bounded WASM/asset bytes and returns correlated buffers or failures. The IDE worker hosts both Rust/WASM and generated transport glue. Workers exchange messages; they do not access a shared global registry.")),
            ),
            element!(Div;
                element!(Dt; text("Studio build service")),
                element!(Dd; text("The optional Rust host relay admits Studio commands. The implemented Python API and Store retain jobs/events in PostgreSQL; a separate worker packages an approved static-web Git subtree under a lease. Availability requires configured services and host integration.")),
            ),
        ),
        element!(Div [Class = "gallery contract-sequences"];
            element!(Figure [Class = "wireframe"];
                element!(Figcaption; element!(H3; text("Worker ownership and message boundaries"))),
                element!(A [Class = "drawing", Href = "/design/forge-workers/ownership.svg"];
                    element!(Img [Src = "/design/forge-workers/ownership.svg", Width = "590", Height = "1598", Alt = "IDE Rust runs inside a browser worker, playtest Rust runs on the page with an I/O worker, and Studio currently uses a legacy JavaScript client plus Rust host admission and Python durable services."]),
                    element!(Span [Class = "open-label"]; text("Open full-size ownership diagram")),
                ),
            ),
        ),
    ),
    element!(Section [Class = "design-section", AriaLabelledby = "providers-title"];
        element!(H2 [Id = "providers-title"]; text("Factory and provider scope")),
        element!(P; text("The native web-host assembly stages effect-free registrars, freezes configuration, then creates and injects its asset source, agent tasks and optional Chat/Studio relays. Pure Room, Loader and replay values use ordinary Rust construction; they do not need empty registrars. Generated browser adapters own their page-scoped worker and teardown.")),
        element!(P; text("The current Studio page is an explicit authoring-boundary exception: its MAIN/WORKER JavaScript strings still own client orchestration, polling and presentation state. Moving Studio intent into a typed Rust/WASM model remains future work. Do not describe a Browser Rust controller as implemented for Studio.")),
        element!(Nav [Class = "section-nav", AriaLabel = "Related design references"];
            element!(A [Href = "/design/game-studio"]; text("Game engine design")),
            element!(A [Href = "https://github.com/roachmaster/homelab/blob/main/docs/architecture/proposals/forge-worker-ownership.md"]; text("Full worker implementation reference")),
        ),
    ),
];

pub(super) const PLAYTEST: &[Node] = &[
    element!(Div [Class = "intro"];
        element!(H1; text("Playtest worker flow")),
        element!(P;
            element!(Strong; text("Implemented repository slice.")),
            text(" The generated asset worker loads bounded bytes; Rust validates and stages the newest requested level, activates it at a fixed boundary, and records the transition for replay."),
        ),
    ),
    element!(Section [Class = "design-section", AriaLabelledby = "playtest-flow-title"];
        element!(H2 [Id = "playtest-flow-title"]; text("From level selection to a committed frame")),
        element!(Ol [Class = "contract-steps"];
            element!(Li; text("Main JavaScript forwards level selection and input edges directly to the page-hosted Rust/WASM exports, and supplies elapsed frame microseconds. There is no separate simulation worker or ActionBatch protocol in the current playtest.")),
            element!(Li; text("Rust requests cooked assets. A JavaScript worker fetches bounded response bytes and returns them or a typed I/O failure with the original request identity and generation.")),
            element!(Li; text("Rust matches the result to pending work, verifies the expected digest, validates the asset, and stages the newest valid candidate before activation. The active room remains selected during preparation or a required-asset failure.")),
            element!(Li; text("At the activation tick, Rust seals the outgoing replay segment, records the transition and starts the new room at tick zero. It publishes a snapshot and pose for the same room and tick. Main JavaScript draws that committed view.")),
        ),
        element!(Div [Class = "gallery contract-sequences"];
            element!(Figure [Class = "wireframe"];
                element!(Figcaption; element!(H3; text("Asset handoff and replay sequence"))),
                element!(A [Class = "drawing", Href = "/design/forge-workers/playtest.svg"];
                    element!(Img [Src = "/design/forge-workers/playtest.svg", Width = "956", Height = "1308", Alt = "The JavaScript I/O worker returns bytes to Rust. Rust verifies and prepares the asset, journals the transition, activates at a fixed tick and publishes a complete view to main JavaScript."]),
                    element!(Span [Class = "open-label"]; text("Open full-size playtest diagram")),
                ),
            ),
        ),
        element!(P; text("Export replay records effective inputs across room changes and restarts, with manifest identity and boundary ticks. Import validates a complete bounded session and obtains missing historical manifests/levels through the configured digest archive. The Rust replay view is separate from the live playtest; stale archive generations cannot replace it.")),
        element!(Nav [Class = "section-nav", AriaLabel = "Related playtest designs"];
            element!(A [Href = "/design/game-asset-pipeline"]; text("Game asset pipeline design")),
            element!(A [Href = "/design/forge-workers/studio"]; text("Studio build worker flow")),
        ),
    ),
];

pub(super) const STUDIO: &[Node] = &[
    element!(Div [Class = "intro"];
        element!(H1; text("Studio build worker flow")),
        element!(P;
            element!(Strong; text("Implemented repository slice.")),
            text(" The durable Studio build service is implemented for the static-web profile. Its JavaScript client, Rust host admission, Python API, PostgreSQL Store and packaging worker have separate responsibilities. Running it requires explicit service and preview-origin configuration."),
        ),
    ),
    element!(Section [Class = "design-section", AriaLabelledby = "studio-flow-title"];
        element!(H2 [Id = "studio-flow-title"]; text("From build action to artifact")),
        element!(Ol [Class = "contract-steps"];
            element!(Li;
                text("The current Studio JavaScript client forwards the selected project, revision and profile to its I/O worker. The worker sends "),
                element!(Code; text("game.build.submit")),
                text(" to the shared Forge gateway at "),
                element!(Code; text("/forge/v1/dispatch")),
                text(" and relays the response to Rust."),
            ),
            element!(Li; text("The Rust host checks session, CSRF, destination/body correlation and project grants before its optional Studio relay. The Python API validates the configured project, exact revision and static-web profile; Store atomically commits the pinned job and first event. A matching retry gets the existing build ID.")),
            element!(Li; text("The Python Studio worker claims a 30-second lease and unique attempt token, reads the approved Git subtree without executing project scripts, and publishes a bounded digest-addressed artifact. Expired claims retry up to three attempts; cancellation invalidates the attempt. Only a current lease may attach the result.")),
            element!(Li; text("After reconnect, the Studio client restores its tab-local project/revision/build ID and polls through the admitted host route with an event cursor. The current JavaScript client renders status. Artifact access uses a signed URL on the separately configured preview origin; a typed Rust Studio client is still future work.")),
        ),
        element!(Div [Class = "gallery contract-sequences"];
            element!(Figure [Class = "wireframe"];
                element!(Figcaption; element!(H3; text("Durable build and lease sequence"))),
                element!(A [Class = "drawing", Href = "/design/forge-workers/studio.svg"];
                    element!(Img [Src = "/design/forge-workers/studio.svg", Width = "1263", Height = "2134", Alt = "Studio JavaScript requests a build through its I/O worker. Rust host admission precedes the Python Store transaction. The packaging worker can attach a verified artifact only during its current lease attempt."]),
                    element!(Span [Class = "open-label"]; text("Open full-size Studio diagram")),
                ),
            ),
        ),
        element!(Nav [Class = "section-nav", AriaLabel = "Related Studio designs"];
            element!(A [Href = "/design/game-studio"]; text("Game engine design")),
            element!(A [Href = "/design/forge-workers/playtest"]; text("Playtest worker flow")),
        ),
    ),
];
