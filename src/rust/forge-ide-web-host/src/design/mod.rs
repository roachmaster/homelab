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
    Ol => "ol", P => "p", Pre => "pre", Section => "section", Select => "select", Small => "small",
    Span => "span", Strong => "strong", Summary => "summary", Table => "table", Textarea => "textarea", Ul => "ul",
});

html_names!(Attribute {
    Accept => "accept", Alt => "alt", AriaAtomic => "aria-atomic", AriaCurrent => "aria-current",
    AriaDescribedby => "aria-describedby", AriaHidden => "aria-hidden", AriaLabel => "aria-label",
    AriaLabelledby => "aria-labelledby", AriaLive => "aria-live", AriaPressed => "aria-pressed",
    Autocomplete => "autocomplete", Autofocus => "autofocus",
    Class => "class", DataHistoryDismiss => "data-history-dismiss",
    DataPreviewChat => "data-preview-chat", DataRole => "data-role",
    DataPlaytestAction => "data-playtest-action",
    DataPlaytestTraceChannel => "data-playtest-trace-channel", Disabled => "disabled",
    For => "for", Height => "height", Hidden => "hidden", Href => "href",
    Id => "id", Loading => "loading", Maxlength => "maxlength", Method => "method", Open => "open",
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

macro_rules! design_link {
    ($path:literal, $title:literal, $description:literal) => {
        element!(Li;
            element!(A [Href = $path]; text($title)),
            element!(P; text($description)),
        )
    };
}

mod dispatch;
mod forge_providers;
mod forge_workers;
mod game_asset_pipeline;
pub mod game_engine;
mod game_physics;
mod history;
mod home;
mod landing;
mod mail;
pub mod playtest;
pub mod playtest_asset_worker;
pub mod playtest_browser;
pub mod studio;

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
    Home,
    Overview,
    ForgeProviders,
    ChatHistory,
    ChatDispatch,
    Mail,
    GameStudio,
    GamePhysics,
    GameAssetPipeline,
    ForgeWorkers,
    ForgeWorkersPlaytest,
    ForgeWorkersStudio,
    Playtest,
    Studio,
}

impl DesignPage {
    /// Render a complete document using the shared portal design shell.
    pub fn render(self) -> String {
        let (title, back_href, back_label, body) = match self {
            Self::Home => ("Forge portal · Forge IDE", "/", "Back to Forge", home::BODY),
            Self::Overview => ("Forge IDE design", "/", "Back to Forge", landing::BODY),
            Self::ForgeProviders => (
                "Forge plug-and-play · Forge IDE",
                "/design#forge-foundations",
                "Back to Design",
                forge_providers::BODY,
            ),
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
            Self::GamePhysics => (
                "Physics for the Forge Game Runtime · Forge IDE",
                "/design",
                "Back to Design",
                game_physics::BODY,
            ),
            Self::GameAssetPipeline => (
                "Asset Pipeline for the Forge Game Runtime · Forge IDE",
                "/design",
                "Back to Design",
                game_asset_pipeline::BODY,
            ),
            Self::ForgeWorkers => (
                "Forge worker and main-thread ownership · Forge IDE",
                "/design",
                "Back to Design",
                forge_workers::OWNERSHIP,
            ),
            Self::ForgeWorkersPlaytest => (
                "Playtest worker flow · Forge IDE",
                "/design/forge-workers",
                "Back to worker ownership",
                forge_workers::PLAYTEST,
            ),
            Self::ForgeWorkersStudio => (
                "Studio build worker flow · Forge IDE",
                "/design/forge-workers",
                "Back to worker ownership",
                forge_workers::STUDIO,
            ),
            Self::Playtest => (
                "Room One · Forge Playtest",
                "/design/game-studio",
                "Back to game engine design",
                playtest::BODY,
            ),
            Self::Studio => ("Forge Studio", "/", "Back to Forge", studio::BODY),
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
            Self::Home => concat!(
                "<link rel=\"stylesheet\" href=\"/design/index.css\">",
                "<link rel=\"stylesheet\" href=\"/design/home.css\">",
            ),
            Self::Overview => concat!(
                "<link rel=\"stylesheet\" href=\"/design/index.css\">",
                "<script src=\"/design/previous-chats.js\" defer></script>",
            ),
            Self::GameStudio => "<link rel=\"stylesheet\" href=\"/design/game-studio.css\">",
            Self::Playtest => concat!(
                "<link rel=\"stylesheet\" href=\"/playtest/style.css\">",
                "<script src=\"/playtest/main.js\" defer></script>",
            ),
            Self::Studio => concat!(
                "<link rel=\"stylesheet\" href=\"/studio/style.css\">",
                "<script src=\"/studio/main.js\" defer></script>",
            ),
            _ => "<link rel=\"stylesheet\" href=\"/design/chat-history.css\">",
        });
        let main = if matches!(self, Self::Home | Self::Overview | Self::Playtest) {
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

pub fn index_stylesheet() -> String {
    landing::stylesheet()
}

pub fn home_stylesheet() -> String {
    home::stylesheet()
}
