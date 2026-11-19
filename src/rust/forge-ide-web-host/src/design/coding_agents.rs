use super::{text, Attribute, Node, Tag};

macro_rules! diagram {
    ($title:literal, $caption:literal, $src:literal, $width:literal, $height:literal, $alt:literal, $open_label:literal) => {
        element!(Div [Class = "gallery"];
            element!(Figure [Class = "wireframe"];
                element!(Figcaption;
                    element!(H3; text($title)),
                    element!(P; text($caption)),
                ),
                element!(Div [Class = "wireframe-stage"];
                    element!(A [Class = "drawing", Href = $src];
                        element!(Img [Src = $src, Width = $width, Height = $height, Loading = "lazy", Alt = $alt]),
                        element!(Span [Class = "open-label"];
                            text($open_label),
                            element!(Span [AriaHidden = "true"]; text(" →")),
                        ),
                    ),
                ),
            ),
        )
    };
}

pub(super) const DISPATCH: &[Node] = &[
    element!(Div [Class = "intro"];
        element!(H1; text("Coding-agent dispatch")),
        element!(P;
            element!(Strong; text("Proposed / not implemented.")),
            text(" Design-only architecture at base revision "),
            element!(Code; text("2e1300a00da92e9f97bdb1a656c2b182d4219a6c")),
            text(". An operator submits a coding assignment through the shared Forge gateway; PostgreSQL commits the task and wakes a separate native coding worker, which stores the full result for review acceptance. No coding worker, migration, or production activation is enabled here."),
        ),
        element!(Nav [Class = "section-nav", AriaLabel = "Coding-agent dispatch sections"];
            element!(A [Href = "#flow"]; text("End-to-end flow")),
            element!(A [Href = "#ownership"]; text("Ownership and providers")),
            element!(A [Href = "#lifecycle"]; text("Leases and takeover")),
            element!(A [Href = "#contracts"]; text("Proposed contracts")),
            element!(A [Href = "#rollout"]; text("Rollout")),
            element!(A [Href = "/design/agent-usage"]; text("Agent usage design (proposed)")),
        ),
    ),
    element!(Section [Id = "flow", Class = "design-section", AriaLabelledby = "flow-title"];
        element!(H2 [Id = "flow-title"]; text("From assignment to reviewed completion")),
        element!(Ol [Class = "contract-steps"];
            element!(Li;
                element!(Strong; text("Single admitted gateway.")),
                text(" An authenticated caller posts "),
                element!(Code; text("ide.work.submit")),
                text(" to "),
                element!(Code; text("POST /forge/v1/dispatch")),
                text(" with matching "),
                element!(Code; text("Forge-Destination")),
                text(", envelope/contract version "),
                element!(Code; text("1")),
                text(", and "),
                element!(Code; text("Forge-Request-Id")),
                text("."),
            ),
            element!(Li;
                element!(Strong; text("Independent admission before effects.")),
                text(" The native Rust host checks session, Host/Origin, CSRF, header/body agreement, and independent coding project, profile, and mode grants before relaying to the private runner "),
                element!(Code; text("POST /v1/dispatch")),
                text("."),
            ),
            element!(Li;
                element!(Strong; text("Transactional commit and wake-up.")),
                text(" Injected "),
                element!(Code; text("WorkService")),
                text(" revalidates grants, pins an immutable non-secret "),
                element!(Code; text("recipe_digest")),
                text(", inserts "),
                element!(Code; text("coding_tasks")),
                text(" plus sequence-1 "),
                element!(Code; text("queued")),
                text(" event and "),
                element!(Code; text("pg_notify('forge_coding_jobs', '')")),
                text(", and returns "),
                element!(Code; text("202 Accepted")),
                text(" only after "),
                element!(Code; text("COMMIT")),
                text("."),
            ),
            element!(Li;
                element!(Strong; text("Native coding worker claim and execution.")),
                text(" A separate local worker (after committed "),
                element!(Code; text("LISTEN")),
                text(" or timed rescan) claims one row with "),
                element!(Code; text("FOR UPDATE SKIP LOCKED")),
                text(", starts lease renewal immediately, locks the isolated approved worktree, revalidates lease and "),
                element!(Code; text("recipe_digest")),
                text(", and runs the real "),
                element!(Code; text("agy")),
                text(" binary with "),
                element!(Code; text("shell=False")),
                text(", fixed argv, and streaming JSON stdin."),
            ),
            element!(Li;
                element!(Strong; text("Full result persistence and review acceptance.")),
                text(" The worker stages full response bytes in fenced chunks and atomically publishes an immutable "),
                element!(Code; text("result_manifest")),
                text(" to enter "),
                element!(Code; text("awaiting_review")),
                text(". Review readiness requires a coherent CLI result, nonempty reply, and confirmed process-scope stop; exit zero alone is insufficient. Completion requires an explicit "),
                element!(Code; text("ide.work.accept")),
                text("."),
            ),
        ),
        element!(P [Class = "contract-note"];
            element!(Strong; text("Token and paging policy: ")),
            text("Coding recipes default to "),
            element!(Code; text("generation_token_limit: null")),
            text(" (provider-managed) and never inherit Chat, Mail, or "),
            element!(Code; text("llama.cpp")),
            text(" output-token caps. Full replies are stored in "),
            element!(Code; text("coding_result_chunks")),
            text(" and read in bounded pages via "),
            element!(Code; text("ide.work.result")),
            text("; preview bounds never truncate the stored reply. See "),
            element!(A [Href = "/design/agent-usage"]; text("Agent usage and token limits (proposed)")),
            text(" for allowance and accounting boundaries."),
        ),
        diagram!(
            "Normal assignment to reviewed completion",
            "Submit, commit, worker claim with parallel lease renewal, chunked result persistence, and explicit review acceptance.",
            "/design/coding-agents/completion.svg",
            "1296",
            "2527",
            "Sequence diagram from caller submitting ide.work.submit through Rust host admission, WorkService transactional commit, CodingWorker claim and lease renewal, agy CLI streaming execution, chunked result publication to awaiting_review, and explicit ide.work.accept to completed.",
            "Open full-size completion sequence diagram"
        ),
    ),
    element!(Section [Id = "ownership", Class = "design-section", AriaLabelledby = "ownership-title"];
        element!(H2 [Id = "ownership-title"]; text("Ownership, providers, and process isolation")),
        element!(Dl [Class = "storage"];
            element!(Div;
                element!(Dt; text("Rust Host and WorkRelayFactory")),
                element!(Dd;
                    text("Host bootstrap validates "),
                    element!(Code; text("Option<WorkConfig>")),
                    text(" (loopback endpoint, absolute token file, and independent "),
                    element!(Code; text("allowed_projects")),
                    text(", "),
                    element!(Code; text("allowed_profiles")),
                    text(", and "),
                    element!(Code; text("allowed_modes")),
                    text("), freezes startup recipes, and injects a host-owned "),
                    element!(Code; text("Singleton<WorkRelay>")),
                    text(". Read-only inspection grants never grant write authority."),
                ),
            ),
            element!(Div;
                element!(Dt; text("Runner WorkService and WorkStore DAO")),
                element!(Dd;
                    text("Mounted on the existing private runner "),
                    element!(Code; text("POST /v1/dispatch")),
                    text(" under its own startup config, independent of Mail or GPU flags. Every DAO read, list, event query, and mutation filters "),
                    element!(Code; text("owner_id = $configured_owner")),
                    text(", returning "),
                    element!(Code; text("404 not_found")),
                    text(" across owners."),
                ),
            ),
            element!(Div;
                element!(Dt; text("Separate signed-in CodingWorker")),
                element!(Dd;
                    text("Because the web runner runs as "),
                    element!(Code; text("forge_agent")),
                    text(" with "),
                    element!(Code; text("ProtectHome=true")),
                    text(", the coding worker runs as a separate native process under the approved signed-in OS account ("),
                    element!(Code; text("lrocha")),
                    text("). Its composition root injects "),
                    element!(Code; text("WorkerDbPort")),
                    text(", "),
                    element!(Code; text("CommandRunnerProvider")),
                    text(", and "),
                    element!(Code; text("WorkspaceProvider")),
                    text("; workers never query a global registry, OAuth stays out of the runner/DB, and DB or bearer secrets stay out of child CLI env. Pure DTO values need no empty registrars."),
                ),
            ),
            element!(Div;
                element!(Dt; text("Least-privilege DB role and trusted Reconciler")),
                element!(Dd;
                    text("The worker calls only "),
                    element!(Code; text("forge_coding_worker_api.*")),
                    text(" "),
                    element!(Code; text("SECURITY DEFINER")),
                    text(" routines. A separate trusted local "),
                    element!(Code; text("Reconciler")),
                    text(" holds "),
                    element!(Code; text("expire_leases")),
                    text(" and "),
                    element!(Code; text("record_reconciliation")),
                    text(" to verify process-scope stop and workspace digest without ability to submit or spawn replacements."),
                ),
            ),
        ),
        diagram!(
            "Ownership, factories, and DAO boundaries",
            "Explicit startup registration, host-scoped relay, runner DAO injection, and worker-scoped attempt factories without a global registry.",
            "/design/coding-agents/ownership.svg",
            "1724",
            "1547",
            "Class diagram showing HostFactory staging WorkRelayFactory from WorkConfig to build Singleton WorkRelay, relaying to WorkService with injected WorkStore DAO, and separate CodingWorker receiving injected WorkerDbPort, CommandRunnerProvider, and WorkspaceProvider.",
            "Open full-size ownership diagram"
        ),
    ),
    element!(Section [Id = "lifecycle", Class = "design-section", AriaLabelledby = "lifecycle-title"];
        element!(H2 [Id = "lifecycle-title"]; text("Leases, cancellation, and reviewed takeover")),
        element!(Dl [Class = "storage"];
            element!(Div;
                element!(Dt; text("Leases and cancel vs stopped")),
                element!(Dd;
                    text("Claims use a 60 s DB-time lease renewed every 10 s through worktree setup, CLI execution, and cleanup. Cancelling a "),
                    element!(Code; text("queued")),
                    text(" task transitions immediately to "),
                    element!(Code; text("cancelled")),
                    text("; cancelling a "),
                    element!(Code; text("running")),
                    text(" task sets "),
                    element!(Code; text("cancel_requested")),
                    text(" to fence publication and enters "),
                    element!(Code; text("cancelled")),
                    text(" only after proven process-scope stop. "),
                    element!(Code; text("record_stop")),
                    text(" is accepted post-expiry without reopening tasks or overwriting outcomes."),
                ),
            ),
            element!(Div;
                element!(Dt; text("No expired-lease replay of edits")),
                element!(Dd;
                    text("An attempt token fences database publication, not filesystem writes. Lease expiry, worker crash, or unconfirmed stop enters "),
                    element!(Code; text("needs_reconcile")),
                    text(" while keeping "),
                    element!(Code; text("workspace_reserved = true")),
                    text(". There is no automatic "),
                    element!(Code; text("running")),
                    text(" to "),
                    element!(Code; text("queued")),
                    text(" requeue."),
                ),
            ),
            element!(Div;
                element!(Dt; text("Trusted reconciliation before takeover")),
                element!(Dd;
                    text("There is no bare boolean override. The trusted local "),
                    element!(Code; text("Reconciler")),
                    text(" verifies the prior supervision scope stopped, inspects the locked worktree digest, and commits an immutable "),
                    element!(Code; text("coding_reconciliation_receipts")),
                    text(" row at the current task revision. "),
                    element!(Code; text("ide.work.resume")),
                    text(" requires that receipt and "),
                    element!(Code; text("expected_revision")),
                    text("; the next worker recomputes the actual worktree digest under its writer lock right before spawning."),
                ),
            ),
            element!(Div;
                element!(Dt; text("Strict outcome classification and mode mapping")),
                element!(Dd;
                    text("Entering "),
                    element!(Code; text("awaiting_review")),
                    text(" requires exit "),
                    element!(Code; text("0")),
                    text(", coherent nonempty NDJSON "),
                    element!(Code; text("SUCCESS")),
                    text(", owned "),
                    element!(Code; text("conversation_id")),
                    text(", zero unresolved CLI/permission errors, and confirmed scope stop. "),
                    element!(Code; text("quota_blocked")),
                    text(" comes only from trusted CLI error channels. Logical "),
                    element!(Code; text("plan")),
                    text(" and "),
                    element!(Code; text("review")),
                    text(" map to native "),
                    element!(Code; text("--mode plan")),
                    text(" (never literal "),
                    element!(Code; text("--mode review")),
                    text(" or "),
                    element!(Code; text("--mode implement")),
                    text(")."),
                ),
            ),
        ),
        diagram!(
            "Task lifecycle and reconciliation gate",
            "Terminal completed and cancelled states have no outgoing edges; every path back to queued passes through a valid reconciliation receipt and revision.",
            "/design/coding-agents/lifecycle.svg",
            "837",
            "1134",
            "State diagram from queued to running, cancel_requested, awaiting_review, stopped error states, and needs_reconcile, routing non-terminal recovery through a reconciliation receipt and revision gate before explicit resume or cancel.",
            "Open full-size lifecycle diagram"
        ),
        diagram!(
            "Quota exhaustion, uncertain stop, and reviewed takeover",
            "Quota blocks, lost leases, and crashes stop without auto-requeue; trusted reconciliation precedes ide.work.resume and pre-spawn workspace digest recheck.",
            "/design/coding-agents/takeover.svg",
            "1512",
            "2522",
            "Sequence diagram showing quota_blocked, lease-loss stop recording, and crash lease expiry all avoiding automatic requeue, followed by trusted Reconciler verification, receipt issuance, coordinator inspection, and ide.work.resume with recipe B.",
            "Open full-size takeover sequence diagram"
        ),
    ),
    element!(Section [Id = "contracts", Class = "design-section", AriaLabelledby = "contracts-title"];
        element!(H2 [Id = "contracts-title"]; text("Proposed API, DTO, and DAO contracts")),
        element!(P;
            text("All proposed destinations share "),
            element!(Code; text("POST /forge/v1/dispatch")),
            text(" -> private "),
            element!(Code; text("POST /v1/dispatch")),
            text(", strict "),
            element!(Code; text("kind")),
            text("/destination agreement, unknown-field rejection, and "),
            element!(Code; text("work_failed")),
            text(" typed error envelopes. Replay lookup checks "),
            element!(Code; text("canonical_intent_hash")),
            text(" before "),
            element!(Code; text("expected_revision")),
            text(" CAS and re-envelopes immutable receipts with the current "),
            element!(Code; text("request_id")),
            text("."),
        ),
        element!(Div [Class = "engine-table"];
            element!(Table;
                element!(Thead;
                    element!(Tr;
                        element!(Th; text("Destination (proposed)")),
                        element!(Th; text("Request kind & key fields")),
                        element!(Th; text("Response & status")),
                        element!(Th; text("Key rejections")),
                    ),
                ),
                element!(Tbody;
                    element!(Tr;
                        element!(Td; element!(Code; text("ide.work.submit"))),
                        element!(Td; element!(Code; text("work_submit")), text(": project_alias, base_revision, mode, agent_profile, prompt, acceptance_criteria, idempotency_key")),
                        element!(Td; element!(Code; text("work_submitted")), text(" (202 after COMMIT): server task_id, revision, recipe_digest")),
                        element!(Td; text("400 invalid, 403 grant denied, 409 idempotency_conflict, 503 queue full")),
                    ),
                    element!(Tr;
                        element!(Td; element!(Code; text("ide.work.get"))),
                        element!(Td; element!(Code; text("work_get")), text(": task_id, include_checkpoint")),
                        element!(Td; element!(Code; text("work_opened")), text(" (200): task header, attempt summary, preview, result_manifest, provenance-split checkpoint")),
                        element!(Td; text("404 not_found (including cross-owner)")),
                    ),
                    element!(Tr;
                        element!(Td; element!(Code; text("ide.work.result"))),
                        element!(Td; element!(Code; text("work_result")), text(": task_id, attempt_id, cursor, limit_bytes")),
                        element!(Td; element!(Code; text("work_result_page")), text(" (200): accepted result digest, UTF-8 page, next_cursor, complete")),
                        element!(Td; text("400 invalid cursor, 404 absent, 409 result mismatch")),
                    ),
                    element!(Tr;
                        element!(Td; element!(Code; text("ide.work.list"))),
                        element!(Td; element!(Code; text("work_list")), text(": status_filter, project_alias, limit, cursor")),
                        element!(Td; element!(Code; text("work_listed")), text(" (200): paginated task summaries")),
                        element!(Td; text("400 invalid cursor or limit")),
                    ),
                    element!(Tr;
                        element!(Td; element!(Code; text("ide.work.events"))),
                        element!(Td; element!(Code; text("work_events")), text(": task_id, after_sequence, limit")),
                        element!(Td; element!(Code; text("work_events")), text(" (200): ordered coding_events, next_sequence, status, revision")),
                        element!(Td; text("404 not_found")),
                    ),
                    element!(Tr;
                        element!(Td; element!(Code; text("ide.work.cancel"))),
                        element!(Td; element!(Code; text("work_cancel")), text(": task_id, expected_revision, idempotency_key, optional reconciliation_receipt_id")),
                        element!(Td; element!(Code; text("work_cancel_recorded")), text(" (200): base/result revision, status (cancel_requested or cancelled)")),
                        element!(Td; text("404, 409 idempotency / revision / reconcile_required")),
                    ),
                    element!(Tr;
                        element!(Td; element!(Code; text("ide.work.resume"))),
                        element!(Td; element!(Code; text("work_resume")), text(": task_id, expected_revision, agent_profile, reconciliation_receipt_id, note, idempotency_key")),
                        element!(Td; element!(Code; text("work_resumed")), text(" (202): status=queued, pinned new recipe_digest")),
                        element!(Td; text("404, 409 conflict / stale_reconciliation / task_active")),
                    ),
                    element!(Tr;
                        element!(Td; element!(Code; text("ide.work.accept"))),
                        element!(Td; element!(Code; text("work_accept")), text(": task_id, expected_revision, review_note, idempotency_key")),
                        element!(Td; element!(Code; text("work_accepted")), text(" (200): status=completed, base/result revision")),
                        element!(Td; text("404, 409 conflict / not_reviewable")),
                    ),
                ),
            ),
        ),
        element!(Div [Class = "recovery"];
            element!(Details;
                element!(Summary; text("Proposed PostgreSQL DAO tables (proposed migration slot 0009)")),
                element!(Dl [Class = "storage"];
                    element!(Div;
                        element!(Dt; element!(Code; text("forge_agent.coding_tasks"))),
                        element!(Dd; text("Immutable admission identity, pinned initial/active recipes, safe-integer revision, status, current_attempt_id, resume_receipt_id, and partial unique index on reserved workspace_id.")),
                    ),
                    element!(Div;
                        element!(Dt; element!(Code; text("forge_agent.coding_attempts"))),
                        element!(Dd; text("Per-attempt token, worker incarnation, DB-time lease and deadline, stop evidence, workspace digest, canonical outcome digest, result_manifest, and provenance-separated observed_checkpoint_json vs model_claims_json.")),
                    ),
                    element!(Div;
                        element!(Dt; element!(Code; text("forge_agent.coding_events"))),
                        element!(Dd; text("Row-locked ordered event log keyed by (task_id, sequence) with composite task/attempt FK.")),
                    ),
                    element!(Div;
                        element!(Dt; element!(Code; text("forge_agent.coding_control_receipts"))),
                        element!(Dd; text("Immutable cancel/resume/accept idempotency receipts keyed by (owner_id, task_id, idempotency_key) with canonical_intent_hash.")),
                    ),
                    element!(Div;
                        element!(Dt; element!(Code; text("forge_agent.coding_reconciliation_receipts"))),
                        element!(Dd; text("Append-only trusted reconciler proof binding owner, task, prior attempt, expected revision, workspace digest, and stop evidence.")),
                    ),
                    element!(Div;
                        element!(Dt; element!(Code; text("forge_agent.coding_result_chunks"))),
                        element!(Dd; text("Fenced UTF-8 result chunks keyed by (attempt_id, chunk_index), hidden from readers until publish commits the verified result_manifest.")),
                    ),
                ),
            ),
        ),
        element!(Div [Class = "recovery"];
            element!(Details;
                element!(Summary; text("Proposed submit request and 202 receipt example")),
                element!(Pre [Class = "contract-code"];
                    element!(Code; text("{\"kind\":\"work_submit\",\"request_id\":\"req-9f2a\",\"project_alias\":\"homelab\",\"base_revision\":\"2e1300a00da92e9f97bdb1a656c2b182d4219a6c\",\"mode\":\"plan\",\"agent_profile\":\"agy-plan-v1\",\"prompt\":\"Draft fix plan\",\"acceptance_criteria\":\"Bounded plan\",\"idempotency_key\":\"task-001\"}\n\n202 Accepted (after COMMIT):\n{\"protocol_version\":1,\"event\":{\"kind\":\"work_submitted\",\"request_id\":\"req-9f2a\",\"task_id\":\"8c1d4c3e-6b4f-4d2e-9a1b-2c3d4e5f6a7b\",\"revision\":1,\"status\":\"queued\",\"recipe_digest\":\"sha256:...\"}}")),
                ),
            ),
        ),
    ),
    element!(Section [Id = "rollout", Class = "design-section", AriaLabelledby = "rollout-title"];
        element!(H2 [Id = "rollout-title"]; text("Phased rollout and verification gates")),
        element!(Ol [Class = "contract-steps"];
            element!(Li;
                element!(Strong; text("Phase 1 — Backend, disposable PostgreSQL, and fake CLI (no paid calls).")),
                text(" Implement migration, Rust "),
                element!(Code; text("WorkConfig")),
                text("/"),
                element!(Code; text("WorkRelayFactory")),
                text(", runner "),
                element!(Code; text("WorkService")),
                text(", "),
                element!(Code; text("CodingWorker")),
                text(", and adapter. Verify "),
                element!(Code; text("SKIP LOCKED")),
                text(", owner "),
                element!(Code; text("404")),
                text(" isolation, replay-before-CAS, zero expired-lease replay, chunked paging above 64 KiB, and reconciliation/drift rejection."),
            ),
            element!(Li;
                element!(Strong; text("Phase 2 — Supported real read-only adapter smoke.")),
                text(" Exercise bounded "),
                element!(Code; text("plan")),
                text("/"),
                element!(Code; text("review")),
                text(" runs against the real "),
                element!(Code; text("agy")),
                text(" binary under the approved signed-in account in an isolated disposable worktree."),
            ),
            element!(Li;
                element!(Strong; text("Phase 3 — Generated UI later.")),
                text(" Author typed Rust/YAML portal views under existing worker ownership and verify with "),
                element!(Code; text("make ci")),
                text(" and "),
                element!(Code; text("make ui-qa")),
                text("."),
            ),
            element!(Li;
                element!(Strong; text("Phase 4 — Opt-in provisioning only.")),
                text(" Enable "),
                element!(Code; text("plan")),
                text("/"),
                element!(Code; text("review")),
                text(" first after role/service sign-off. Write-profile "),
                element!(Code; text("implement")),
                text(" remains blocked until verified supervision and OS/credential isolation exist. No production activation is part of this design."),
            ),
        ),
        element!(Nav [Class = "section-nav", AriaLabel = "Related coding-agent design references"];
            element!(A [Href = "/design/agent-usage"]; text("Agent usage and token limits (proposed)")),
            element!(A [Href = "/design/forge-providers"]; text("Forge plug-and-play")),
            element!(A [Href = "https://github.com/roachmaster/homelab/blob/main/docs/architecture/proposals/forge-coding-agent-dispatch.md"]; text("Full coding-agent dispatch Markdown")),
        ),
    ),
];

