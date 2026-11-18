use super::{text, Attribute, Node, Tag};

pub(super) const BODY: &[Node] = &[
    element!(Section [Id = "previous-chats", Class = "design-section", AriaLabelledby = "previous-chats-title"];
        element!(H1 [Id = "previous-chats-title"]; text("Previous chats")),
        element!(P [Class = "previous-chat-intro"]; text("This earlier menu sandbox demonstrates retention and deletion with sample conversations. The implemented Chat screen uses the thread-title Conversations drawer. Drafts and changes here stay inside the preview.")),
        element!(Div [Class = "previous-chat-layout"];
            element!(Div [Class = "sandbox-stage"];
                element!(Div [Class = "sandbox-toolbar"];
                    element!(Span; text("Interactive preview")),
                    element!(A [Href = "/design/previous-chats", AriaLabel = "Reset preview"]; text("Reset")),
                ),
                element!(Div [Class = "sandbox-content previous-chat-demo", Role = "group", AriaLabel = "Previous chats interactive sample"];
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
            ),
            element!(Aside [Class = "previous-chat-brief", AriaLabelledby = "previous-chat-brief-title"];
                element!(H3 [Id = "previous-chat-brief-title"]; text("Menu states")),
                element!(Table [Class = "state-table", AriaLabel = "Previous chats menu states"];
                    element!(Thead;
                        element!(Tr;
                            element!(Th; text("State")),
                            element!(Th; text("Behavior")),
                        ),
                    ),
                    element!(Tbody;
                        element!(Tr;
                            element!(Th; text("Empty")),
                            element!(Td; text("Show “No previous chats yet.” Keep New chat available.")),
                        ),
                        element!(Tr;
                            element!(Th; text("Loading")),
                            element!(Td; text("Announce loading. Keep the open transcript and draft available.")),
                        ),
                        element!(Tr;
                            element!(Th; text("Retry")),
                            element!(Td; text("Show the load error and Retry in place. Preserve the draft.")),
                        ),
                        element!(Tr;
                            element!(Th; text("Close")),
                            element!(Td; text("Choose a chat, press Escape, or close the menu. Return focus to Previous chats.")),
                        ),
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
];
