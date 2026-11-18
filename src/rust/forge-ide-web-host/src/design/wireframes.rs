use super::{text, Attribute, Node, Tag};

pub(super) const BODY: &[Node] = &[
    element!(Div [Class = "reader-intro"];
        element!(H1; text("IDE wireframes")),
        element!(P; text("Phone and desktop layout references for Chat and Workbench.")),
        element!(P [Class = "wireframe-note"]; text("These light-mode snapshots preserve earlier layouts. The live pages now use compact navigation, conversation drawers, and project sheets. Each snapshot is framed separately from the dark reader.")),
    ),
    element!(Section [Id = "phone", Class = "design-section", AriaLabelledby = "phone-title"];
        element!(H2 [Id = "phone-title"]; text("Phone wireframes")),
        element!(Div [Class = "wireframes gallery phone-gallery"];
            element!(Figure [Class = "wireframe"];
                element!(Figcaption;
                    element!(H3; text("Chat")),
                    element!(P; text("Earlier Chat layout reference. The current Chat page uses a compact thread-title menu, open message stream, and inline composer.")),
                ),
                element!(Div [Class = "wireframe-stage"];
                    element!(A [Class = "drawing", Href = "/design/forge-ide-chat-phone.png", AriaLabel = "Open the Chat phone wireframe at full size"];
                        element!(Img [Src = "/design/forge-ide-chat-phone.png", Width = "390", Height = "664", Alt = "Chat conversation picker, Markdown reply, and message composer"]),
                        element!(Span [Class = "open-label"];
                            text("Open full size "),
                            element!(Span [AriaHidden = "true"]; text("→")),
                        ),
                    ),
                ),
            ),
            element!(Figure [Class = "wireframe"];
                element!(Figcaption;
                    element!(H3; text("Workbench")),
                    element!(P; text("One pane at a time, with an arrow back to Explorer and Raw / Display for Markdown. The live page also has a compact header and unified search.")),
                ),
                element!(Div [Class = "wireframe-stage"];
                    element!(A [Class = "drawing", Href = "/design/forge-ide-workbench-phone.png", AriaLabel = "Open the Workbench phone wireframe at full size"];
                        element!(Img [Src = "/design/forge-ide-workbench-phone.png", Width = "390", Height = "780", Alt = "Workbench File pane with Back to files arrow, Markdown controls, and numbered source"]),
                        element!(Span [Class = "open-label"];
                            text("Open full size "),
                            element!(Span [AriaHidden = "true"]; text("→")),
                        ),
                    ),
                ),
            ),
            element!(Figure [Class = "wireframe"];
                element!(Figcaption;
                    element!(H3; text("Project options")),
                    element!(P; text("Earlier project disclosure reference. The live node/project breadcrumb opens a touch-friendly sheet; layout choices open from the header icon.")),
                ),
                element!(Div [Class = "wireframe-stage"];
                    element!(A [Class = "drawing", Href = "/design/forge-ide-workbench-project-options.png", AriaLabel = "Open the Project options phone wireframe at full size"];
                        element!(Img [Src = "/design/forge-ide-workbench-project-options.png", Width = "390", Height = "780", Alt = "Open Project options disclosure with Node and Project selectors"]),
                        element!(Span [Class = "open-label"];
                            text("Open full size "),
                            element!(Span [AriaHidden = "true"]; text("→")),
                        ),
                    ),
                ),
            ),
        ),
    ),
    element!(Section [Id = "desktop", Class = "design-section", AriaLabelledby = "desktop-title"];
        element!(H2 [Id = "desktop-title"]; text("Desktop wireframes")),
        element!(Div [Class = "wireframes gallery desktop-gallery"];
            element!(Figure [Class = "wireframe"];
                element!(Figcaption;
                    element!(H3; text("Chat")),
                    element!(P; text("A centered conversation column. The current thread-title trigger opens history, keeping management outside the message stream.")),
                ),
                element!(Div [Class = "wireframe-stage"];
                    element!(A [Class = "drawing", Href = "/design/forge-ide-chat-desktop.png", AriaLabel = "Open the Chat desktop wireframe at full size"];
                        element!(Img [Src = "/design/forge-ide-chat-desktop.png", Width = "1440", Height = "1000", Loading = "lazy", Alt = "Desktop Chat with a centered transcript and composer"]),
                        element!(Span [Class = "open-label"];
                            text("Open full size "),
                            element!(Span [AriaHidden = "true"]; text("→")),
                        ),
                    ),
                ),
            ),
            element!(Figure [Class = "wireframe"];
                element!(Figcaption;
                    element!(H3; text("Workbench")),
                    element!(P; text("Explorer and Outline flank the read-only File pane. The active node/project header opens the project sheet; local pane tabs retain workspace context.")),
                ),
                element!(Div [Class = "wireframe-stage"];
                    element!(A [Class = "drawing", Href = "/design/forge-ide-workbench-desktop.png", AriaLabel = "Open the Workbench desktop wireframe at full size"];
                        element!(Img [Src = "/design/forge-ide-workbench-desktop.png", Width = "1440", Height = "1000", Loading = "lazy", Alt = "Desktop Workbench with Explorer, numbered source in the File pane, and Outline"]),
                        element!(Span [Class = "open-label"];
                            text("Open full size "),
                            element!(Span [AriaHidden = "true"]; text("→")),
                        ),
                    ),
                ),
            ),
        ),
    ),
];
