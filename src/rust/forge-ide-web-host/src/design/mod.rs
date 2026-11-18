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
    Footer => "footer", Form => "form", H1 => "h1", H2 => "h2", H3 => "h3", H4 => "h4",
    Img => "img", Input => "input", Label => "label", Li => "li", Nav => "nav",
    Ol => "ol", P => "p", Path => "path", Pre => "pre", Section => "section", Select => "select", Small => "small", Svg => "svg",
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
    D => "d", Fill => "fill", Focusable => "focusable", For => "for", Height => "height", Hidden => "hidden", Href => "href",
    Id => "id", Loading => "loading", Maxlength => "maxlength", Method => "method", Open => "open",
    Rel => "rel", Required => "required", Role => "role", Rows => "rows",
    Src => "src", Stroke => "stroke", StrokeLinecap => "stroke-linecap", StrokeLinejoin => "stroke-linejoin", StrokeWidth => "stroke-width",
    Tabindex => "tabindex", Target => "target", Type => "type", ViewBox => "viewBox", Width => "width",
});

#[derive(Clone, Copy)]
enum Icon {
    Terminal,
    Studio,
    Playtest,
}

enum Node {
    Text(&'static str),
    Element(Tag, &'static [(Attribute, &'static str)], &'static [Node]),
    GameEngineReport,
    Icon(Icon),
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
            Node::Icon(icon) => render_icon(*icon, output),
        }
    }
}

fn render_icon(icon: Icon, output: &mut String) {
    let paths: &[&str] = match icon {
        Icon::Terminal => &[
            "M5 4h14a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2Z",
            "m7 9 3 3-3 3M13 15h4",
        ],
        Icon::Studio => &["m12 3 9 5-9 5-9-5 9-5Z", "m3 12 9 5 9-5M3 16l9 5 9-5"],
        Icon::Playtest => &[
            "M5 4h14a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2Z",
            "m10 8 6 4-6 4V8Z",
        ],
    };
    render_element(
        Tag::Svg,
        &[
            (Attribute::ViewBox, "0 0 24 24"),
            (Attribute::Width, "20"),
            (Attribute::Height, "20"),
            (Attribute::AriaHidden, "true"),
            (Attribute::Focusable, "false"),
            (Attribute::Fill, "none"),
            (Attribute::Stroke, "currentColor"),
            (Attribute::StrokeWidth, "1.6"),
            (Attribute::StrokeLinecap, "round"),
            (Attribute::StrokeLinejoin, "round"),
        ],
        output,
        |out| {
            for path in paths {
                render_element(Tag::Path, &[(Attribute::D, path)], out, |_| {});
            }
        },
    );
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
            Self::Home => "<link rel=\"stylesheet\" href=\"/design/home.css\">",
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
        let body_class = if matches!(self, Self::Home) {
            "home-page"
        } else {
            ""
        };
        let _ = write!(
            output,
            "</head><body class=\"{body_class}\"><a class=\"skip-link\" href=\"#main\">Skip to content</a>\
             <header class=\"site-header\"><div class=\"page-width header-inner\">\
             <a class=\"wordmark return-link\" href=\"/home\"><span class=\"mark\" aria-hidden=\"true\">F</span> Forge IDE</a>",
        );
        if matches!(self, Self::Home) {
            render_element(
                Tag::A,
                &[
                    (Attribute::Class, "return-link home-github"),
                    (Attribute::Href, "https://github.com/roachmaster/homelab"),
                    (Attribute::AriaLabel, "GitHub"),
                ],
                &mut output,
                |out| {
                    out.push_str("GitHub ");
                    render_element(Tag::Span, &[(Attribute::AriaHidden, "true")], out, |out| {
                        out.push('↗')
                    });
                },
            );
        } else {
            let _ = write!(
                output,
                "<a class=\"return-link\" href=\"{}\"><span aria-hidden=\"true\">←</span> {}</a>",
                escape(back_href),
                escape(back_label)
            );
        }
        let _ = write!(output, "</div></header><main id=\"main\" class=\"{main}\">");
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
