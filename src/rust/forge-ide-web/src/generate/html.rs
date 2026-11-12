//! Audited semantic HTML emitter shared by component documents. Application
//! labels escape here; only this generator owns markup syntax.
use super::{ButtonEmphasis, Component, PanePosition, TextStyle};
use std::fmt::Write;
pub(crate) fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
fn children(items: &[Component], out: &mut String) {
    for child in items {
        emit(child, out);
    }
}
pub(crate) fn emit(c: &Component, out: &mut String) {
    match c {
        Component::Screen {
            id,
            title,
            protected,
            children: items,
        } => {
            let _ = write!(
                out,
                "<section id=\"{}\" class=\"fg-screen\" aria-label=\"{}\"{}>",
                escape(id),
                escape(title),
                if *protected {
                    " data-protected hidden"
                } else {
                    ""
                }
            );
            children(items, out);
            out.push_str("</section>");
        }
        Component::Disclosure {
            id,
            title,
            children: items,
        } => {
            let _ = write!(out, "<details id=\"{}\" class=\"fg-disclosure\"><summary>{}</summary><div class=\"fg-disclosure-body\">", escape(id), escape(title));
            children(items, out);
            out.push_str("</div></details>");
        }
        Component::Panel {
            id,
            title,
            children: items,
        } => {
            let _ = write!(
                out,
                "<section class=\"fg-panel\" id=\"{}\"><h2>{}</h2>",
                escape(id),
                escape(title)
            );
            children(items, out);
            out.push_str("</section>");
        }
        Component::Region {
            id,
            children: items,
        } => {
            let _ = write!(out, "<div id=\"{}\" class=\"fg-region\">", escape(id));
            children(items, out);
            out.push_str("</div>");
        }
        Component::Toolbar(items) | Component::Row(items) | Component::Workspace(items) => {
            out.push_str(match c {
                Component::Toolbar(_) => "<div class=\"fg-toolbar\">",
                Component::Row(_) => "<div class=\"fg-row\">",
                _ => "<main class=\"fg-workspace\" id=\"workspace\">",
            });
            children(items, out);
            out.push_str(if matches!(c, Component::Workspace(_)) {
                "</main>"
            } else {
                "</div>"
            });
        }
        Component::CompactToolbar {
            id,
            label,
            children: items,
        } => {
            let id = escape(id);
            let _ = write!(out, "<div class=\"fg-compact-toolbar\"><button type=\"button\" id=\"{id}-toggle\" data-compact-toolbar aria-controls=\"{id}\" aria-expanded=\"false\">{}</button><div class=\"fg-toolbar\" id=\"{id}\">", escape(label));
            children(items, out);
            out.push_str("</div></div>");
        }
        Component::PaneLayout {
            id,
            single_secondary,
            children: items,
        } => {
            let id = escape(id);
            let _=write!(out,"<section id=\"{id}\" class=\"fg-pane-layout\" data-layout data-single-secondary=\"{single_secondary}\"><div class=\"fg-layout-controls\"><details id=\"{id}-options\" class=\"fg-view-options\"><summary>View options</summary><div class=\"fg-option-list\">");
            for pane in items {
                if let Component::Pane {
                    id,
                    title,
                    position,
                    ..
                } = pane
                {
                    if *position != PanePosition::Main {
                        let _=write!(out,"<button type=\"button\" id=\"{}-toggle\" data-pane-toggle=\"{}\" aria-controls=\"{}\" aria-expanded=\"true\">{}</button>",escape(id),escape(id),escape(id),escape(title));
                    }
                }
            }
            let _=write!(out,"<button type=\"button\" id=\"{id}-reset\" data-layout-reset>Reset panes</button></div></details><nav class=\"fg-pane-switches\" aria-label=\"Visible pane\">");
            for pane in items {
                if let Component::Pane {
                    id,
                    title,
                    position,
                    ..
                } = pane
                {
                    let _=write!(out,"<button type=\"button\" id=\"{}-switch\" data-pane-position=\"{}\" data-pane-switch=\"{}\" aria-controls=\"{}\" aria-pressed=\"false\">{}</button>",escape(id),position.key(),escape(id),escape(id),escape(title));
                }
            }
            out.push_str("</nav></div><div class=\"fg-panes\">");
            children(items, out);
            out.push_str("</div></section>");
        }
        Component::Pane {
            id,
            title,
            position,
            min,
            initial,
            max,
            children: items,
        } => {
            let id = escape(id);
            let title = escape(title);
            let position_key = position.key();
            let _=write!(out,"<section id=\"{id}\" class=\"fg-pane fg-pane-{position_key}\" data-pane=\"{position_key}\" data-min=\"{min}\" data-default=\"{initial}\" data-max=\"{max}\" aria-labelledby=\"{id}-heading\"><header class=\"fg-pane-header\"><h2 id=\"{id}-heading\" tabindex=\"-1\">{title}</h2>");
            if *position != PanePosition::Main {
                let _=write!(out,"<button type=\"button\" id=\"{id}-close\" class=\"fg-pane-close\" data-pane-toggle=\"{id}\" aria-label=\"Collapse {title}\" aria-controls=\"{id}\">−</button>");
            }
            out.push_str("</header><div class=\"fg-pane-body\">");
            children(items, out);
            out.push_str("</div></section>");
            if *position != PanePosition::Main {
                let orientation = if *position == PanePosition::Bottom {
                    "horizontal"
                } else {
                    "vertical"
                };
                let _=write!(out,"<div id=\"{id}-separator\" class=\"fg-separator fg-separator-{position_key}\" role=\"separator\" tabindex=\"0\" aria-label=\"Resize {title}; arrows adjust, Enter collapses\" aria-controls=\"{id}\" aria-orientation=\"{orientation}\" aria-valuemin=\"{min}\" aria-valuemax=\"{max}\" aria-valuenow=\"{initial}\" data-resize-pane=\"{id}\"></div>");
            }
        }
        Component::Text {
            id,
            text,
            live,
            style,
        } => {
            let (tag, class) = match style {
                TextStyle::Body => ("p", "fg-text"),
                TextStyle::Caption => ("p", "fg-caption"),
                TextStyle::Context => ("p", "fg-context"),
                TextStyle::Heading => ("h3", "fg-selection-heading"),
            };
            let role = if *live {
                " role=\"status\" aria-live=\"polite\""
            } else {
                ""
            };
            let _ = write!(
                out,
                "<{tag} id=\"{}\" class=\"{class}\" data-bind-text{role}>{}</{tag}>",
                escape(id),
                escape(text)
            );
        }
        Component::Select { id, label, action } => {
            let _=write!(out,"<label class=\"fg-field\" for=\"{id}\"><span>{}</span><select id=\"{id}\" data-action=\"{}\" disabled><option value=\"\">Not connected</option></select></label>",escape(label),escape(action));
        }
        Component::Button {
            id,
            label,
            action,
            emphasis,
            check_input,
        } => {
            let class = match emphasis {
                ButtonEmphasis::Standard => "fg-button",
                ButtonEmphasis::Primary => "fg-button fg-button-primary",
            };
            let check = check_input.as_ref().map_or_else(String::new, |id| {
                format!(" data-check-input=\"{}\"", escape(id))
            });
            let _ = write!(
                out,
                "<button type=\"button\" class=\"{class}\" id=\"{}\" data-action=\"{}\"{check} disabled>{}</button>",
                escape(id),
                escape(action),
                escape(label)
            );
        }
        Component::Navigation {
            id,
            title,
            children: items,
        } => {
            let _ = write!(out, "<nav id=\"{}\" class=\"fg-toolbar fg-navigation\" aria-label=\"{}\" data-protected hidden>", escape(id), escape(title));
            children(items, out);
            out.push_str("</nav>");
        }
        Component::ReadingColumn(items) => {
            out.push_str("<div class=\"fg-reading-column\">");
            children(items, out);
            out.push_str("</div>");
        }
        Component::Transcript { id, label } => {
            let _ = write!(out, "<div id=\"{}\" class=\"fg-transcript\" role=\"region\" aria-label=\"{}\" tabindex=\"0\" data-transcript></div>", escape(id), escape(label));
        }
        Component::TextArea { id, label, action } => {
            let _ = write!(out, "<label class=\"fg-field fg-textarea\" for=\"{id}\"><span>{}</span><textarea id=\"{id}\" rows=\"3\" data-action=\"{}\" data-dynamic-action autocomplete=\"off\" disabled></textarea></label>", escape(label), escape(action));
        }
        Component::Filter { id, label, action } => {
            let _=write!(out,"<label class=\"fg-field fg-filter\" for=\"{id}\"><span>{}</span><input id=\"{id}\" type=\"search\" data-action=\"{}\" data-dynamic-action autocomplete=\"off\" maxlength=\"256\" disabled></label>",escape(label),escape(action));
        }
        Component::Reload { id, label } => {
            let _ = write!(
                out,
                "<button type=\"button\" id=\"{}\" data-reload>{}</button>",
                escape(id),
                escape(label)
            );
        }
        Component::PairForm { id, label, action } => {
            let _=write!(out,"<form id=\"{id}-form\" data-action=\"{}\" class=\"fg-row\"><label class=\"fg-field\" for=\"{id}-code\"><span>{}</span><input id=\"{id}-code\" name=\"value\" type=\"password\" maxlength=\"128\" autocomplete=\"off\" required></label><button id=\"{id}\" type=\"submit\" disabled>Connect</button></form>",escape(action),escape(label));
        }
        Component::Tree { id, label } => {
            let _=write!(out,"<div id=\"{}\" class=\"fg-tree\" role=\"tree\" aria-label=\"{}\" tabindex=\"0\"><p class=\"fg-empty\">No report yet</p></div><p id=\"{}-full-name\" class=\"fg-focus-name\" data-name-for=\"{}\" aria-label=\"Focused item full name\"></p>",escape(id),escape(label),escape(id),escape(id));
        }
        Component::Source { id, label } => {
            let _ = write!(out, "<div class=\"fg-source-frame\" data-source-frame><pre id=\"{}\" class=\"fg-source\" data-source-view tabindex=\"0\" aria-label=\"{}\"></pre></div>", escape(id), escape(label));
        }
        Component::SourceWrap { id, label, target } => {
            let _ = write!(out, "<button type=\"button\" id=\"{}\" class=\"fg-source-wrap\" data-source-wrap aria-controls=\"{}\" aria-pressed=\"false\">{}</button>", escape(id), escape(target), escape(label));
        }
        Component::PaneLink { id, label, target } => {
            let _ = write!(out, "<button type=\"button\" id=\"{}\" class=\"fg-pane-link\" data-pane-reveal=\"{}\">{}</button>", escape(id), escape(target), escape(label));
        }
        Component::Details { id } | Component::Facts { id } => {
            let class = if matches!(c, Component::Facts { .. }) {
                "fg-details fg-facts"
            } else {
                "fg-details"
            };
            let _ = write!(
                out,
                "<dl id=\"{}\" class=\"{class}\"><div class=\"fg-property\"><dt>Selection</dt><dd>No selection</dd></div></dl>",
                escape(id)
            );
        }
    }
}
// A reusable shell/recovery primitive; content is exclusively typed emitter output.
pub(crate) const SHELL: &str = "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>{{title}}</title><link rel=\"stylesheet\" href=\"/style.css\"><script src=\"/main.js\" defer></script></head><body><header class=\"fg-header\"><div class=\"fg-brand\"><span class=\"fg-mark\" aria-hidden=\"true\">F</span><h1>{{title}}</h1></div><p>{{subtitle}}</p></header><section id=\"runtime-recovery\" class=\"fg-recovery\" hidden><p id=\"runtime-message\" role=\"alert\"></p><button id=\"runtime-reload\" type=\"button\" data-reload>Reload application</button></section><div class=\"fg-app\">{{{content}}}</div><noscript>This application requires JavaScript and WebAssembly.</noscript></body></html>";
