use super::{text, Attribute, Node, Tag};

macro_rules! diagram {
    ($title:literal, $path:literal, $description:literal) => {
        element!(Figure [Class = "wireframe wireframe-stage"];
            element!(Figcaption;
                element!(H3; text($title)),
                element!(P; text($description)),
            ),
            element!(A [Class = "drawing", Href = $path];
                element!(Img [Src = $path, Loading = "lazy", Alt = $description]),
                element!(Span [Class = "open-label"]; text("Open full-size diagram →")),
            ),
        )
    };
}

pub(super) const BODY: &[Node] = &[
    Node::Markdown(include_str!(
        "../../../../../docs/architecture/content-identity.md"
    )),
    element!(Section [Class = "design-section", AriaLabelledby = "content-identity-uml"];
        element!(H2 [Id = "content-identity-uml"]; text("Identity and ownership diagrams")),
        element!(P; text("Source implementation with explicit content-v2 selection. Existing legacy-v1 work is preserved. Git remains the current CI and publication transport; these diagrams do not claim live activation.")),
        element!(Div [Class = "contract-sequences"];
            diagram!("Identity data", "/design/content-identity/data.svg", "Immutable source entries and artifact, policy and generation identities, with Git provenance kept separate."),
            diagram!("Components and provider ownership", "/design/content-identity/components.svg", "Bootstrap injects ready source providers; frozen durable identities gate scoped review, CI and publication."),
            diagram!("Native review and resume", "/design/content-identity/native.svg", "Persist or validate the selected identity snapshot before checks; preserve scoped review and exact-candidate publication receipts."),
            diagram!("External author and independent reviewer", "/design/content-identity/external.svg", "Trusted quarantine acquisition freezes content identity before delivery; an independent reviewer checks content and workflow authority before exact-candidate CI."),
        ),
    ),
];
