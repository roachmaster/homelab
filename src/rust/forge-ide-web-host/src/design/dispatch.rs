use super::{text, Attribute, Node, Tag};

pub(super) const BODY: &[Node] = &[
    element!(Div [Class = "intro"];
        element!(H1; text("PostgreSQL event dispatch for Forge GPU inference")),
        element!(P;
            element!(Strong; text("Implemented dispatch mode.")),
            text(" Reviewed 18 November 2026 against the committed runner, SQL worker API, Rust request contracts, and generated UI. Mission Control can admit work while Forge GPU is offline; the Forge worker claims eligible jobs and calls its local model adapter."),
        ),
        element!(Nav [Class = "section-nav", AriaLabel = "Event dispatch design sections"];
            element!(A [Href = "#overview"]; text("Overview")),
            element!(A [Href = "#sequence"]; text("Sequence diagram")),
            element!(A [Href = "#data-access"]; text("DAO")),
            element!(A [Href = "#contracts"]; text("DTOs")),
            element!(A [Href = "#connection"]; text("Connection")),
            element!(A [Href = "#rollout"]; text("Rollout")),
        ),
    ),
    element!(Section [Id = "overview", Class = "design-section", AriaLabelledby = "overview-title"];
        element!(H2 [Id = "overview-title"]; text("Inference responsibilities")),
        element!(Div [Class = "contract-examples"];
            element!(Section [Class = "contract-example", AriaLabelledby = "deployed-title"];
                element!(H3 [Id = "deployed-title"]; text("Direct inference mode")),
                element!(P; text("The retained legacy runner mode owns an inference worker on Mission Control and calls Forge over the model tunnel. Its compatibility chat route can wait up to 13 seconds. It remains an explicit alternate configuration; it is not the durable GPU-worker path.")),
            ),
            element!(Section [Class = "contract-example", AriaLabelledby = "proposed-title"];
                element!(H3 [Id = "proposed-title"]; text("PostgreSQL dispatch mode")),
                element!(P; text("The dispatch service unit selects FORGE_RUNNER_MODE=dispatch. Mission Control commits the turn and returns 202 with a run ID. The Forge GPU Python worker claims through the restricted PostgreSQL API, publishes a result inbox row, and Mission Control finalizes the transcript and ordered terminal event.")),
            ),
        ),
        element!(P; text("PostgreSQL stays on Mission Control and is the shared availability dependency. During a database outage, the API cannot confirm new jobs and Forge cannot claim work or publish results. Committed jobs and results are available after database recovery.")),
    ),
    element!(Section [Id = "sequence", Class = "design-section", AriaLabelledby = "sequence-title"];
        element!(H2 [Id = "sequence-title"]; text("Request and completion sequence")),
        element!(Div [Class = "gallery contract-sequences"];
            element!(Figure [Class = "wireframe"];
                element!(Figcaption;
                    element!(H3; text("From chat submission to saved reply")),
                ),
                element!(A [Class = "drawing", Href = "/design/chat-dispatch-sequence.svg"];
                    element!(Img [Src = "/design/chat-dispatch-sequence.svg", Width = "1455", Height = "1887", Loading = "lazy", Alt = "Sequence showing the IDE, Mission Control, PostgreSQL, Forge GPU worker and local model adapter."]),
                    element!(Span [Class = "open-label"];
                        text("Open full-size diagram "),
                        element!(Span [AriaHidden = "true"]; text("↗")),
                    ),
                ),
                element!(Ol [Class = "contract-steps"];
                    element!(Li;
                        text("The IDE sends a chat turn through Forge dispatch. Mission Control commits the run, input and "),
                        element!(Code; text("queued")),
                        text(" event, then returns "),
                        element!(Code; text("202 Accepted")),
                        text(" with the run ID. The job waits in PostgreSQL while Forge is offline."),
                    ),
                    element!(Li; text("The Forge worker subscribes before scanning. It selects the next eligible model, loads it through its local adapter, then claims an eligible job with an attempt token and a 60-second lease. A local inference thread permits lease renewal every ten seconds; bounded five-second scans recover missed wake-ups.")),
                    element!(Li; text("The worker publishes the bounded reply or failure for the current attempt. PostgreSQL commits the result and wakes the Mission Control completion consumer.")),
                    element!(Li; text("Mission Control commits the transcript, final status and terminal event. The IDE requests the events after its last sequence and receives the saved reply on completion.")),
                ),
            ),
        ),
    ),
    element!(Section [Id = "data-access", Class = "design-section", AriaLabelledby = "data-access-title"];
        element!(H2 [Id = "data-access-title"]; text("PostgreSQL data access")),
        element!(P;
            text("The implemented queue reuses "),
            element!(Code; text("runs")),
            text(", "),
            element!(Code; text("messages")),
            text(" and "),
            element!(Code; text("run_events")),
            text(". Each run tracks a lease deadline, attempt number, attempt token, next eligible time and absolute run deadline. The result inbox stores one result per run with a cascading foreign key."),
        ),
        element!(Dl [Class = "storage"];
            element!(Div;
                element!(Dt; text("Admit a run")),
                element!(Dd;
                    text("Pin an allowed model and atomically store the input, run and "),
                    element!(Code; text("queued")),
                    text(" event. Forge records its ready model and last-seen time so the IDE can show "),
                    element!(Strong; text("Waiting for Forge")),
                    text("."),
                ),
            ),
            element!(Div;
                element!(Dt; text("Claim and renew a lease")),
                element!(Dd;
                    text("Claim the oldest eligible unlocked job for a supported model with "),
                    element!(Code; text("FOR UPDATE SKIP LOCKED")),
                    text(". Use the database clock for lease renewal and release the transaction before inference. A lost lease allows retry within the run limits. Only the current attempt can publish an accepted result. "),
                    element!(A [Href = "https://www.postgresql.org/docs/16/sql-select.html#SQL-FOR-UPDATE-SHARE"]; text("PostgreSQL 16 row locking")),
                ),
            ),
            element!(Div;
                element!(Dt; text("Publish a result")),
                element!(Dd;
                    text("Check the current attempt and lease, store a bounded result, set the run to "),
                    element!(Code; text("result_ready")),
                    text(" and notify Mission Control in one transaction. A retry of an accepted publication can return its existing receipt or terminal status. A run with a published result is ineligible for another claim."),
                ),
            ),
            element!(Div;
                element!(Dt; text("Finalize a run")),
                element!(Dd; text("Drain the inbox on startup and reconnect. Apply the existing chat DAO rules and acknowledge the result in the same transaction as the terminal event and transcript update.")),
            ),
        ),
        element!(P [Class = "contract-note"];
            element!(Code; text("LISTEN/NOTIFY")),
            text(" wakes the workers. Run and result rows provide durability. Commit "),
            element!(Code; text("LISTEN")),
            text(", scan in a new transaction and rescan on notification, timer or reconnect. Use empty notification payloads because notifications are visible to other database users. "),
            element!(A [Href = "https://www.postgresql.org/docs/16/sql-listen.html"]; text("PostgreSQL 16 LISTEN")),
            text(" · "),
            element!(A [Href = "https://www.postgresql.org/docs/16/sql-notify.html"]; text("NOTIFY")),
        ),
        element!(P;
            text("Mission Control expires unfinished jobs at the run deadline. A result accepted before the deadline can still finalize if the chat is available. Acquire locks in chat, run, then result order. Cancellation, deletion and chat expiry invalidate later writes. Deletion and expiry purge related work and receipts. Finalization checks the current chat policy and accepted attempt. "),
            element!(A [Href = "/design/chat-history"]; text("Previous chats technical design")),
        ),
    ),
    element!(Section [Id = "contracts", Class = "design-section", AriaLabelledby = "contracts-title"];
        element!(H2 [Id = "contracts-title"]; text("Header routing and data transfer objects")),
        element!(P;
            text("The browser sends "),
            element!(Code; text("POST /forge/v1/dispatch")),
            text(" with the existing session and CSRF checks. The "),
            element!(Code; text("Forge-Destination")),
            text(" header selects the operation."),
        ),
        element!(Dl [Class = "storage"];
            element!(Div;
                element!(Dt;
                    element!(Code; text("ide.chat.run.models")),
                ),
                element!(Dd; text("List the model IDs the runner accepts, the default model and which model is currently ready on Forge.")),
            ),
            element!(Div;
                element!(Dt;
                    element!(Code; text("ide.chat.run.submit")),
                ),
                element!(Dd;
                    text("Send a stable chat ID, the new turn, the selected "),
                    element!(Code; text("model_id")),
                    text(" and an idempotency key. Include the expected revision for a continuation. Receive "),
                    element!(Code; text("202 Accepted")),
                    text(" with the run ID, chat ID and queued status after commit. Retrying with the same key returns the existing run."),
                ),
            ),
            element!(Div;
                element!(Dt;
                    element!(Code; text("ide.chat.run.events")),
                ),
                element!(Dd;
                    text("Send the run ID and "),
                    element!(Code; text("after_sequence")),
                    text(". Receive the current state, a bounded page of ordered events and the next sequence cursor. A completed run also returns the saved reply and chat revision."),
                ),
            ),
            element!(Div;
                element!(Dt;
                    element!(Code; text("ide.chat.run.cancel")),
                ),
                element!(Dd; text("Send the run ID. Receive the current cancellation or terminal status.")),
            ),
        ),
        element!(P; text("The runner pins a configured GPT-OSS 20B or Qwen2.5-Coder 7B model for each Chat run. GPT-OSS 120B is the separate fixed Mail recipient. Chat submissions currently reserve 384 completion tokens and expire after ten minutes; Mail uses its own profile and four-hour deadline. Forge loads the next eligible model before claiming its job. The IDE keeps the pending run ID and its model in the tab checkpoint. A reload resumes authorized event reads even for a first turn that is still waiting to appear in Previous chats. The browser uses short event-page requests because the current web host serves one request at a time. Chat history actions keep their existing destinations.")),
        element!(Div [Class = "contract-examples"];
            element!(Section [Class = "contract-example", AriaLabelledby = "claim-dto-title"];
                element!(H3 [Id = "claim-dto-title"]; text("Worker claim DTO")),
                element!(P; text("Run ID, model, bounded inference input, attempt token and deadline.")),
            ),
            element!(Section [Class = "contract-example", AriaLabelledby = "result-dto-title"];
                element!(H3 [Id = "result-dto-title"]; text("Worker result DTO")),
                element!(P; text("Run ID, attempt token and bounded reply or failure.")),
            ),
        ),
        element!(P; text("PostgreSQL derives the worker identity from its connection role. The runner uses its configured mission-control owner after native session and runner-credential admission; clients cannot select an owner.")),
    ),
    element!(Section [Id = "connection", Class = "design-section", AriaLabelledby = "connection-title"];
        element!(H2 [Id = "connection-title"]; text("Connection and privileges")),
        element!(P;
            text("The supplied forge-postgres-tunnel service creates a Forge-initiated local forward to Mission Control’s loopback PostgreSQL listener; the worker connects through Forge loopback port 15432. Mission Control’s runner uses its Unix socket. Provisioning restricts the forwarding account and worker database role; configured service files are deployment inputs, not proof that a remote machine is currently online. "),
            element!(A [Href = "https://man.openbsd.org/sshd_config#PermitOpen"]; text("OpenSSH forwarding controls")),
            text(" · "),
            element!(A [Href = "https://www.postgresql.org/docs/16/auth-pg-hba-conf.html"]; text("PostgreSQL host-based authentication")),
        ),
        element!(P;
            text("The migrations define the forge_agent_worker_api advertise, next_model, claim, renew, and publish operations. The worker gets execution grants and no direct chat/checkpoint table access. SECURITY DEFINER functions set a trusted search path, derive the worker from session_user, and revoke default public execution. "),
            element!(A [Href = "https://www.postgresql.org/docs/16/sql-createfunction.html#SQL-CREATEFUNCTION-SECURITY"]; text("PostgreSQL function privileges")),
        ),
    ),
    element!(Section [Id = "rollout", Class = "design-section", AriaLabelledby = "rollout-title"];
        element!(H2 [Id = "rollout-title"]; text("Implemented controls and acceptance checks")),
        element!(Ol [Class = "flow"];
            element!(Li;
                element!(H3; text("Durable queue controls")),
                element!(P; text("SQL migrations and worker/runner test modules cover the queue, current-attempt publication, and reconnect scans. LISTEN/NOTIFY carries empty wake-up payloads; committed rows remain authoritative. Running this documentation build does not repeat remote outage or hardware qualification.")),
            ),
            element!(Li;
                element!(H3; text("Offline and retry behavior")),
                element!(P; text("A committed run can wait offline until its deadline. Expired leases permit another eligible attempt, so inference can execute more than once; only an accepted current attempt can finalize. Duplicate submission uses the same idempotency key. New deployments should still exercise a powered-off Forge, worker loss, and a Mission Control restart.")),
            ),
            element!(Li;
                element!(H3; text("Test browser and retention")),
                element!(P;
                    text("Reload during the first turn. Test cancellation, chat deletion and expiry during inference. Track queue age, worker heartbeat, lease expirations and time in "),
                    element!(Code; text("result_ready")),
                    text("."),
                ),
            ),
            element!(Li;
                element!(H3; text("Mode selection and rollback")),
                element!(P; text("Dispatch mode starts the completion consumer instead of the legacy inference worker. An explicitly configured tool repository can still start a separate tools-only worker on Mission Control; it must not claim distributed Chat/Mail runs. Before changing modes, drain or resolve leases and result inbox rows. The retained legacy recovery path must not run concurrently against distributed inference.")),
            ),
        ),
        element!(P;
            text("Source evidence: "),
            element!(A [Href = "https://github.com/roachmaster/homelab/blob/main/deploy/mission-control/agent-runner/runner.py"]; text("runner modes and completion loop")),
            text(" · "),
            element!(A [Href = "https://github.com/roachmaster/homelab/blob/main/deploy/mission-control/agent-runner/forge_gpu_worker.py"]; text("Forge worker")),
            text(" · "),
            element!(A [Href = "https://github.com/roachmaster/homelab/blob/main/deploy/mission-control/sql/migrations/0005_inference_dispatch.sql"]; text("leased worker SQL API")),
        ),
    ),
];
