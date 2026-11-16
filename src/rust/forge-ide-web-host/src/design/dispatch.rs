use super::{text, Attribute, Node, Tag};

pub(super) const BODY: &[Node] = &[
    element!(Div [Class = "intro"];
        element!(H1; text("PostgreSQL event dispatch for Forge GPU inference")),
        element!(P;
            element!(Strong; text("Dispatch design.")),
            text(" In PostgreSQL dispatch mode, Mission Control accepts chat turns while Forge GPU is offline. A worker on Forge claims pending jobs and calls the model locally."),
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
                element!(P; text("The Mission Control runner writes runs, input messages and replayable events to PostgreSQL. Its worker calls Forge GPU over an inference tunnel. The chat compatibility route can query the active model before queueing and wait up to 13 seconds for a reply.")),
            ),
            element!(Section [Class = "contract-example", AriaLabelledby = "proposed-title"];
                element!(H3 [Id = "proposed-title"]; text("PostgreSQL dispatch mode")),
                element!(P; text("Mission Control commits the chat turn and returns a run ID. Forge GPU claims the job and publishes a result through PostgreSQL. Mission Control commits the transcript and serves ordered events to the browser.")),
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
                    element!(Img [Src = "/design/chat-dispatch-sequence.svg", Width = "1072", Height = "1355", Loading = "lazy", Alt = "Sequence showing the IDE, Mission Control, PostgreSQL, Forge GPU worker and local model adapter."]),
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
                    element!(Li; text("The Forge worker subscribes to job notifications and scans for eligible jobs. It claims a job, receives an attempt token and calls its local model adapter. It renews the lease while inference runs.")),
                    element!(Li; text("The worker publishes the bounded reply or failure for the current attempt. PostgreSQL commits the result and wakes the Mission Control completion consumer.")),
                    element!(Li; text("Mission Control commits the transcript, final status and terminal event. The IDE requests the events after its last sequence and receives the saved reply on completion.")),
                ),
            ),
        ),
    ),
    element!(Section [Id = "data-access", Class = "design-section", AriaLabelledby = "data-access-title"];
        element!(H2 [Id = "data-access-title"]; text("PostgreSQL data access")),
        element!(P;
            text("The queue reuses "),
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
                    element!(Code; text("ide.chat.run.submit")),
                ),
                element!(Dd;
                    text("Send a stable chat ID, the new turn and an idempotency key. Include the expected revision for a continuation. Receive "),
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
        element!(P; text("The IDE keeps the pending run ID in its tab checkpoint. A reload resumes authorized event reads even for a first turn that is still waiting to appear in Previous chats. The browser uses short event-page requests because the current web host serves one request at a time. Chat history actions keep their existing destinations.")),
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
        element!(P; text("PostgreSQL derives the worker identity from its connection role. Mission Control derives chat ownership from the admitted session.")),
    ),
    element!(Section [Id = "connection", Class = "design-section", AriaLabelledby = "connection-title"];
        element!(H2 [Id = "connection-title"]; text("Connection and privileges")),
        element!(P;
            text("Forge initiates an SSH forward to a PostgreSQL listener bound to Mission Control loopback. Mission Control keeps its Unix socket connection. Limit the forwarding account to the database destination and disable shell and subsystem sessions. Limit host-based authentication on loopback to the agent database and the Forge worker login. "),
            element!(A [Href = "https://man.openbsd.org/sshd_config#PermitOpen"]; text("OpenSSH forwarding controls")),
            text(" · "),
            element!(A [Href = "https://www.postgresql.org/docs/16/auth-pg-hba-conf.html"]; text("PostgreSQL host-based authentication")),
        ),
        element!(P;
            text("Give the Forge login execution grants for worker readiness, claim, renew and publish operations. It receives inference input through the claim and has no table grants for chat history, browser checkpoints or unrelated jobs. Use a trusted function search path and revoke public execution in the same transaction that creates the functions. "),
            element!(A [Href = "https://www.postgresql.org/docs/16/sql-createfunction.html#SQL-CREATEFUNCTION-SECURITY"]; text("PostgreSQL function privileges")),
        ),
    ),
    element!(Section [Id = "rollout", Class = "design-section", AriaLabelledby = "rollout-title"];
        element!(H2 [Id = "rollout-title"]; text("Cutover and acceptance checks")),
        element!(Ol [Class = "flow"];
            element!(Li;
                element!(H3; text("Stage dispatch")),
                element!(P; text("Exercise the queue operations, role and API with a fake Forge worker and disposable PostgreSQL. Drop the notification listener during submission and verify the reconnect scan finds the job.")),
            ),
            element!(Li;
                element!(H3; text("Test offline and retry")),
                element!(P; text("Submit with Forge off and verify the job completes when Forge returns. Test duplicate submission, a worker crash, a rejected late result and a Mission Control restart after publication.")),
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
                element!(H3; text("Move inference to Forge")),
                element!(P; text("Drain and disable the legacy worker before enabling Forge claims. Its recovery process marks running jobs interrupted. Retire the old inference tunnel after verification. To roll back, stop Forge claims and resolve its leases and receipts before restoring the legacy worker.")),
            ),
        ),
    ),
];
