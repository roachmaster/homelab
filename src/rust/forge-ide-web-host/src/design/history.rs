use super::{text, Attribute, Node, Tag};

pub(super) const BODY: &[Node] = &[
    element!(Div [Class = "intro"];
        element!(H1; text("Previous chats technical design")),
        element!(P; text("Contracts for opening, retaining, deleting, and expiring saved chats through Forge dispatch.")),
        element!(Nav [Class = "section-nav", AriaLabel = "Technical design sections"];
            element!(A [Href = "#dispatch"]; text("Dispatch")),
            element!(A [Href = "#dto"]; text("DTOs")),
            element!(A [Href = "#dao"]; text("DAO")),
            element!(A [Href = "#sequences"]; text("Sequence diagrams")),
            element!(A [Href = "/design/chat-dispatch"]; text("PostgreSQL dispatch")),
        ),
    ),
    element!(Section [Id = "dispatch", Class = "design-section", AriaLabelledby = "dispatch-title"];
        element!(H2 [Id = "dispatch-title"]; text("Dispatch and admission")),
        element!(Ol [Class = "flow"];
            element!(Li;
                element!(H3; text("Browser → Forge web host")),
                element!(P;
                    text("Send "),
                    element!(Code; text("application/json")),
                    text(" to "),
                    element!(Code; text("POST /forge/v1/dispatch")),
                    text(" through the existing access-controlled origin. "),
                    element!(Code; text("Forge-Destination")),
                    text(" identifies the operation. The host checks the configured "),
                    element!(Code; text("Host")),
                    text(" and "),
                    element!(Code; text("Origin")),
                    text(", the "),
                    element!(Code; text("forge_ui")),
                    text(" session cookie, "),
                    element!(Code; text("Forge-CSRF")),
                    text(", and "),
                    element!(Code; text("Forge-Request-Id")),
                    text(". Both "),
                    element!(Code; text("Forge-Envelope-Version")),
                    text(" and "),
                    element!(Code; text("Forge-Contract-Version")),
                    text(" are "),
                    element!(Code; text("1")),
                    text(". The typed body must match its destination and request ID."),
                ),
            ),
            element!(Li;
                element!(H3; text("Forge web host → Forge runner")),
                element!(P;
                    text("After admission, the host sends the validated request to the existing runner’s private "),
                    element!(Code; text("POST /v1/dispatch")),
                    text(". It uses the same destination, versions and request ID with the runner’s bearer credential. The host keeps browser cookies and CSRF values at the browser boundary. The runner derives the operator from its admitted credential and selects the registered handler."),
                ),
            ),
            element!(Li;
                element!(H3; text("Forge runner → PostgreSQL")),
                element!(P;
                    text("The runner invokes an operator-scoped DAO operation and returns a bounded, typed result. The browser uses the echoed "),
                    element!(Code; text("request_id")),
                    text(" to match the result to the action that initiated it."),
                ),
            ),
        ),
        element!(P [Class = "contract-note"];
            text("Workspace recovery uses "),
            element!(Code; text("POST /forge/v1/dispatch/workspace")),
            text(", which relays to "),
            element!(Code; text("POST /v1/workspace")),
            text(" when durable recovery is enabled. The tab checkpoint stores draft and selection state by chat ID. A linked inference commits the saved transcript in the runner."),
        ),
    ),
    element!(Section [Id = "dto", Class = "design-section", AriaLabelledby = "dto-title"];
        element!(H2 [Id = "dto-title"]; text("Data transfer objects")),
        element!(P;
            text("The examples use sample chat data. Each response event is wrapped in the Forge envelope "),
            element!(Code; text("{\"protocol_version\": 1, \"event\": …}")),
            text("."),
        ),
        element!(Dl [Class = "storage"];
            element!(Div;
                element!(Dt;
                    element!(Code; text("ChatSummaryDto")),
                ),
                element!(Dd;
                    element!(Code; text("chat_id")),
                    text(" is a stable opaque UUID. "),
                    element!(Code; text("title")),
                    text(" is the display label. "),
                    element!(Code; text("updated_at")),
                    text(" is the UTC time of the most recent committed turn. "),
                    element!(Code; text("revision")),
                    text(" changes with each committed chat mutation, including a retention change. "),
                    element!(Code; text("retention")),
                    text(" is a "),
                    element!(Code; text("RetentionPolicyDto")),
                    text("."),
                ),
            ),
            element!(Div;
                element!(Dt;
                    element!(Code; text("RetentionPolicyDto")),
                ),
                element!(Dd;
                    text("The default is "),
                    element!(Code; text("expires")),
                    text(", with an inclusive "),
                    element!(Code; text("expires_on")),
                    text(" date 30 days after the first saved turn in UTC. "),
                    element!(Code; text("{\"kind\":\"long_term\"}")),
                    text(" is an explicit choice that clears the date. For custom dates, the runner accepts an IANA "),
                    element!(Code; text("time_zone")),
                    text(" and uses UTC when omitted. It returns the derived "),
                    element!(Code; text("expires_at")),
                    text(" UTC instant when the next local calendar day begins."),
                ),
            ),
            element!(Div;
                element!(Dt;
                    element!(Code; text("ChatMessageDto")),
                ),
                element!(Dd;
                    element!(Code; text("position")),
                    text(", "),
                    element!(Code; text("role")),
                    text(", and "),
                    element!(Code; text("content")),
                    text(" represent a committed message in transcript order. This tab’s draft is restored separately by stable chat ID."),
                ),
            ),
        ),
        element!(Div [Class = "contract-examples"];
            element!(Section [Class = "contract-example", AriaLabelledby = "list-dto-title"];
                element!(H3 [Id = "list-dto-title"]; text("List saved chats")),
                element!(P;
                    element!(Code; text("Forge-Destination: ide.chat.history.list")),
                ),
                element!(H4;
                    element!(Code; text("ChatHistoryListRequest")),
                ),
                element!(Pre [Class = "contract-code"];
                    element!(Code; text("{\n  \"kind\": \"chat_history_list\",\n  \"request_id\": \"list-1\",\n  \"limit\": 20,\n  \"cursor\": null\n}")),
                ),
                element!(H4; text("Response event")),
                element!(Pre [Class = "contract-code"];
                    element!(Code; text("{\n  \"kind\": \"chat_history_listed\",\n  \"request_id\": \"list-1\",\n  \"items\": [{\n    \"chat_id\": \"4fb285b2-5a53-4caa-90b6-d0aff012c0a4\",\n    \"title\": \"Explain source verification\",\n    \"updated_at\": \"2026-11-16T10:00:00Z\",\n    \"revision\": 7,\n    \"retention\": {\n      \"kind\": \"expires\",\n      \"expires_on\": \"2026-12-16\",\n      \"time_zone\": \"UTC\",\n      \"expires_at\": \"2026-12-17T00:00:00Z\"\n    }\n  }],\n  \"next_cursor\": null\n}")),
                ),
                element!(P;
                    text("The runner limits the page size. An opaque cursor continues the ordered list. "),
                    element!(Code; text("items: []")),
                    text(" is a successful empty result. Draft-only chats stay in this tab until the first committed turn."),
                ),
            ),
            element!(Section [Class = "contract-example", AriaLabelledby = "open-dto-title"];
                element!(H3 [Id = "open-dto-title"]; text("Open a saved chat")),
                element!(P;
                    element!(Code; text("Forge-Destination: ide.chat.history.open")),
                ),
                element!(H4;
                    element!(Code; text("ChatHistoryOpenRequest")),
                ),
                element!(Pre [Class = "contract-code"];
                    element!(Code; text("{\n  \"kind\": \"chat_history_open\",\n  \"request_id\": \"open-1\",\n  \"chat_id\": \"4fb285b2-5a53-4caa-90b6-d0aff012c0a4\"\n}")),
                ),
                element!(H4; text("Response event")),
                element!(Pre [Class = "contract-code"];
                    element!(Code; text("{\n  \"kind\": \"chat_history_opened\",\n  \"request_id\": \"open-1\",\n  \"chat\": {\n    \"summary\": {\n      \"chat_id\": \"4fb285b2-5a53-4caa-90b6-d0aff012c0a4\",\n      \"title\": \"Explain source verification\",\n      \"updated_at\": \"2026-11-16T10:00:00Z\",\n      \"revision\": 7,\n      \"retention\": {\n        \"kind\": \"expires\",\n        \"expires_on\": \"2026-12-16\",\n        \"time_zone\": \"UTC\",\n        \"expires_at\": \"2026-12-17T00:00:00Z\"\n      }\n    },\n    \"messages\": [\n      {\"position\": 1, \"role\": \"user\", \"content\": \"How is source verified?\"},\n      {\"position\": 2, \"role\": \"assistant\", \"content\": \"The host checks the source against the accepted inspection.\"}\n    ]\n  }\n}")),
                ),
                element!(P; text("Opening reads the saved transcript. An explicit Send starts the next turn.")),
            ),
            element!(Section [Class = "contract-example", AriaLabelledby = "retention-dto-title"];
                element!(H3 [Id = "retention-dto-title"]; text("Set retention")),
                element!(P;
                    element!(Code; text("Forge-Destination: ide.chat.history.retention.set")),
                ),
                element!(H4;
                    element!(Code; text("RetentionSetRequest")),
                ),
                element!(Pre [Class = "contract-code"];
                    element!(Code; text("{\n  \"kind\": \"chat_history_retention_set\",\n  \"request_id\": \"retention-1\",\n  \"chat_id\": \"4fb285b2-5a53-4caa-90b6-d0aff012c0a4\",\n  \"expected_revision\": 7,\n  \"policy\": {\n    \"kind\": \"expires\",\n    \"expires_on\": \"2026-11-30\",\n    \"time_zone\": \"UTC\"\n  }\n}")),
                ),
                element!(H4; text("Response event")),
                element!(Pre [Class = "contract-code"];
                    element!(Code; text("{\n  \"kind\": \"chat_history_retention_set\",\n  \"request_id\": \"retention-1\",\n  \"chat_id\": \"4fb285b2-5a53-4caa-90b6-d0aff012c0a4\",\n  \"revision\": 8,\n  \"retention\": {\n    \"kind\": \"expires\",\n    \"expires_on\": \"2026-11-30\",\n    \"time_zone\": \"UTC\",\n    \"expires_at\": \"2026-12-01T00:00:00Z\"\n  }\n}")),
                ),
                element!(P;
                    text("The request supplies the calendar date. The runner derives "),
                    element!(Code; text("expires_at")),
                    text(" using the next calendar day in the resolved zone. Setting "),
                    element!(Code; text("long_term")),
                    text(" removes a previously scheduled expiry."),
                ),
            ),
            element!(Section [Class = "contract-example", AriaLabelledby = "delete-dto-title"];
                element!(H3 [Id = "delete-dto-title"]; text("Delete a saved chat")),
                element!(P;
                    element!(Code; text("Forge-Destination: ide.chat.history.delete")),
                ),
                element!(H4;
                    element!(Code; text("DeleteChatRequest")),
                ),
                element!(Pre [Class = "contract-code"];
                    element!(Code; text("{\n  \"kind\": \"chat_history_delete\",\n  \"request_id\": \"delete-1\",\n  \"chat_id\": \"4fb285b2-5a53-4caa-90b6-d0aff012c0a4\",\n  \"expected_revision\": 8\n}")),
                ),
                element!(H4; text("Response event")),
                element!(Pre [Class = "contract-code"];
                    element!(Code; text("{\n  \"kind\": \"chat_history_deleted\",\n  \"request_id\": \"delete-1\",\n  \"chat_id\": \"4fb285b2-5a53-4caa-90b6-d0aff012c0a4\",\n  \"revision\": 9\n}")),
                ),
                element!(P; text("The interface confirms deletion before sending. On success, it removes the chat and the draft keyed to that chat.")),
            ),
        ),
        element!(P [Class = "contract-note"];
            element!(Code; text("409")),
            text(" means the expected revision is stale. Refresh the saved version before attempting another change. "),
            element!(Code; text("404")),
            text(" covers missing, expired, deleted, and out-of-scope chat IDs. An admitted operation’s error result echoes "),
            element!(Code; text("request_id")),
            text(" with a typed code such as "),
            element!(Code; text("{\"kind\":\"chat_history_failed\",\"request_id\":\"delete-1\",\"code\":\"revision_conflict\"}")),
            text(". A failed open preserves the active transcript and draft."),
        ),
    ),
    element!(Section [Id = "dao", Class = "design-section", AriaLabelledby = "dao-title"];
        element!(H2 [Id = "dao-title"]; text("PostgreSQL data access")),
        element!(P;
            element!(Code; text("ChatHistoryDao")),
            text(" belongs to the Forge runner. Every operation takes the operator scope established at admission. Stable chat IDs connect committed messages, retention, and authorized run links. Tab selections and drafts refer to those IDs."),
        ),
        element!(Dl [Class = "storage"];
            element!(Div;
                element!(Dt;
                    element!(Code; text("commit_transcript(owner, chat_id, messages)")),
                ),
                element!(Dd; text("Save the transcript with the default expiration on its first committed turn. Subsequent turns retain the existing policy.")),
            ),
            element!(Div;
                element!(Dt;
                    element!(Code; text("list(owner, limit, cursor)")),
                ),
                element!(Dd; text("Read available summaries in descending last-committed-turn time, with the stable ID as the cursor tie-breaker. Omit transcripts, drafts, tombstones, and already-due chats.")),
            ),
            element!(Div;
                element!(Dt;
                    element!(Code; text("get(owner, chat_id)")),
                ),
                element!(Dd; text("Read the summary and ordered committed messages from a consistent snapshot. Apply the same ownership and expiry predicates as the list.")),
            ),
            element!(Div;
                element!(Dt;
                    element!(Code; text("set_retention(owner, chat_id, expected_revision, policy)")),
                ),
                element!(Dd; text("Lock the chat row, verify its revision and availability, and persist the resolved policy with the next revision in one transaction.")),
            ),
            element!(Div;
                element!(Dt;
                    element!(Code; text("delete(owner, chat_id, expected_revision)")),
                ),
                element!(Dd; text("Lock the chat row, compare the revision, and commit the tombstone and content removal together. Preserve the ID, revision, and removal reason so stale writers cannot recreate it.")),
            ),
            element!(Div;
                element!(Dt;
                    element!(Code; text("expire_due(now, batch_limit)")),
                ),
                element!(Dd; text("Run bounded batches in the existing runner scheduler. Recheck the policy, due instant, and candidate revision under the row lock, then use the same removal transaction as manual deletion. A policy change committed before that check takes effect.")),
            ),
        ),
        element!(P [Class = "contract-note"]; text("The removal transaction covers the committed transcript, chat-owned linked run content, and recoverable checkpoint copies. Use explicit chat ownership links to determine which records belong to the chat. Check the tombstone at checkpoint save, restore, and late run completion so an open tab or a finishing request cannot revive the chat.")),
        element!(P [Class = "contract-note"]; text("The runner imports shared workspace transcripts as saved chats. Separate copies in older tabs, unlinked runner snapshots, and existing backups follow their existing retention scope.")),
    ),
    element!(Section [Id = "sequences", Class = "design-section", AriaLabelledby = "sequence-title"];
        element!(H2 [Id = "sequence-title"]; text("Sequence diagrams")),
        element!(Div [Class = "gallery contract-sequences"];
            element!(Figure [Class = "wireframe"];
                element!(Figcaption;
                    element!(H3; text("Open a saved chat")),
                ),
                element!(A [Class = "drawing", Href = "/design/chat-history-open.png"];
                    element!(Img [Src = "/design/chat-history-open.png", Width = "1449", Height = "1608", Loading = "lazy", Alt = "Saved chat list and open requests pass through Forge dispatch to the runner’s PostgreSQL reads."]),
                    element!(Span [Class = "open-label"];
                        text("Open full-size diagram "),
                        element!(Span [AriaHidden = "true"]; text("↗")),
                    ),
                ),
                element!(Ol [Class = "contract-steps"];
                    element!(Li; text("The browser requests summaries and selects a chat ID.")),
                    element!(Li; text("The web host admits the open request and forwards it to the Forge runner.")),
                    element!(Li; text("The runner reads the available transcript and sends the typed result. The browser opens it and restores this tab’s draft.")),
                ),
            ),
            element!(Figure [Class = "wireframe"];
                element!(Figcaption;
                    element!(H3; text("Set retention")),
                ),
                element!(A [Class = "drawing", Href = "/design/chat-history-retention.png"];
                    element!(Img [Src = "/design/chat-history-retention.png", Width = "1590", Height = "1544", Loading = "lazy", Alt = "A retention choice flows through Forge dispatch to a revision-checked PostgreSQL update."]),
                    element!(Span [Class = "open-label"];
                        text("Open full-size diagram "),
                        element!(Span [AriaHidden = "true"]; text("↗")),
                    ),
                ),
                element!(Ol [Class = "contract-steps"];
                    element!(Li; text("The browser submits the selected policy with the displayed revision.")),
                    element!(Li; text("The web host admits and forwards the typed request.")),
                    element!(Li; text("The runner checks the revision, derives an expiry for a dated policy, and commits. The browser displays the returned policy.")),
                ),
            ),
            element!(Figure [Class = "wireframe"];
                element!(Figcaption;
                    element!(H3; text("Delete a saved chat")),
                ),
                element!(A [Class = "drawing", Href = "/design/chat-history-delete.png"];
                    element!(Img [Src = "/design/chat-history-delete.png", Width = "1564", Height = "1501", Loading = "lazy", Alt = "Confirmed deletion goes through Forge dispatch to a PostgreSQL transaction that removes chat content and records its tombstone."]),
                    element!(Span [Class = "open-label"];
                        text("Open full-size diagram "),
                        element!(Span [AriaHidden = "true"]; text("↗")),
                    ),
                ),
                element!(Ol [Class = "contract-steps"];
                    element!(Li; text("The browser confirms the chat ID and sends the delete request with the displayed revision.")),
                    element!(Li; text("The web host admits the request. The runner checks ownership and revision under lock.")),
                    element!(Li; text("PostgreSQL commits the removal and tombstone. The success event lets the browser remove the chat.")),
                ),
            ),
            element!(Figure [Class = "wireframe"];
                element!(Figcaption;
                    element!(H3; text("Expire due chats")),
                ),
                element!(A [Class = "drawing", Href = "/design/chat-history-expiry.png"];
                    element!(Img [Src = "/design/chat-history-expiry.png", Width = "1202", Height = "1120", Loading = "lazy", Alt = "The Forge runner scheduler checks due chat policies and applies the removal transaction in PostgreSQL."]),
                    element!(Span [Class = "open-label"];
                        text("Open full-size diagram "),
                        element!(Span [AriaHidden = "true"]; text("↗")),
                    ),
                ),
                element!(Ol [Class = "contract-steps"];
                    element!(Li; text("The runner scheduler starts a bounded batch and finds due chats.")),
                    element!(Li; text("The DAO locks each eligible chat and rechecks its persisted expiry before removing linked content and writing a tombstone.")),
                    element!(Li;
                        text("Subsequent browser lists omit those chats. An open request for an expired chat returns "),
                        element!(Code; text("404")),
                        text("."),
                    ),
                ),
            ),
        ),
    ),
];
