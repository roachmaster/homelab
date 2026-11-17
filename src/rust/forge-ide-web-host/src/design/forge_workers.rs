use super::{text, Attribute, Node, Tag};

pub(super) const OWNERSHIP: &[Node] = &[
    element!(Div [Class = "intro"];
        element!(H1; text("Forge worker and main-thread ownership")),
        element!(P;
            element!(Strong; text("Design proposal.")),
            text(" Main JavaScript renders and forwards actions. Rust main owns application decisions. JavaScript workers handle I/O, run support work and deliver typed messages."),
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
                element!(Dd; text("Capture browser input and forward ordered actions and frame timing. Draw the latest complete view in Canvas or the DOM.")),
            ),
            element!(Div;
                element!(Dt; text("Rust main")),
                element!(Dd; text("Own fixed ticks, simulation state, level activation and replay. Request work from providers, validate completed results and publish immutable views.")),
            ),
            element!(Div;
                element!(Dt; text("JavaScript and Rust support workers")),
                element!(Dd; text("JavaScript workers carry out directed I/O, perform bounded computation or delegate it to a configured Rust support backend, and return correlated results. They can relay committed views to main JavaScript.")),
            ),
            element!(Div;
                element!(Dt; text("Forge host and native Studio worker")),
                element!(Dd; text("The host admits typed Studio commands. PostgreSQL retains jobs and events. The Studio worker claims and builds a pinned revision under a lease.")),
            ),
        ),
        element!(Div [Class = "gallery contract-sequences"];
            element!(Figure [Class = "wireframe"];
                element!(Figcaption; element!(H3; text("Worker ownership and message boundaries"))),
                element!(A [Class = "drawing", Href = "/design/forge-workers/ownership.svg"];
                    element!(Img [Src = "/design/forge-workers/ownership.svg", Width = "710", Height = "1672", Alt = "Main JavaScript sends actions and frame time to Rust. Rust requests support work, validates results and publishes committed views. Studio jobs cross a separate host and database boundary."]),
                    element!(Span [Class = "open-label"]; text("Open full-size ownership diagram")),
                ),
            ),
        ),
    ),
    element!(Section [Class = "design-section", AriaLabelledby = "providers-title"];
        element!(H2 [Id = "providers-title"]; text("Factory and provider scope")),
        element!(P; text("Each execution context bootstraps and freezes its own typed factory registrations, then injects the providers it uses. Across those contexts, send IDs, request generations, owned buffers or receipts. The receiver handles the message using its own injected provider.")),
        element!(P; text("Rust main names the authority for simulation or application state. The current Room One page hosts its WASM directly. A dedicated worker can host the same Rust runtime with a thin JavaScript adapter.")),
        element!(Nav [Class = "section-nav", AriaLabel = "Related design references"];
            element!(A [Href = "/design/game-studio"]; text("Game engine design")),
            element!(A [Href = "https://github.com/roachmaster/homelab/blob/main/docs/architecture/proposals/forge-worker-ownership.md"]; text("Full worker proposal")),
        ),
    ),
];

pub(super) const PLAYTEST: &[Node] = &[
    element!(Div [Class = "intro"];
        element!(H1; text("Playtest worker flow")),
        element!(P;
            element!(Strong; text("Design proposal.")),
            text(" Load a cooked level in the background, activate it at a Rust tick boundary and preserve the transition for replay."),
        ),
    ),
    element!(Section [Class = "design-section", AriaLabelledby = "playtest-flow-title"];
        element!(H2 [Id = "playtest-flow-title"]; text("From level selection to a committed frame")),
        element!(Ol [Class = "contract-steps"];
            element!(Li; text("Main JavaScript passes level selection, ordered input edges and frame timing to Rust main.")),
            element!(Li; text("Rust requests cooked assets with an expected digest and a request generation. A support worker reads, hashes or decodes the assets outside the tick, then returns a correlated result or error.")),
            element!(Li; text("Rust matches the result to pending work, checks the asset and readiness, and stages the newest valid candidate. The active room keeps running during preparation or a required-asset failure.")),
            element!(Li; text("At the activation tick, Rust seals the outgoing replay segment, records the transition and starts the new room at tick zero. It publishes a snapshot and pose for the same room and tick. Main JavaScript draws that committed view.")),
        ),
        element!(Div [Class = "gallery contract-sequences"];
            element!(Figure [Class = "wireframe"];
                element!(Figcaption; element!(H3; text("Asset handoff and replay sequence"))),
                element!(A [Class = "drawing", Href = "/design/forge-workers/playtest.svg"];
                    element!(Img [Src = "/design/forge-workers/playtest.svg", Width = "952", Height = "1404", Alt = "The support worker returns a correlated asset result to Rust. Rust validates it, journals the transition, activates at a fixed tick and publishes a complete view to main JavaScript."]),
                    element!(Span [Class = "open-label"]; text("Open full-size playtest diagram")),
                ),
            ),
        ),
        element!(P; text("Replay loads the recorded cooked generation and takes the transition at its recorded tick. Worker completion determines readiness. Session, request, room and view identities guard against stale replies.")),
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
            element!(Strong; text("Design proposal.")),
            text(" Submit a pinned build through Forge, run it under a durable lease and reconnect to see its status."),
        ),
    ),
    element!(Section [Class = "design-section", AriaLabelledby = "studio-flow-title"];
        element!(H2 [Id = "studio-flow-title"]; text("From build action to artifact")),
        element!(Ol [Class = "contract-steps"];
            element!(Li;
                text("Main JavaScript forwards the build action. Browser Rust emits a typed effect. The I/O worker sends "),
                element!(Code; text("game.build.submit")),
                text(" to the shared Forge gateway at "),
                element!(Code; text("/forge/v1/dispatch")),
                text("."),
            ),
            element!(Li; text("The Forge host authorizes the project and profile. PostgreSQL atomically commits the pinned job and first event. A matching retry gets the existing build ID.")),
            element!(Li; text("The native Studio worker claims the job with a lease and attempt token. It builds the pinned revision and verifies the artifact. Only the current attempt can attach the immutable artifact.")),
            element!(Li; text("After reconnect, the browser requests status by build ID and event cursor. Browser Rust projects the authorized result for main JavaScript to render. The preview opens on its separate origin.")),
        ),
        element!(Div [Class = "gallery contract-sequences"];
            element!(Figure [Class = "wireframe"];
                element!(Figcaption; element!(H3; text("Durable build and lease sequence"))),
                element!(A [Class = "drawing", Href = "/design/forge-workers/studio.svg"];
                    element!(Img [Src = "/design/forge-workers/studio.svg", Width = "1268", Height = "2138", Alt = "Browser Rust requests an admitted build through a JavaScript I/O worker. A native worker claims the committed job and can publish an artifact only during the current lease attempt."]),
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
