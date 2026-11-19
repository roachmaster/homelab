use super::{text, Attribute, Node, Tag};

pub(super) const BODY: &[Node] = &[
    element!(Div [Class = "intro"];
        element!(H1; text("Forge Mail design")),
        element!(P; text("Implemented correspondence for GPT-OSS 120B: queue a request, return for its reply, and retain its complete thread after automatic recall-limit or optional manual archiving.")),
        element!(P [Class = "contract-note"]; text("Repository archive contract updated 19 November 2026. This page describes Rust UI, Python runner, PostgreSQL migrations, and Forge adapter support; live enablement is separate. Operational model settings must match on both hosts.")),
        element!(Nav [Class = "section-nav", AriaLabel = "Mail design sections"];
            element!(A [Href = "#wireframe"]; text("Wireframe")),
            element!(A [Href = "#recall"]; text("Recall and archive")),
            element!(A [Href = "#dispatch"]; text("Dispatch sequence")),
            element!(A [Href = "#contracts"]; text("Data contracts")),
            element!(A [Href = "#qualification"]; text("Forge qualification")),
        ),
    ),
    element!(Section [Id = "wireframe", Class = "design-section", AriaLabelledby = "wireframe-title"];
        element!(H2 [Id = "wireframe-title"]; text("Mail workspace")),
        element!(Div [Class = "gallery phone-gallery"];
            element!(Figure [Class = "wireframe"];
                element!(Figcaption; element!(H3; text("Current phone inbox")), element!(P; text("Controlled browser fixture: folder tabs, sender, date, subject, preview, and Compose."))),
                element!(Div [Class = "wireframe-stage"];
                    element!(A [Class = "drawing", Href = "/design/forge-ide-mail-inbox-phone.png"];
                        element!(Img [Src = "/design/forge-ide-mail-inbox-phone.png", Width = "390", Height = "720", Loading = "lazy", Alt = "Current Forge Mail inbox with compact folder tabs, preview rows, and Compose."]),
                        element!(Span [Class = "open-label"]; text("Open full-size inbox →")),
                    ),
                ),
            ),
            element!(Figure [Class = "wireframe"];
                element!(Figcaption; element!(H3; text("Current phone thread")), element!(P; text("Controlled browser fixture: email headers, rendered message bodies, and Reply to thread."))),
                element!(Div [Class = "wireframe-stage"];
                    element!(A [Class = "drawing", Href = "/design/forge-ide-mail-thread-phone.png"];
                        element!(Img [Src = "/design/forge-ide-mail-thread-phone.png", Width = "390", Height = "844", Loading = "lazy", Alt = "Current Forge Mail thread with email-style messages and its reply control."]),
                        element!(Span [Class = "open-label"]; text("Open full-size thread →")),
                    ),
                ),
            ),
        ),
        element!(P;
            text("The "),
            element!(A [Href = "/design/forge-ide-mail-wireframe.svg"]; text("original Mail wireframe")),
            text(" is retained as an earlier design record. The captures above and the behavior below describe the implemented interface."),
        ),
        element!(Ol [Class = "contract-steps"];
            element!(Li; text("Open Mail from the shared IDE navigation. Inbox and Archive are compact folder tabs with Refresh. Inbox rows show the GPT-OSS 120B sender, available date, subject, status, and a bounded preview; Compose is a floating action. Desktop keeps a list and reader together. A phone shows one pane with Back to threads.")),
            element!(Li; text("Choose Compose, enter a subject and message, and select Send. The recipient is fixed to GPT-OSS 120B. After the runner commits the message and run, the saved thread shows Queued, Waiting for Forge, Running, Reply ready, or Failed. Closing the browser after acceptance does not cancel the queued job.")),
            element!(Li; text("Read email-style sender and recipient headers, per-message dates when supplied, and safe rendered Markdown. Reply to thread opens the inline follow-up composer. One run can be active per thread. Retry saved message reuses the failed inference input. Automatic five-second refresh is scheduled by Rust while a visible list or thread contains processing work.")),
            element!(Li; text("Archive retains the ordered transcript and its archive reason, with message paging. Archived threads are read-only. Compose starts a separate thread. Manual archive is implemented as an opt-in terminal state for Ready/Failed threads stopping follow-ups; there is no unarchive/delete operation or persisted read/unread flag; Reply ready is the generation status.")),
        ),
    ),
    element!(Section [Id = "recall", Class = "design-section", AriaLabelledby = "recall-title"];
        element!(H2 [Id = "recall-title"]; text("Recall and automatic archive")),
        element!(P; text("PostgreSQL stores the ordered user messages and visible assistant replies. For each run, Forge receives the completed exchanges and the current message, in order. Paging the thread in the browser only changes what you see. It leaves the prompt history intact.")),
        element!(P; text("Each thread pins gpt-oss-120b-harmony-v1. Mission Control counts the complete Harmony prompt offline, including the system message and next assistant prefix. The Forge adapter checks the profile and token count again before inference. Browser message paging does not increase the model context.")),
        element!(P [Class = "contract-note"];
            text("After a saved reply, archive when the remaining input allowance is below 256 tokens, or when the 64-message / 65,536-byte replay bound is reached. The next generation reserve and margin are included in the calculation. An oversized new draft returns context_limit before a message or job is created; it remains editable instead of archiving an otherwise usable thread."),
        ),
        element!(P; text("The reader shows remaining recall allowance when the thread is idle. Archive removes Reply to thread and shows its reason. Replay includes only exchanges with a completed assistant reply, followed by the new message. Failed user submissions remain visible but do not become completed recall pairs. Retry uses the saved run input; finalization adds at most one assistant reply for that source turn. A generation-limit reply is retained with a length marker.")),
        element!(P;
            text("The adapter enables cache_prompt for Mail. One loaded model slot can reuse the unchanged prefix in memory; model replacement or restart discards it. PostgreSQL messages rebuild a cold prompt. Persistent KV snapshots, summaries, and external semantic memory are not implemented. "),
            element!(A [Href = "https://github.com/ggml-org/llama.cpp/blob/master/tools/server/README.md"]; text("llama.cpp server documentation")),
        ),
    ),
    element!(Section [Id = "dispatch", Class = "design-section", AriaLabelledby = "dispatch-title"];
        element!(H2 [Id = "dispatch-title"]; text("Background dispatch sequence")),
        element!(P; text("Mission Control accepts Mail through the existing Forge dispatch endpoint. PostgreSQL commits the message and queued job before returning an acceptance. Notifications wake consumers. The durable queue and rescans recover missed wake-ups. The Forge GPU worker claims a job under a renewable lease and sends the prompt to the local 120B adapter.")),
        element!(Div [Class = "gallery contract-sequences"];
            element!(Figure [Class = "wireframe"];
                element!(Figcaption; element!(H3; text("From Send to Reply ready or Archive"))),
                element!(A [Class = "drawing", Href = "/design/mail-dispatch-sequence.svg"];
                    element!(Img [Src = "/design/mail-dispatch-sequence.svg", Width = "1463", Height = "1963", Loading = "lazy", Alt = "Mission Control commits a job in PostgreSQL and returns acceptance. Forge claims it, runs GPT-OSS 120B and publishes the reply. Mission Control saves the reply and decides whether to archive."]),
                    element!(Span [Class = "open-label"]; text("Open full-size sequence ↗")),
                ),
            ),
        ),
        element!(P;
            text("A worker can publish a result only for its current attempt. Mission Control saves the reply, run status and any archive decision together. The reader can close and reopen using the same thread ID. "),
            element!(A [Href = "/design/chat-dispatch"]; text("Existing PostgreSQL dispatch design")),
        ),
    ),
    element!(Section [Id = "contracts", Class = "design-section", AriaLabelledby = "contracts-title"];
        element!(H2 [Id = "contracts-title"]; text("Data access and transfer objects")),
        element!(P;
            text("Implemented destinations share "),
            element!(Code; text("POST /forge/v1/dispatch")),
            text(". The "),
            element!(Code; text("Forge-Destination")),
            text(" header selects an authorized operation. Mission Control derives the owner from the admitted session."),
        ),
        element!(Dl [Class = "storage"];
            element!(Div;
                element!(Dt; element!(Code; text("ide.mail.list"))),
                element!(Dd; text("Inbox or Archive filter and cursor in. Default 20 summaries, maximum 50, with the next cursor. include_metadata opts into the bounded body preview.")),
            ),
            element!(Div;
                element!(Dt; element!(Code; text("ide.mail.open"))),
                element!(Dd; text("Thread ID and after_sequence in. Default 10 messages, maximum 20, with thread revision, status, archive reason, optional remaining recall allowance, and next_sequence. include_metadata adds actual per-message dates.")),
            ),
            element!(Div;
                element!(Dt; element!(Code; text("ide.mail.send"))),
                element!(Dd; text("Client-generated thread ID, subject for a new thread or expected_revision for a follow-up, message, and idempotency key. Return 202 with the saved thread and run IDs after commit. Reusing the same key and content returns the original receipt; changed content conflicts.")),
            ),
            element!(Div;
                element!(Dt; element!(Code; text("ide.mail.retry"))),
                element!(Dd; text("Failed run ID and idempotency key in. Return the new run receipt using the saved inference input, after validating that the thread is still eligible.")),
            ),
            element!(Div;
                element!(Dt; element!(Code; text("ide.mail.archive"))),
                element!(Dd; text("Thread ID and expected_revision in. Manual archive stops future follow-ups but retains messages. Returns mail_archived with result_revision. The same owner, thread, and base revision returns the original receipt without changing timestamps or revision again.")),
            ),
        ),
        element!(P; text("mail_store owns ordered mail_threads and mail_messages reads and writes. queue_store owns turn admission, idempotency, saved inference input, and finalization transactions. The MailRequest / MailResponse Rust DTOs and Python command dataclasses enforce the same four operations. The runner uses its configured mission-control owner; callers cannot supply an owner. The GPU worker receives only the bounded inference payload through the restricted claim API.")),
    ),
    element!(Section [Id = "qualification", Class = "design-section", AriaLabelledby = "qualification-title"];
        element!(H2 [Id = "qualification-title"]; text("Forge qualification")),
        element!(P; text("The committed Mail profile uses 4,096 context tokens, a 1,024-token generation reserve covering reasoning and visible text, a 16-token margin, and a 256-token minimum useful follow-up. A message is also limited to 4,096 UTF-8 bytes, the subject to 120 bytes, and the saved visible reply to 32,768 bytes. These byte limits are independent of token counts.")),
        element!(P; text("The adapter supports configured Mail context sizes up to 8,192, but changing the adapter alone does not change admission: Mission Control and Forge must use the same profile budget and tokenizer. Mail runs have a four-hour queue deadline and up to 30 minutes for local inference. Current code supports prefix-cache replay, lease recovery, duplicate-send protection, and bounded transcript paging; a new hardware/context setting still requires its own measured qualification.")),
        element!(P; text("Sent messages and replies are durable. Unsent Mail drafts live in the current Rust model and the bounded same-tab Home/Design handoff cache. They are not a cross-device draft store; regular reload is a separate leave-guard path.")),
        element!(P;
            text("Source evidence: "),
            element!(A [Href = "https://github.com/roachmaster/homelab/blob/main/deploy/mission-control/agent-runner/mail_tokenizer.py"]; text("profile and tokenizer")),
            text(" · "),
            element!(A [Href = "https://github.com/roachmaster/homelab/blob/main/deploy/mission-control/agent-runner/mail_store.py"]; text("Mail data access")),
            text(" · "),
            element!(A [Href = "https://github.com/roachmaster/homelab/blob/main/src/rust/forge-ide-ui/src/model/mail.rs"]; text("Rust Mail state")),
        ),
    ),
];
