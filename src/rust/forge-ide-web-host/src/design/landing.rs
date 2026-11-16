use super::{text, Attribute, Node, Tag};

pub(super) const BODY: &[Node] = &[
    element!(Div [Class = "intro"];
        element!(H1; text("Forge IDE design")),
        element!(P; text("Chat and read-only Workbench wireframes, workspace recovery, and the proposed inference dispatch flow.")),
        element!(Nav [Class = "section-nav", AriaLabel = "Design sections"];
            element!(A [Href = "#previous-chats"]; text("Previous chats")),
            element!(A [Href = "#phone"]; text("Phone")),
            element!(A [Href = "#desktop"]; text("Desktop")),
            element!(A [Href = "#data"]; text("Data flow")),
            element!(A [Href = "/design/chat-dispatch"]; text("PostgreSQL dispatch")),
        ),
    ),
    element!(Section [Id = "previous-chats", Class = "design-section", AriaLabelledby = "previous-chats-title"];
        element!(H2 [Id = "previous-chats-title"]; text("Previous chats")),
        element!(P [Class = "previous-chat-intro"]; text("Try the proposed Chat menu with sample conversations. Drafts and changes stay inside this preview.")),
        element!(Div [Class = "previous-chat-layout"];
            element!(Div [Class = "previous-chat-demo", Role = "group", AriaLabel = "Previous chats interactive sample"];
                element!(Div [Class = "previous-chat-toolbar"];
                    element!(Details [Id = "previous-chat-menu", Open = ""];
                        element!(Summary; text("Previous chats")),
                        element!(Div [Class = "previous-chat-list"];
                            element!(Section [AriaLabelledby = "previous-chat-saved-title"];
                                element!(H3 [Id = "previous-chat-saved-title"]; text("Saved chats")),
                                element!(P [Id = "previous-chat-saved-empty", Hidden = ""]; text("No saved chats yet.")),
                                element!(Button [Type = "button", DataPreviewChat = "workbench", AriaCurrent = "true"]; text("Summarize Workbench")),
                                element!(Button [Type = "button", DataPreviewChat = "source"]; text("Explain source verification")),
                                element!(Button [Type = "button", DataPreviewChat = "backup"]; text("Plan a backup")),
                            ),
                            element!(Section [Id = "previous-chat-local", AriaLabelledby = "previous-chat-local-title", Hidden = ""];
                                element!(H3 [Id = "previous-chat-local-title"]; text("This tab")),
                                element!(Div [Id = "previous-chat-local-rows"]),
                            ),
                            element!(Section [AriaLabelledby = "previous-chat-archive-title"];
                                element!(H3 [Id = "previous-chat-archive-title"]; text("Archived runs")),
                                element!(Button [Type = "button", DataPreviewChat = "archive"];
                                    element!(Span; text("Inspect a Rust project")),
                                    element!(Small [AriaHidden = "true"]; text("Snapshot")),
                                ),
                            ),
                        ),
                    ),
                    element!(Button [Type = "button", Id = "previous-chat-new"]; text("New chat")),
                ),
                element!(H3 [Id = "previous-chat-title"]; text("Summarize Workbench")),
                element!(Div [Class = "previous-chat-retention-tools"];
                    element!(P [Id = "previous-chat-retention"]),
                    element!(Details [Id = "previous-chat-manage"];
                        element!(Summary; text("Manage")),
                        element!(Div [Class = "previous-chat-management-actions"];
                            element!(Button [Type = "button", Id = "previous-chat-expire"]; text("Set expiration")),
                            element!(Button [Type = "button", Id = "previous-chat-keep"]; text("Keep long term")),
                            element!(Button [Type = "button", Id = "previous-chat-delete", Class = "previous-chat-danger"]; text("Delete chat")),
                        ),
                    ),
                ),
                element!(Div [Id = "previous-chat-transcript", Role = "region", AriaLabel = "Sample transcript", Tabindex = "0"];
                    element!(P [DataRole = "user"];
                        element!(Strong; text("You")),
                        element!(Span; text("Summarize Workbench.")),
                    ),
                    element!(P [DataRole = "assistant"];
                        element!(Strong; text("Forge")),
                        element!(Span; text("Explorer finds files. The File pane shows verified source, and Outline lists declarations.")),
                    ),
                ),
                element!(Div [Id = "previous-chat-compose"];
                    element!(Label [For = "previous-chat-draft"]; text("Message draft")),
                    element!(Textarea [Id = "previous-chat-draft", Rows = "3"]; text("Add a note about Explorer.")),
                ),
                element!(Div [Id = "previous-chat-archive", Hidden = ""];
                    element!(P; text("Archived run snapshot")),
                    element!(Button [Type = "button", Id = "previous-chat-continue"]; text("Continue as new chat")),
                ),
                element!(P [Id = "previous-chat-status", Role = "status", AriaLive = "polite", AriaAtomic = "true"]),
            ),
            element!(Aside [Class = "previous-chat-brief", AriaLabelledby = "previous-chat-brief-title"];
                element!(H3 [Id = "previous-chat-brief-title"]; text("Menu states")),
                element!(Dl;
                    element!(Div;
                        element!(Dt; text("Empty")),
                        element!(Dd; text("Show “No previous chats yet.” Keep New chat beside the menu.")),
                    ),
                    element!(Div;
                        element!(Dt; text("Loading")),
                        element!(Dd; text("Announce “Loading previous chats…” and keep the open transcript and draft available.")),
                    ),
                    element!(Div;
                        element!(Dt; text("Retry")),
                        element!(Dd; text("Show “Couldn’t load previous chats.” Offer Retry in place and preserve the draft.")),
                    ),
                    element!(Div;
                        element!(Dt; text("Close")),
                        element!(Dd; text("Choose a chat, press Escape, or close the disclosure. Return keyboard focus to Previous chats.")),
                    ),
                ),
                element!(H3; text("Saved chats and archived runs")),
                element!(P; text("A saved chat opens its transcript and this tab’s draft. The draft follows the chat ID when the list order changes.")),
                element!(P; text("An archived run is one submitted exchange snapshot. It opens read-only. Continue as new chat starts a separate conversation, preserving the archived snapshot.")),
                element!(H3 [Class = "previous-chat-retention-heading"]; text("Retention and deletion")),
                element!(P; text("Forge automatically keeps a chat through the UTC date 30 days after its first saved turn. You can change that date or choose Keep long term to clear it. Expiration begins at midnight UTC after the keep-through date.")),
                element!(P; text("Delete asks you to confirm the named saved chat. Archived runs remain read-only.")),
                element!(P;
                    element!(A [Class = "previous-chat-design-link", Href = "/design/chat-history"]; text("Chat history data design")),
                ),
            ),
        ),
        element!(Dialog [Id = "previous-chat-expiration-dialog", Class = "previous-chat-dialog", AriaLabelledby = "previous-chat-expiration-heading", AriaDescribedby = "previous-chat-expiration-help"];
            element!(Form [Id = "previous-chat-expiration-form", Method = "dialog"];
                element!(H3 [Id = "previous-chat-expiration-heading"]; text("Set expiration")),
                element!(P [Id = "previous-chat-expiration-help"]; text("The chat stays saved through the selected date. The preview uses UTC.")),
                element!(Label [For = "previous-chat-expiration-date"]; text("Keep through")),
                element!(Input [Id = "previous-chat-expiration-date", Type = "date", Required = ""]),
                element!(Div [Class = "previous-chat-dialog-actions"];
                    element!(Button [Type = "button", DataHistoryDismiss = ""]; text("Cancel")),
                    element!(Button [Type = "submit"]; text("Save expiration")),
                ),
            ),
        ),
        element!(Dialog [Id = "previous-chat-delete-dialog", Class = "previous-chat-dialog", AriaLabelledby = "previous-chat-delete-heading", AriaDescribedby = "previous-chat-delete-description"];
            element!(H3 [Id = "previous-chat-delete-heading"]; text("Delete chat?")),
            element!(P [Id = "previous-chat-delete-description"];
                text("Remove "),
                element!(Strong [Id = "previous-chat-delete-title"]),
                text(" from the preview’s saved chats."),
            ),
            element!(Div [Class = "previous-chat-dialog-actions"];
                element!(Button [Type = "button", DataHistoryDismiss = "", Autofocus = ""]; text("Cancel")),
                element!(Button [Type = "button", Id = "previous-chat-confirm-delete", Class = "previous-chat-danger"]; text("Delete chat")),
            ),
        ),
    ),
    element!(Section [Id = "phone", Class = "design-section", AriaLabelledby = "phone-title"];
        element!(H2 [Id = "phone-title"]; text("Phone wireframes")),
        element!(Div [Class = "gallery phone-gallery"];
            element!(Figure [Class = "wireframe"];
                element!(Figcaption;
                    element!(H3; text("Chat")),
                    element!(P; text("Switch conversations above the transcript. Replies render Markdown, and the composer stays below.")),
                ),
                element!(A [Class = "drawing", Href = "/design/forge-ide-chat-phone.png", Target = "_blank", Rel = "noopener noreferrer", AriaLabel = "Open the Chat phone wireframe at full size in a new tab"];
                    element!(Img [Src = "/design/forge-ide-chat-phone.png", Width = "390", Height = "664", Alt = "Chat conversation picker, Markdown reply, and message composer"]),
                    element!(Span [Class = "open-label"];
                        text("Open full size "),
                        element!(Span [AriaHidden = "true"]; text("↗")),
                    ),
                ),
            ),
            element!(Figure [Class = "wireframe"];
                element!(Figcaption;
                    element!(H3; text("Workbench")),
                    element!(P; text("One pane at a time. The File header arrow returns to Explorer, and Markdown files offer Raw and Display.")),
                ),
                element!(A [Class = "drawing", Href = "/design/forge-ide-workbench-phone.png", Target = "_blank", Rel = "noopener noreferrer", AriaLabel = "Open the Workbench phone wireframe at full size in a new tab"];
                    element!(Img [Src = "/design/forge-ide-workbench-phone.png", Width = "390", Height = "780", Alt = "Workbench File pane with Back to files arrow, Markdown controls, and numbered source"]),
                    element!(Span [Class = "open-label"];
                        text("Open full size "),
                        element!(Span [AriaHidden = "true"]; text("↗")),
                    ),
                ),
            ),
            element!(Figure [Class = "wireframe"];
                element!(Figcaption;
                    element!(H3; text("Project options")),
                    element!(P; text("Open the disclosure beside View options to choose the node and project. Refresh sits beside Problems.")),
                ),
                element!(A [Class = "drawing", Href = "/design/forge-ide-workbench-project-options.png", Target = "_blank", Rel = "noopener noreferrer", AriaLabel = "Open the Project options phone wireframe at full size in a new tab"];
                    element!(Img [Src = "/design/forge-ide-workbench-project-options.png", Width = "390", Height = "780", Alt = "Open Project options disclosure with Node and Project selectors"]),
                    element!(Span [Class = "open-label"];
                        text("Open full size "),
                        element!(Span [AriaHidden = "true"]; text("↗")),
                    ),
                ),
            ),
        ),
    ),
    element!(Section [Id = "desktop", Class = "design-section", AriaLabelledby = "desktop-title"];
        element!(H2 [Id = "desktop-title"]; text("Desktop wireframes")),
        element!(Div [Class = "gallery desktop-gallery"];
            element!(Figure [Class = "wireframe"];
                element!(Figcaption;
                    element!(H3; text("Chat")),
                    element!(P; text("A centered reading column keeps the conversation picker, transcript, and composer together.")),
                ),
                element!(A [Class = "drawing", Href = "/design/forge-ide-chat-desktop.png", Target = "_blank", Rel = "noopener noreferrer", AriaLabel = "Open the Chat desktop wireframe at full size in a new tab"];
                    element!(Img [Src = "/design/forge-ide-chat-desktop.png", Width = "1440", Height = "1000", Loading = "lazy", Alt = "Desktop Chat with a centered transcript and composer"]),
                    element!(Span [Class = "open-label"];
                        text("Open full size "),
                        element!(Span [AriaHidden = "true"]; text("↗")),
                    ),
                ),
            ),
            element!(Figure [Class = "wireframe"];
                element!(Figcaption;
                    element!(H3; text("Workbench")),
                    element!(P; text("Explorer and Outline flank the read-only File pane. The active node and project appear in the header.")),
                ),
                element!(A [Class = "drawing", Href = "/design/forge-ide-workbench-desktop.png", Target = "_blank", Rel = "noopener noreferrer", AriaLabel = "Open the Workbench desktop wireframe at full size in a new tab"];
                    element!(Img [Src = "/design/forge-ide-workbench-desktop.png", Width = "1440", Height = "1000", Loading = "lazy", Alt = "Desktop Workbench with Explorer, numbered source in the File pane, and Outline"]),
                    element!(Span [Class = "open-label"];
                        text("Open full size "),
                        element!(Span [AriaHidden = "true"]; text("↗")),
                    ),
                ),
            ),
        ),
    ),
    element!(Section [Id = "data", Class = "design-section", AriaLabelledby = "data-title"];
        element!(H2 [Id = "data-title"]; text("Workspace data flow")),
        element!(Ol [Class = "flow", AriaLabel = "Workspace save and restore path"];
            element!(Li;
                element!(Strong; text("Browser")),
                element!(P; text("Keeps a random tab identifier in session storage and requests a saved workspace.")),
            ),
            element!(Li;
                element!(Strong; text("Forge web host")),
                element!(P; text("Checks the active session, request origin, and CSRF token before forwarding.")),
            ),
            element!(Li;
                element!(Strong; text("Agent Runner")),
                element!(P; text("Validates the checkpoint, tab lease, and save revision.")),
            ),
            element!(Li;
                element!(Strong; text("PostgreSQL")),
                element!(P; text("Stores the operator’s Chat history and separate tab checkpoints.")),
            ),
        ),
        element!(Div [Class = "recovery"];
            element!(H3; text("Workspace recovery")),
            element!(Dl [Class = "storage"];
                element!(Div;
                    element!(Dt; text("Shared Chat history")),
                    element!(Dd;
                        element!(Code; text("forge_agent.ide_workspace_history")),
                    ),
                    element!(Dd; text("Completed Chat messages are available when the operator opens Forge in a new tab or browser.")),
                ),
                element!(Div;
                    element!(Dt; text("Tab checkpoint")),
                    element!(Dd;
                        element!(Code; text("forge_agent.ide_workspace_tabs")),
                    ),
                    element!(Dd; text("The original tab recovers its drafts, selected conversation, and Workbench bookmark. A fresh inspection verifies source again.")),
                ),
            ),
            element!(P; text("Older saves cannot replace newer shared history. A competing transcript stays recoverable in the tab that saved it.")),
        ),
    ),
];
