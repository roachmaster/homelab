use super::{text, Attribute, Node, Tag};

pub(super) const BODY: &[Node] = &[
    element!(Section [Id = "data", Class = "design-section", AriaLabelledby = "data-title"];
        element!(H1 [Id = "data-title"]; text("Workspace data flow")),
        element!(P; text("How Forge saves shared conversation history and recovers this tab’s working context.")),
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
