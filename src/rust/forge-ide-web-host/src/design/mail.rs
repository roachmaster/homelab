use super::{text, Attribute, Node, Tag};

pub(super) const BODY: &[Node] = &[
    element!(Div [Class = "intro"];
        element!(H1; text("Forge Mail design")),
        element!(P; text("Proposed workspace for drafting requests to GPT-OSS 120B, returning for replies and reviewing complete threads after their recall limit.")),
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
        element!(Div [Class = "gallery contract-sequences"];
            element!(Figure [Class = "wireframe"];
                element!(Figcaption; element!(H3; text("Inbox, compose, thread and archive"))),
                element!(A [Class = "drawing", Href = "/design/forge-ide-mail-wireframe.svg"];
                    element!(Img [Src = "/design/forge-ide-mail-wireframe.png", Width = "1640", Height = "1420", Loading = "lazy", Alt = "Proposed Forge Mail screens showing inbox statuses, a new message, follow-up and read-only archive."]),
                    element!(Span [Class = "open-label"]; text("Open full-size wireframe ↗")),
                ),
            ),
        ),
        element!(Ol [Class = "contract-steps"];
            element!(Li; text("Open Mail from the IDE navigation. Inbox shows one row per thread with its subject, current status and last activity. Archive opens read-only threads. Desktop can show the list and reader side by side. On a phone, the reader opens on its own with a back-to-list control.")),
            element!(Li; text("Choose New message, enter a subject and message, and select Send. The composer identifies GPT-OSS 120B as the recipient. Once accepted, the saved thread shows Queued, Waiting for Forge, Running, Reply ready or Failed. You can close the tab and return to the same thread.")),
            element!(Li; text("Read the rendered Markdown reply and send a follow-up in the same thread. One reply can be pending per thread. A failed run shows a retry action for the saved message. Automatic status changes preserve focus and any draft.")),
            element!(Li; text("Archive shows the full transcript and the reason it closed. New message opens a fresh thread. An unsent draft can be carried over at your direction.")),
        ),
    ),
    element!(Section [Id = "recall", Class = "design-section", AriaLabelledby = "recall-title"];
        element!(H2 [Id = "recall-title"]; text("Recall and automatic archive")),
        element!(P; text("PostgreSQL stores the ordered user messages and visible assistant replies. For each run, Forge receives the completed exchanges and the current message, in order. Paging the thread in the browser only changes what you see. It leaves the prompt history intact.")),
        element!(P; text("Pin the model artifact, Harmony template and tokenizer to the thread profile. Size the fully rendered prompt on Mission Control, so sending works while Forge sleeps. Reserve generation tokens for both reasoning and the visible reply, plus a small operating margin. Forge verifies the same profile before inference.")),
        element!(P [Class = "contract-note"];
            text("Archive after a saved reply when the existing transcript leaves less than the profile’s minimum useful follow-up allowance, with the next reply reserve included. Keep every saved message. If a large draft alone exceeds the budget while a shorter follow-up can still fit, keep the thread open and the draft editable."),
        ),
        element!(P; text("Warn beside the composer as room gets low. On archive, replace Follow up with the reason and New message. Every completed exchange stays in the prompt until the thread closes. Earlier failed submissions stay visible in the transcript. Retry uses the original run input and appends at most one completed reply.")),
        element!(P;
            text("A loaded model can reuse the unchanged prefix through llama.cpp’s prompt KV cache. When the cache is cold, Forge rebuilds the prompt from the saved text. Measure warm versus cold replay before considering local KV snapshots. "),
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
                    element!(Img [Src = "/design/mail-dispatch-sequence.svg", Width = "921", Height = "1778", Loading = "lazy", Alt = "Mission Control commits a job in PostgreSQL and returns acceptance. Forge claims it, runs GPT-OSS 120B and publishes the reply. Mission Control saves the reply and decides whether to archive."]),
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
            text("Proposed destinations share "),
            element!(Code; text("POST /forge/v1/dispatch")),
            text(". The "),
            element!(Code; text("Forge-Destination")),
            text(" header selects an authorized operation. Mission Control derives the owner from the admitted session."),
        ),
        element!(Dl [Class = "storage"];
            element!(Div;
                element!(Dt; element!(Code; text("ide.mail.list"))),
                element!(Dd; text("Inbox or Archive filter and cursor in. Thread summaries, statuses and next cursor out.")),
            ),
            element!(Div;
                element!(Dt; element!(Code; text("ide.mail.open"))),
                element!(Dd; text("Thread ID and message cursor in. Thread revision, status, archive reason, remaining recall allowance, ordered message page and next cursor out.")),
            ),
            element!(Div;
                element!(Dt; element!(Code; text("ide.mail.send"))),
                element!(Dd; text("Subject for a new thread, or thread ID and expected revision for a follow-up. Include the message and an idempotency key. Return 202 with the saved thread and run IDs after commit. Reusing the key returns the original receipt.")),
            ),
            element!(Div;
                element!(Dt; element!(Code; text("ide.mail.retry"))),
                element!(Dd; text("Failed run ID and idempotency key in. Return the new run receipt using the saved inference input, after validating that the thread is still eligible.")),
            ),
        ),
        element!(P; text("The Mail DAO owns ordered thread messages, model profile, revision and archive metadata. Lock the thread while admitting a turn and associate the run with a fixed message boundary. Finalization adds the visible reply once, then updates recall and archive state in the same transaction. The GPU worker receives the bounded inference payload through the existing claim contract.")),
    ),
    element!(Section [Id = "qualification", Class = "design-section", AriaLabelledby = "qualification-title"];
        element!(H2 [Id = "qualification-title"]; text("Forge qualification")),
        element!(P; text("The prior hardware trial used a 1,024-token context, a 4 GiB expert cache and NVMe-backed model weights. It completed two short prompts, with 60 prompt tokens reused on the second. That trial establishes a starting configuration for testing.")),
        element!(P; text("Qualify 2,048 context tokens with a 768-token generation reserve as a first test, then try 4,096 and 8,192 with longer replies. The initial profile leaves at most 1,280 tokens for the input and its template. Choose the release profile after a long answer and several meaningful follow-ups pass under paging. Run a cold-cache replay, a restart during a queued job, a duplicate Send, and an oversized draft near the archive boundary. Verify that the entire closed thread can be read on a second device.")),
    ),
];
