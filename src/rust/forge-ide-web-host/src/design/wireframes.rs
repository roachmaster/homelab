use super::{text, Attribute, Node, Tag};

macro_rules! capture {
    ($title:literal, $description:literal, $src:literal, $width:literal, $height:literal) => {
        element!(Figure [Class = "wireframe"];
            element!(Figcaption;
                element!(H3; text($title)),
                element!(P; text($description)),
            ),
            element!(Div [Class = "wireframe-stage"];
                element!(A [Class = "drawing", Href = $src];
                    element!(Img [Src = $src, Width = $width, Height = $height, Loading = "lazy", Alt = $description]),
                    element!(Span [Class = "open-label"]; text("Open full size "), element!(Span [AriaHidden = "true"]; text("→"))),
                ),
            ),
        )
    };
}

pub(super) const BODY: &[Node] = &[
    element!(Div [Class = "reader-intro"];
        element!(H1; text("IDE wireframes")),
        element!(P; text("Historical Chat and Mail captures, plus Workbench layout references. Chat and Mail are inactive in the live portal; retained images are design history.")),
        element!(P [Class = "wireframe-note"]; text("Captured from the generated UI in disposable acceptance fixtures on 18 November 2026. Editable controls are masked. Some fixtures intentionally disable Mail or contain partial reports and long strings. Historical light schematics remain in the repository as archived design references.")),
    ),
    element!(Section [Id = "phone", Class = "design-section", AriaLabelledby = "phone-title"];
        element!(H2 [Id = "phone-title"]; text("Phone wireframes")),
        element!(Div [Class = "wireframes gallery phone-gallery"];
                capture!("Chat", "Compact header, thread-title drawer trigger, open message stream and inline composer.", "/design/forge-ide-chat-phone.png", "390", "664"),
                capture!("Conversations", "Saved history and retention management share one drawer; editable fields are masked in this fixture.", "/design/forge-ide-chat-history-phone.png", "390", "664"),
                capture!("Workbench File", "Verified source with declaration highlight, Wrap, supporting details and the contextual prompt dock. Partial status is explicit.", "/design/forge-ide-workbench-phone.png", "390", "664"),
                capture!("Workbench Explorer", "One search field, inline entry-name mode, safe dense file rows and bounded local paging. This fixture includes adversarial names.", "/design/forge-ide-workbench-explorer-phone.png", "390", "664"),
                capture!("Project options", "The header breadcrumb opens Node and Project in a native phone sheet. Escape closes it and restores focus.", "/design/forge-ide-workbench-project-options.png", "375", "667"),
                capture!("Mail inbox", "Sender, supplied date and body preview sit in divider rows. Compose is a floating action; Reply ready is generation state.", "/design/forge-ide-mail-inbox-phone.png", "390", "720"),
                capture!("Mail thread", "Message headers, recipient disclosures, safe Markdown and a docked Reply trigger. The long subject deliberately tests wrapping.", "/design/forge-ide-mail-thread-phone.png", "390", "844"),
        ),
    ),
    element!(Section [Id = "desktop", Class = "design-section", AriaLabelledby = "desktop-title"];
        element!(H2 [Id = "desktop-title"]; text("Desktop wireframes")),
        element!(Div [Class = "wireframes gallery desktop-gallery"];
                capture!("Chat", "A centered conversation with compact navigation; the title opens the history drawer.", "/design/forge-ide-chat-desktop.png", "1440", "1000"),
                capture!("Workbench", "Explorer and source remain primary; optional tools and verified context retain their own ownership.", "/design/forge-ide-workbench-desktop.png", "1440", "1000"),
                capture!("Mail", "Desktop list-and-reader layout for bounded correspondence; private operator data is not used.", "/design/forge-ide-mail-thread-desktop.png", "1440", "1000"),
        ),
    ),
];
