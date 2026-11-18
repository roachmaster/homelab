use super::{text, Attribute, Node, Tag};

pub(super) const BODY: &[Node] = &[
    element!(Div [Class = "intro"];
        element!(H1; text("Forge plug-and-play")),
        element!(P;
            text("Use "), element!(Code; text("$wire-forge-providers")),
            text(" when adding or replacing a Forge capability, adapter, worker, or runtime dependency. The host selects an implementation of the consumer’s typed port."),
        ),
        element!(P [Class = "contract-note"];
            text("Implementation review · 18 November 2026. Typed registrars, singleton/transient providers, configured native hosts, durable jobs, and the generated portal are implemented. Enabled features still depend on each host’s configuration."),
        ),
        element!(Nav [Class = "section-nav", AriaLabel = "On this page"];
            element!(A [Href = "#wiring-sequence"]; text("Wiring sequence")),
            element!(A [Href = "#factory-provider-roles"]; text("Factory and provider roles")),
            element!(A [Href = "#runtime-examples"]; text("Working implementations")),
            element!(A [Href = "#verification"]; text("Verify a change")),
        ),
    ),
    element!(Section [Class = "design-section", AriaLabelledby = "wiring-sequence"];
        element!(H2 [Id = "wiring-sequence"]; text("Wiring sequence")),
        element!(Ol [Class = "contract-steps"];
            element!(Li;
                element!(Strong; text("Choose the port and implementation.")),
                text(" Start from the normal executable, host factory, or browser bootstrap. Use typed configuration to select the adapter and its scope."),
            ),
            element!(Li;
                element!(Strong; text("Register and freeze.")),
                text(" Stage the selected factory recipes and validate duplicate or required bindings. Freeze the startup plan before opening files, binding sockets, or starting workers."),
            ),
            element!(Li;
                element!(Strong; text("Create and inject.")),
                text(" Construct instances in dependency order. Inject a ready port into the consumer, or a scoped provider when it needs to acquire an instance during execution."),
            ),
            element!(Li;
                element!(Strong; text("Serve and shut down.")),
                text(" Admit work after readiness. During shutdown, stop admission and wait for users and workers before releasing their dependencies."),
            ),
        ),
    ),
    element!(Section [Class = "design-section", AriaLabelledby = "factory-provider-roles"];
        element!(H2 [Id = "factory-provider-roles"]; text("Factory and provider roles")),
        element!(Dl [Class = "storage"];
            element!(Div;
                element!(Dt; text("Factory")),
                element!(Dd; text("Constructs the selected implementation from explicit inputs. It cleans up partial acquisition until ownership transfers.")),
            ),
            element!(Div;
                element!(Dt; text("Provider")),
                element!(Dd; text("Controls acquisition and reuse. A singleton shares one instance within its owner. A transient provider creates an instance on acquisition.")),
            ),
            element!(Div;
                element!(Dt; text("Registrar")),
                element!(Dd; text("Stages a cohesive group of typed recipes. Bootstrap chooses the recipes and owns their construction order.")),
            ),
            element!(Div;
                element!(Dt; text("Consumer")),
                element!(Dd; text("Calls its injected port. A worker receives its owned dependencies or an explicit message port. The startup registry stays in bootstrap.")),
            ),
        ),
    ),
    element!(Section [Class = "design-section", AriaLabelledby = "runtime-examples"];
        element!(H2 [Id = "runtime-examples"]; text("Working implementations")),
        element!(Dl [Class = "storage"];
            element!(Div;
                element!(Dt; text("Agent managed writers")),
                element!(Dd; text("The Agent stages node, project, managed-writer and gateway recipes, then freezes before construction. Each explicitly writable private Unix project gets its own retained writer. Read grants do not grant mutation.")),
                element!(Dd;
                    element!(A [Href = "https://github.com/roachmaster/homelab/blob/main/src/rust/forge-agent/src/host.rs"]; text("Agent composition source")),
                ),
            ),
            element!(Div;
                element!(Dt; text("Portal resources")),
                element!(Dd; text("HostFactory validates configuration, creates the selected assets and optional Chat/Studio relays, and retains them in host-owned singletons. Remote client factory groups freeze before their clients are created; runtime requests use injected ports.")),
                element!(Dd;
                    element!(A [Href = "https://github.com/roachmaster/homelab/blob/main/src/rust/forge-ide-web-host/src/service.rs"]; text("Portal host factory source")),
                ),
            ),
            element!(Div;
                element!(Dt; text("Model runtime")),
                element!(Dd; text("The provider supplies a GPU reservation. A factory starts the selected model process. Model switches retain the reservation.")),
                element!(Dd;
                    element!(A [Href = "https://github.com/roachmaster/homelab/blob/main/src/rust/forge-llama/src/model_runtime.rs"]; text("Model runtime provider source")),
                ),
            ),
            element!(Div;
                element!(Dt; text("Durable jobs")),
                element!(Dd; text("The optional Unix jobs command stages its operator verifier, private file store and worker recipe. After freeze it creates the verifier/store, authenticates the account, and injects the acquired store into the admitted bridge or owned worker. Receipts and results survive reopening that store.")),
                element!(Dd;
                    element!(A [Href = "https://github.com/roachmaster/homelab/blob/main/src/rust/forge-terminal/src/bin/forge-jobs.rs"]; text("Durable jobs composition source")),
                ),
            ),
            element!(Div;
                element!(Dt; text("Playtest asset reads")),
                element!(Dd; text("Rust issues and verifies the asset requests. A page-scoped JavaScript worker executes I/O and returns the result with its request identity.")),
            ),
        ),
        element!(Nav [Class = "section-nav", AriaLabel = "Related worker designs"];
            element!(A [Href = "/design/forge-workers"]; text("Worker and thread ownership")),
            element!(A [Href = "/design/forge-workers/playtest"]; text("Playtest worker flow")),
            element!(A [Href = "/design/forge-workers/studio"]; text("Studio build worker flow")),
        ),
    ),
    element!(Section [Class = "design-section", AriaLabelledby = "verification"];
        element!(H2 [Id = "verification"]; text("Verify a change")),
        element!(P; text("Current bootstrap validates declared dependency availability and order; hosts execute the constructors explicitly. Automatic graph execution, generic provider pools, async cleanup, federation/delegation, and generic remote jobs remain separate proposals. Pure logic and DTO-only modules need no empty registrar.")),
        element!(Ol [Class = "contract-steps"];
            element!(Li; text("Trace the normal entry point through registration, freeze, construction, and acquisition. Check that rejecting startup leaves resources untouched.")),
            element!(Li; text("Select a second implementation of the same port. Check that the host uses the selected instance and leaves the unused factory untouched.")),
            element!(Li; text("Exercise the promised lifetime. Verify reuse or isolation, then check that workers release the resource on shutdown.")),
            element!(Li;
                text("Run a focused behavior test and use "),
                element!(Code; text("make check-deps")),
                text(" when package or feature dependencies change."),
            ),
        ),
        element!(Nav [Class = "section-nav", AriaLabel = "Provider reference material"];
            element!(A [Href = "https://github.com/roachmaster/homelab/blob/main/.agents/skills/wire-forge-providers/SKILL.md"]; text("Read the provider skill")),
            element!(A [Href = "https://github.com/roachmaster/homelab/blob/main/docs/architecture/bootstrap-registrars.md"]; text("Bootstrap and registrar APIs")),
            element!(A [Href = "https://github.com/roachmaster/homelab/blob/main/docs/architecture/dependency-injection.md"]; text("Provider lifetime reference")),
            element!(A [Href = "https://github.com/roachmaster/homelab/blob/main/docs/architecture/forge-framework-blueprint.md#current-implementation"]; text("Current implementation inventory")),
        ),
    ),
];
