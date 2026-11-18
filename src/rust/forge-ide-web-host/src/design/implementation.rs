use super::{text, Attribute, Node, Tag};

pub(super) const BODY: &[Node] = &[
    Node::Markdown(include_str!(
        "../../../../../docs/design/implementation-status.md"
    )),
    element!(Section [Class = "design-section", AriaLabelledby = "portal-handoff-title"];
        element!(H2 [Id = "portal-handoff-title"]; text("Navigation handoff sequence")),
        element!(Div [Class = "contract-sequences"];
            element!(Figure [Class = "wireframe"];
                element!(Figcaption;
                    element!(H3; text("Docs navigation and restored view intent")),
                    element!(P; text("The configured store persists the accepted checkpoint. The bounded local handoff is a fallback, not authority. Returning rechecks admission before the declared target view runs.")),
                ),
                element!(A [Class = "drawing", Href = "/design/portal-navigation-sequence.svg"];
                    element!(Img [Src = "/design/portal-navigation-sequence.svg", Width = "1050", Height = "1495", Loading = "lazy", Alt = "Checkpointed same-tab docs navigation, admission and state restoration before applying the declared view entry."]),
                    element!(Span [Class = "open-label"]; text("Open full-size sequence →")),
                ),
            ),
        ),
    ),
];