pub(super) const USAGE: &[Node] = &[
    element!(Div [Class = "intro"];
        element!(H1; text("Agent usage and token limits")),
        element!(P;
            element!(Strong; text("Proposed / not implemented.")),
            text(" Design-only architecture at base revision "),
            element!(Code; text("2e1300a00da92e9f97bdb1a656c2b182d4219a6c")),
            text(". Coding agents run under native provider limits instead of local "),
            element!(Code; text("llama.cpp")),
            text(" caps, while a planned "),
            element!(Code; text("/agents")),
            text(" portal view separates provider allowances, Forge-observed job tokens, context occupancy, and cost estimates without fabricated percentages. No live "),
            element!(Code; text("/agents")),
            text(" route, account probes, or usage percentages are active here."),
        ),
        element!(Nav [Class = "section-nav", AriaLabel = "Agent usage design sections"];
            element!(A [Href = "#limits"]; text("Limits and metrics")),
            element!(A [Href = "#providers"]; text("Provider matrix")),
            element!(A [Href = "#accounting"]; text("Providers and accounting")),
            element!(A [Href = "#layout"]; text("Illustrative layout")),
            element!(A [Href = "#rollout"]; text("Rollout")),
            element!(A [Href = "/design/coding-agents"]; text("Coding-agent dispatch (proposed)")),
        ),
    ),
    element!(Section [Id = "limits", Class = "design-section", AriaLabelledby = "limits-title"];
        element!(H2 [Id = "limits-title"]; text("Token-limit separation and planned /agents visibility")),
        element!(P;
            text("Coding-agent recipes default to "),
            element!(Code; text("generation_token_limit: null")),
            text(" (provider-managed) and never inherit Forge's local inference caps (Chat "),
            element!(Code; text("384")),
            text(", legacy/local "),
            element!(Code; text("2048")),
            text(", or Mail "),
            element!(Code; text("4096")),
            text(" context / "),
            element!(Code; text("1024")),
            text(" completion). Native provider context windows and allowances still apply, and full coding replies use chunked storage and paging independently of token generation."),
        ),
        element!(P;
            text("The planned "),
            element!(Code; text("/agents")),
            text(" view enters from Home and compact IDE overflow—avoiding top-level tab sprawl and keeping at most two header/navigation bars below "),
            element!(Code; text("640 px")),
            text(". Every metric carries "),
            element!(Code; text("source")),
            text(", "),
            element!(Code; text("scope")),
            text(", "),
            element!(Code; text("observed_at")),
            text(", and "),
            element!(Code; text("fresh | stale | unknown | unavailable")),
            text("."),
        ),
        element!(Dl [Class = "storage"];
            element!(Div;
                element!(Dt; text("Model, account alias, and worker status")),
                element!(Dd; text("Displays server-configured provider, model, and display alias alongside observed worker status and current task ID. An alias is a label, not account identity.")),
            ),
            element!(Div;
                element!(Dt; text("Provider allowance and reset")),
                element!(Dd; text("Shows provider-reported quota buckets and reset times (which may include activity outside Forge). Percentages and elapsed reset times never synthesize readiness or replenished quota.")),
            ),
            element!(Div;
                element!(Dt; text("Observed task tokens and runtime")),
                element!(Dd;
                    text("Tracks tokens and monotonic duration on Forge-managed jobs under explicit "),
                    element!(Code; text("Forge-observed jobs")),
                    text(" scope. Account-wide totals are kept separate and never added to job totals."),
                ),
            ),
            element!(Div;
                element!(Dt; text("Separately scoped activity, context, and cost estimates")),
                element!(Dd;
                    text("Disclosed progressively rather than mixed into primary counters. Context occupancy treats absent or zero capacity as "),
                    element!(Code; text("null")),
                    text(". Subscription and API billing stay distinct ("),
                    element!(Code; text("null != 0")),
                    text("), cost figures stay labeled as estimates, and tokens are never converted into a dollar bill."),
                ),
            ),
        ),
    ),
    element!(Section [Id = "providers", Class = "design-section", AriaLabelledby = "providers-title"];
        element!(H2 [Id = "providers-title"]; text("Provider capability matrix")),
        element!(P; text("Capabilities reflect installed CLI safe-help and published protocol schemas, not live authenticated probes. Missing or unverified feeds remain explicitly unknown or unsupported.")),
        element!(Div [Class = "engine-table"];
            element!(Table;
                element!(Thead;
                    element!(Tr;
                        element!(Th; text("Proposed adapter (CLI)")),
                        element!(Th; text("Observed job tokens")),
                        element!(Th; text("Provider allowance / reset")),
                        element!(Th; text("Activity, context, and constraints")),
                    ),
                ),
                element!(Tbody;
                    element!(Tr;
                        element!(Td; element!(Code; text("CodexUsageProvider")), text(" ("), element!(Code; text("codex-cli 0.160.0")), text(")")),
                        element!(Td; text("Per-turn "), element!(Code; text("turn.completed.usage")), text(" in exec mode; app-server "), element!(Code; text("last")), text(" vs cumulative "), element!(Code; text("total")), text(".")),
                        element!(Td; text("Machine-readable "), element!(Code; text("account/rateLimits/read")), text(" and updates: meter buckets, nullable primary/secondary windows, used fraction, and reset.")),
                        element!(Td; text("Machine-readable "), element!(Code; text("account/usage/read")), text(" supports nullable account activity/daily buckets and separate thread cost estimates. Authenticated availability is untested; "), element!(Code; text("ordinaryUsageAllowed")), text(" stays tri-state.")),
                    ),
                    element!(Tr;
                        element!(Td; element!(Code; text("AntigravityUsageProvider")), text(" ("), element!(Code; text("agy 1.5.12")), text(")")),
                        element!(Td; text("Session-cumulative result metadata (usage, turn count, and duration) from headless output.")),
                        element!(Td; text("Headless results do not report remaining quota. Statusline TUI documents bucket "), element!(Code; text("remaining_fraction")), text("/"), element!(Code; text("remaining_amount")), text(" and reset, but a supported headless allowance feed is still unverified, so allowance stays "), element!(Code; text("unknown")), text(".")),
                        element!(Td; text("Context and list-price estimates stay separate. No hidden-endpoint scraping, no transcript/email leakage, and no model turns to poll quota.")),
                    ),
                    element!(Tr;
                        element!(Td; text("Future registered adapters")),
                        element!(Td; text("Version-probed capability flags per installed CLI.")),
                        element!(Td; text("Explicit per-metric "), element!(Code; text("unsupported")), text(" or "), element!(Code; text("unavailable")), text(" when absent.")),
                        element!(Td; text("Same provenance, scope, and privacy rules.")),
                    ),
                ),
            ),
        ),
        element!(P [Class = "contract-note"];
            element!(Strong; text("Truthful reporting rules: ")),
            text("No fabricated combined percentages across providers, no token-to-dollar billing conversion, and no merging of account-wide totals into Forge-observed job totals."),
        ),
    ),
    element!(Section [Id = "accounting", Class = "design-section", AriaLabelledby = "accounting-title"];
        element!(H2 [Id = "accounting-title"]; text("Providers, collection, and exact accounting")),
        element!(Dl [Class = "storage"];
            element!(Div;
                element!(Dt; text("Injected UsageService, UsageStore, and UsageCollector")),
                element!(Dd;
                    text("Bootstrap validates "),
                    element!(Code; text("AgentUsageConfig")),
                    text(" and freezes effect-free registrars before opening sockets or subprocesses. Host injects "),
                    element!(Code; text("Singleton<UsageRelay>")),
                    text("; runner injects "),
                    element!(Code; text("UsageStore")),
                    text(" into "),
                    element!(Code; text("UsageService")),
                    text("; a separate native "),
                    element!(Code; text("UsageCollector")),
                    text(" owns versioned "),
                    element!(Code; text("AgentUsageProvider")),
                    text(" adapters under the signed-in OS boundary. No worker global registry; browser holds no credentials and makes no direct provider calls."),
                ),
            ),
            element!(Div;
                element!(Dt; text("Canonical feeds, baselines, dedup, and epoch + lineage")),
                element!(Dd;
                    text("Pin one canonical accounting feed per attempt (Codex exec or app-server; AGY session results or verified steps). Cumulative metadata computes deltas once against a verified matching session/epoch baseline in the same transaction as the canonical origin-key dedup receipt. Missing baselines or ambiguous counter decreases stay raw-only/unknown. Preallocated poll generation and configuration epoch prevent stale overwrites and account-switch mixing; parent-inclusive subagent totals require explicit covered-child identities."),
                ),
            ),
            element!(Div;
                element!(Dt; text("Metadata-only refresh and failure isolation")),
                element!(Dd;
                    element!(Code; text("ide.usage.refresh")),
                    text(" coalesces and rate-limits supported metadata reads with durable receipts ("),
                    element!(Code; text("202 usage_refresh_queued")),
                    text("). Refresh never runs generation, auth flows, spend/credit top-ups, or auto-rotation. Poll errors stay separate from last-success freshness, and telemetry failures never fail completed coding work."),
                ),
            ),
        ),
        element!(Div [Class = "recovery"];
            element!(Details;
                element!(Summary; text("Proposed usage destinations and PostgreSQL storage")),
                element!(Dl [Class = "storage"];
                    element!(Div;
                        element!(Dt; text("Proposed destinations (POST /forge/v1/dispatch)")),
                        element!(Dd;
                            element!(Code; text("ide.usage.overview")),
                            text(" (paginated summary + coverage), "),
                            element!(Code; text("ide.usage.detail")),
                            text(" (configured alias allowance windows, job links, optional diagnostics), and "),
                            element!(Code; text("ide.usage.refresh")),
                            text(" (idempotent coalesced metadata poll). Owner and project grants filter rows before aggregation; large counters use validated decimal strings."),
                        ),
                    ),
                    element!(Div;
                        element!(Dt; text("Proposed tables (migration slot unfrozen)")),
                        element!(Dd;
                            element!(Code; text("attempt_usage_samples")),
                            text(" (canonical origin-key dedup and lineage), "),
                            element!(Code; text("agent_allowance_snapshots")),
                            text(" (monotonic generation + multi-window buckets), "),
                            element!(Code; text("agent_account_activity_snapshots")),
                            text(" (separate account activity and labeled cost estimates), and "),
                            element!(Code; text("agent_usage_refreshes")),
                            text(" (coalesced idempotent refresh receipts)."),
                        ),
                    ),
                ),
            ),
        ),
        diagram!(
            "Usage providers and composition boundaries",
            "Frozen configuration, host-scoped UsageRelay, runner UsageService/UsageStore, and native UsageCollector with versioned provider adapters.",
            "/design/agent-usage/providers.svg",
            "1668",
            "921",
            "Class diagram showing HostFactory staging UsageRelayFactory from AgentUsageConfig to create Singleton UsageRelay, relaying to UsageService with injected UsageStore, and UsageCollector with injected UsageStore and scoped AgentUsageProvider adapters for Codex and Antigravity.",
            "Open full-size usage provider diagram"
        ),
        diagram!(
            "Metadata collection, overview reads, and refresh sequence",
            "Native collector commits samples and monotonic allowance snapshots; admitted browser reads and coalesced metadata-only refreshes never touch provider credentials.",
            "/design/agent-usage/collection.svg",
            "1293",
            "1683",
            "Sequence diagram showing UsageCollector reading trusted native provider metadata into PostgreSQL, Browser WASM calling ide.usage.overview and ide.usage.refresh through Rust Host and UsageService, and UsageCollector waking to commit monotonic allowance snapshots.",
            "Open full-size usage collection diagram"
        ),
    ),
    element!(Section [Id = "layout", Class = "design-section", AriaLabelledby = "layout-title"];
        element!(H2 [Id = "layout-title"]; text("Illustrative /agents layout (placeholders only)")),
        element!(P [Class = "contract-note"];
            element!(Strong; text("Static design illustration only: ")),
            text("Placeholder rows and "),
            element!(Code; text("unknown")),
            text(" states built from typed Rust nodes to preview information hierarchy. These are not working controls, not a live "),
            element!(Code; text("/agents")),
            text(" screen, and not live account data."),
        ),
        element!(Div [Class = "contract-examples"];
            element!(Div [Class = "contract-example"];
                element!(H3; text("Desktop /agents (placeholder wireframe)")),
                element!(P;
                    element!(Small; text("Header bar: Forge · Home · Chat · Mail · Workbench · More — Scope: Forge-observed jobs · Metadata refresh (placeholder)")),
                ),
                element!(Div [Class = "engine-table"];
                    element!(Table;
                        element!(Thead;
                            element!(Tr;
                                element!(Th; text("Provider / Model / Alias")),
                                element!(Th; text("Worker / Task")),
                                element!(Th; text("Allowance & Reset")),
                                element!(Th; text("Forge Jobs")),
                                element!(Th; text("Updated")),
                            ),
                        ),
                        element!(Tbody;
                            element!(Tr;
                                element!(Td; text("codex · <model> (codex-primary)")),
                                element!(Td; text("unknown (<task_id>)")),
                                element!(Td; text("unknown (untested)")),
                                element!(Td; text("— / —")),
                                element!(Td; text("unknown")),
                            ),
                            element!(Tr;
                                element!(Td; text("agy · <model> (agy-primary)")),
                                element!(Td; text("unknown (<task_id>)")),
                                element!(Td; text("unknown (unsupported_headless)")),
                                element!(Td; text("— / —")),
                                element!(Td; text("unknown")),
                            ),
                        ),
                    ),
                ),
                element!(Details;
                    element!(Summary; text("Detail disclosure: Context & cost estimate (illustrative — closed by default)")),
                    element!(P;
                        element!(Small; text("Context occupancy: unknown (capacity null). Cost estimate: unknown (subscription plan — not billed from tokens). Account activity: separately scoped, unknown.")),
                    ),
                ),
            ),
            element!(Div [Class = "contract-example"];
                element!(H3; text("Phone /agents (<= 2 bars, placeholder wireframe)")),
                element!(P;
                    element!(Small; text("Bar 1: Forge · Agents · Metadata refresh (static)")),
                ),
                element!(P;
                    element!(Small; text("Bar 2: Chat · Mail · Workbench · More")),
                ),
                element!(Dl [Class = "storage"];
                    element!(Div;
                        element!(Dt; text("codex · <model> / codex-primary · unknown")),
                        element!(Dd; text("Worker: unknown (<task_id>)")),
                        element!(Dd; text("Provider allowance: unknown (authenticated feed untested)")),
                        element!(Dd; text("Forge-observed jobs: — tokens · — runtime · updated unknown")),
                    ),
                    element!(Div;
                        element!(Dt; text("agy · <model> / agy-primary · unknown")),
                        element!(Dd; text("Worker: unknown (<task_id>)")),
                        element!(Dd; text("Provider allowance: unknown (unsupported_headless)")),
                        element!(Dd; text("Forge-observed jobs: — tokens · — runtime · updated unknown")),
                    ),
                ),
            ),
        ),
    ),
    element!(Section [Id = "rollout", Class = "design-section", AriaLabelledby = "rollout-title"];
        element!(H2 [Id = "rollout-title"]; text("Phased verification and rollout gates")),
        element!(Ol [Class = "contract-steps"];
            element!(Li;
                element!(Strong; text("Phase 1 — Backend, disposable PostgreSQL, and fake providers (no model calls).")),
                text(" Verify idempotent replay/conflicts, canonical-feed deduplication, cumulative baselines, delayed/decreasing/reset samples, covered-child lineage, null/decimal-string counters, out-of-order poll generations, account-switch epoch rotation, pre-aggregation owner/project isolation, coalesced refresh receipts, and telemetry failure isolation."),
            ),
            element!(Li;
                element!(Strong; text("Phase 2 — Generated UI gate (separate).")),
                text(" Build typed Rust/YAML "),
                element!(Code; text("/agents")),
                text(" views under "),
                element!(Code; text("make ci")),
                text(" and "),
                element!(Code; text("make ui-qa")),
                text(" (desktop and phone 2-bar budget, keyboard/focus, and honest "),
                element!(Code; text("unknown")),
                text("/"),
                element!(Code; text("stale")),
                text(" states)."),
            ),
            element!(Li;
                element!(Strong; text("Phase 3 — Versioned read-only real-CLI capability smoke.")),
                text(" Verify installed CLI capability feeds before enabling live collectors; never auto-enable unverified feeds or TUI-only hooks headless."),
            ),
        ),
        element!(Nav [Class = "section-nav", AriaLabel = "Related agent usage design references"];
            element!(A [Href = "/design/coding-agents"]; text("Coding-agent dispatch (proposed)")),
            element!(A [Href = "/design/forge-providers"]; text("Forge plug-and-play")),
            element!(A [Href = "https://github.com/roachmaster/homelab/blob/main/docs/architecture/proposals/forge-agent-usage.md"]; text("Full agent usage Markdown")),
        ),
    ),
];
