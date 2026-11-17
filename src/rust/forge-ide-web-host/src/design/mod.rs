//! Rust declarations for the portal's reference and prototype pages.
mod markup;
use markup::escape;
use std::fmt::Write;

macro_rules! html_names {
    ($kind:ident { $($variant:ident => $literal:literal),+ $(,)? }) => {
        #[derive(Clone, Copy)]
        enum $kind { $($variant),+ }

        impl $kind {
            fn name(self) -> &'static str {
                match self { $(Self::$variant => $literal),+ }
            }
        }
    };
}

html_names!(Tag {
    A => "a", Article => "article", Aside => "aside", Button => "button", Canvas => "canvas", Code => "code",
    Dd => "dd", Details => "details", Dialog => "dialog", Div => "div",
    Dl => "dl", Dt => "dt", Figcaption => "figcaption", Figure => "figure",
    Form => "form", H1 => "h1", H2 => "h2", H3 => "h3", H4 => "h4",
    Img => "img", Input => "input", Label => "label", Li => "li", Nav => "nav",
    Ol => "ol", P => "p", Pre => "pre", Section => "section", Small => "small",
    Span => "span", Strong => "strong", Summary => "summary", Table => "table", Textarea => "textarea",
});

html_names!(Attribute {
    Alt => "alt", AriaAtomic => "aria-atomic", AriaCurrent => "aria-current",
    AriaDescribedby => "aria-describedby", AriaHidden => "aria-hidden", AriaLabel => "aria-label",
    AriaLabelledby => "aria-labelledby", AriaLive => "aria-live", Autofocus => "autofocus",
    Class => "class", DataHistoryDismiss => "data-history-dismiss",
    DataPreviewChat => "data-preview-chat", DataRole => "data-role",
    DataPlaytestAction => "data-playtest-action", Disabled => "disabled",
    For => "for", Height => "height", Hidden => "hidden", Href => "href",
    Id => "id", Loading => "loading", Method => "method", Open => "open",
    Rel => "rel", Required => "required", Role => "role", Rows => "rows",
    Src => "src", Tabindex => "tabindex", Target => "target", Type => "type", Width => "width",
});

enum Node {
    Text(&'static str),
    Element(Tag, &'static [(Attribute, &'static str)], &'static [Node]),
    GameEngineReport,
}

const fn text(value: &'static str) -> Node {
    Node::Text(value)
}

macro_rules! element {
    ($tag:ident $([$($key:ident = $value:literal),* $(,)?])? $(; $($child:expr),* $(,)?)?) => {
        Node::Element(Tag::$tag, &[$($( (Attribute::$key, $value) ),*)?], &[$($($child),*)?])
    };
}

mod dispatch;
pub mod game_engine;
mod history;
mod landing;
mod mail;
pub mod playtest;
pub mod playtest_browser;

fn render(nodes: &[Node], output: &mut String) {
    for node in nodes {
        match node {
            Node::Text(value) => output.push_str(&escape(value)),
            Node::Element(tag, attributes, children) => {
                render_element(*tag, attributes, output, |out| render(children, out));
            }
            Node::GameEngineReport => game_engine::render_report(output),
        }
    }
}

fn render_element(
    tag: Tag,
    attributes: &[(Attribute, &str)],
    output: &mut String,
    body: impl FnOnce(&mut String),
) {
    let name = tag.name();
    let _ = write!(output, "<{name}");
    for (attribute, value) in attributes {
        let _ = write!(output, " {}=\"{}\"", attribute.name(), escape(value));
    }
    output.push('>');
    if !matches!(tag, Tag::Img | Tag::Input) {
        body(output);
        let _ = write!(output, "</{name}>");
    }
}

pub enum DesignPage {
    Overview,
    ChatHistory,
    ChatDispatch,
    Mail,
    GameStudio,
    Playtest,
}

impl DesignPage {
    /// Render a complete document using the shared portal design shell.
    pub fn render(self) -> String {
        let (title, back_href, back_label, body) = match self {
            Self::Overview => ("Forge IDE design", "/", "Back to Forge", landing::BODY),
            Self::ChatHistory => (
                "Previous chats technical design · Forge IDE",
                "/design#previous-chats",
                "Back to Previous chats",
                history::BODY,
            ),
            Self::ChatDispatch => (
                "PostgreSQL event dispatch · Forge IDE",
                "/design",
                "Back to Design",
                dispatch::BODY,
            ),
            Self::Mail => (
                "Mail design · Forge IDE",
                "/design",
                "Back to Design",
                mail::BODY,
            ),
            Self::GameStudio => (
                "Extending Homelab with a Rust Game Engine · Forge IDE",
                "/design",
                "Back to Design",
                game_engine::BODY,
            ),
            Self::Playtest => (
                "Room One · Forge Playtest",
                "/design/game-studio",
                "Back to game engine design",
                playtest::BODY,
            ),
        };
        let mut output = String::from(
            "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
             <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">",
        );
        let _ = write!(
            output,
            "<title>{}</title><link rel=\"stylesheet\" href=\"/design/style.css\">",
            escape(title),
        );
        output.push_str(match self {
            Self::Overview => "<script src=\"/design/previous-chats.js\" defer></script>",
            Self::GameStudio => "<link rel=\"stylesheet\" href=\"/design/game-studio.css\">",
            Self::Playtest => concat!(
                "<link rel=\"stylesheet\" href=\"/playtest/style.css\">",
                "<script src=\"/playtest/main.js\" defer></script>",
            ),
            _ => "<link rel=\"stylesheet\" href=\"/design/chat-history.css\">",
        });
        let main = if matches!(self, Self::Overview | Self::Playtest) {
            "page-width"
        } else {
            "page-width history-contract"
        };
        let _ = write!(
            output,
            "</head><body><a class=\"skip-link\" href=\"#main\">Skip to content</a>\
             <header class=\"site-header\"><div class=\"page-width header-inner\">\
             <span class=\"wordmark\"><span class=\"mark\" aria-hidden=\"true\">F</span> Forge IDE</span>\
             <a class=\"return-link\" href=\"{}\"><span aria-hidden=\"true\">←</span> {}</a>\
             </div></header><main id=\"main\" class=\"{main}\">",
            escape(back_href),
            escape(back_label),
        );
        render(body, &mut output);
        output.push_str("</main></body></html>\n");
        output
    }
}
