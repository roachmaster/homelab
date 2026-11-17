//! Audited semantic HTML emitter shared by component documents. Application
//! labels escape here; only this generator owns markup syntax.
use super::{ButtonEmphasis, Component, PanePosition, TextStyle};
use std::fmt::Write;

// Only output from this component emitter enters the shell's raw slots.
pub(super) const SHELL: &str = include_str!("../../ui/document.forge");

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
fn disclosure(id: &str, title: &str, items: &[Component], group: Option<&str>, out: &mut String) {
    let _ = write!(
        out,
        "<details id=\"{}\" class=\"fg-disclosure\"",
        escape(id)
    );
    if let Some(name) = group {
        let _ = write!(out, " name=\"{name}\"");
    }
    let _ = write!(
        out,
        "><summary>{}</summary><div class=\"fg-disclosure-body\">",
        escape(title)
    );
    children(items, out);
    out.push_str("</div></details>");
}
pub(crate) fn emit(c: &Component, out: &mut String) {
    match c {
        Component::BrandContext { id } => {
            let _ = write!(out, "<span id=\"{}\" class=\"fg-brand-context\" data-bind-text data-protected hidden></span>", escape(id));
        }
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
        } => disclosure(id, title, items, None, out),
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
        Component::Dialog {
            id,
            title,
            close_action,
            initial_focus,
            children: items,
        } => {
            let id = escape(id);
            let _ = write!(out, "<dialog id=\"{id}\" class=\"fg-modal\" data-modal=\"{}\" data-initial-focus=\"{}\" aria-labelledby=\"{id}-title\"><h2 id=\"{id}-title\">{}</h2>", escape(close_action), escape(initial_focus), escape(title));
            children(items, out);
            out.push_str("</dialog>");
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
        Component::PaneLayout {
            id,
            single_secondary,
            children: items,
        } => {
            let id = escape(id);
            let options = format!("{id}-options");
            let controls = items.iter().find_map(|item| match item {
                Component::LayoutControls(items) => Some(items),
                _ => None,
            });
            let action = items
                .iter()
                .find(|item| matches!(item, Component::PaneAction(_)));
            let _=write!(out,"<section id=\"{id}\" class=\"fg-pane-layout\" data-layout data-single-secondary=\"{single_secondary}\"><div class=\"fg-layout-controls\">");
            if controls.is_some() {
                out.push_str("<div class=\"fg-layout-options\">");
            }
            let _ = write!(
                out,
                "<details id=\"{options}\" class=\"fg-view-options\" name=\"{options}\"><summary>View options</summary><div class=\"fg-option-list\">"
            );
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
            let _=write!(out,"<button type=\"button\" id=\"{id}-reset\" data-layout-reset>Reset panes</button></div></details>");
            if let Some(controls) = controls {
                for item in controls {
                    match item {
                        Component::Disclosure {
                            id,
                            title,
                            children,
                        } => disclosure(id, title, children, Some(&options), out),
                        _ => emit(item, out),
                    }
                }
                out.push_str("</div>");
            }
            if action.is_some() {
                out.push_str("<div class=\"fg-pane-actions\">");
            }
            out.push_str("<nav class=\"fg-pane-switches\" aria-label=\"Visible pane\">");
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
            out.push_str("</nav>");
            if let Some(action) = action {
                emit(action, out);
                out.push_str("</div>");
            }
            out.push_str("</div><div class=\"fg-panes\">");
            for pane in items
                .iter()
                .filter(|item| matches!(item, Component::Pane { .. }))
            {
                emit(pane, out);
            }
            out.push_str("</div></section>");
        }
        Component::LayoutControls(items) => children(items, out),
        Component::PaneAction(button) => emit(button, out),
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
            let _=write!(out,"<section id=\"{id}\" class=\"fg-pane fg-pane-{position_key}\" data-pane=\"{position_key}\" data-min=\"{min}\" data-default=\"{initial}\" data-max=\"{max}\" aria-labelledby=\"{id}-heading\"><header class=\"fg-pane-header\">");
            if let Some(link) = items
                .iter()
                .find(|item| matches!(item, Component::PaneHeaderLink { .. }))
            {
                emit(link, out);
            }
            let _ = write!(out, "<h2 id=\"{id}-heading\" tabindex=\"-1\">{title}</h2>");
            if *position != PanePosition::Main {
                let _=write!(out,"<button type=\"button\" id=\"{id}-close\" class=\"fg-pane-close\" data-pane-toggle=\"{id}\" aria-label=\"Collapse {title}\" aria-controls=\"{id}\">−</button>");
            }
            out.push_str("</header><div class=\"fg-pane-body\">");
            for item in items
                .iter()
                .filter(|item| !matches!(item, Component::PaneHeaderLink { .. }))
            {
                emit(item, out);
            }
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
            controls,
            uuid_field,
        } => {
            let class = match emphasis {
                ButtonEmphasis::Standard => "fg-button",
                ButtonEmphasis::Primary => "fg-button fg-button-primary",
            };
            let check = check_input.as_ref().map_or_else(String::new, |id| {
                format!(" data-check-input=\"{}\"", escape(id))
            });
            let controls = controls.as_ref().map_or_else(String::new, |id| {
                format!(" aria-controls=\"{}\" aria-expanded=\"false\"", escape(id))
            });
            let uuid = uuid_field.as_ref().map_or_else(String::new, |field| {
                format!(" data-uuid-field=\"{}\"", escape(field))
            });
            let _ = write!(
                out,
                "<button type=\"button\" class=\"{class}\" id=\"{}\" data-action=\"{}\"{check}{controls}{uuid} disabled>{}</button>",
                escape(id),
                escape(action),
                escape(label)
            );
        }
        Component::IconButton {
            id,
            label,
            action,
            emphasis,
            glyph,
        } => {
            let primary = if *emphasis == ButtonEmphasis::Primary {
                " fg-button-primary"
            } else {
                ""
            };
            let label = escape(label);
            let _ = write!(
                out,
                "<button type=\"button\" class=\"fg-button{primary} fg-button-icon\" id=\"{}\" data-action=\"{}\" aria-label=\"{label}\" title=\"{label}\" disabled><span aria-hidden=\"true\" data-glyph=\"{}\"></span></button>",
                escape(id),
                escape(action),
                escape(&glyph.to_string())
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
            if items
                .iter()
                .any(|item| matches!(item, Component::NavigationLink { .. }))
            {
                out.push_str("<p data-workspace-navigation-status role=\"status\" hidden></p>");
            }
        }
        Component::NavigationLink {
            id,
            label,
            document,
        } => {
            let _ = write!(
                out,
                "<a id=\"{}\" class=\"fg-navigation-link\" href=\"{}\" data-workspace-navigation>{}</a>",
                escape(id),
                document.href(),
                escape(label),
            );
        }
        Component::ReaderLayout(items) => {
            out.push_str("<div class=\"fg-reader-layout\" data-reader-layout>");
            children(items, out);
            out.push_str("</div>");
        }
        Component::ReadingColumn(items) => {
            out.push_str("<div class=\"fg-reading-column\">");
            children(items, out);
            out.push_str("</div>");
        }
        Component::Transcript {
            id,
            label,
            user_label,
            assistant_label,
            empty_message,
        } => {
            let _ = write!(
                out,
                concat!(
                    "<div id=\"{}\" class=\"fg-transcript\" role=\"region\" aria-label=\"{}\" tabindex=\"0\" data-transcript",
                    " data-transcript-user=\"{}\" data-transcript-assistant=\"{}\" data-transcript-empty=\"{}\"></div>",
                ),
                escape(id),
                escape(label),
                escape(user_label),
                escape(assistant_label),
                escape(empty_message)
            );
        }
        Component::TextField { id, label, action } => {
            let id = escape(id);
            let _ = write!(out, "<label class=\"fg-field\" for=\"{id}\"><span>{}</span><input id=\"{id}\" type=\"text\" data-workspace-draft data-action=\"{}\" data-dynamic-action autocomplete=\"off\" disabled></label>", escape(label), escape(action));
        }
        Component::TextArea { id, label, action } => {
            let _ = write!(out, "<label class=\"fg-field fg-textarea\" for=\"{id}\"><span>{}</span><textarea id=\"{id}\" rows=\"3\" data-action=\"{}\" data-dynamic-action autocomplete=\"off\" disabled></textarea></label>", escape(label), escape(action));
        }
        Component::DateInput { id, label, action } => {
            let id = escape(id);
            let _ = write!(out, "<label class=\"fg-field\" for=\"{id}\"><span>{}</span><input id=\"{id}\" type=\"date\" data-action=\"{}\" max=\"9999-12-31\" required disabled></label>", escape(label), escape(action));
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
        Component::Tree { id, label, stacked } => {
            let class = if *stacked {
                "fg-tree fg-stacked-tree"
            } else {
                "fg-tree"
            };
            let _=write!(out,"<div id=\"{}\" class=\"{class}\" role=\"tree\" aria-label=\"{}\" tabindex=\"0\"><p class=\"fg-empty\">No report yet</p></div><p id=\"{}-full-name\" class=\"fg-focus-name\" data-name-for=\"{}\" aria-label=\"Focused item full name\"></p>",escape(id),escape(label),escape(id),escape(id));
        }
        Component::Source { id, label } => {
            let _ = write!(out, "<pre id=\"{}\" class=\"fg-source\" data-source-view tabindex=\"0\" aria-label=\"{}\"></pre>", escape(id), escape(label));
        }
        Component::SourceFormats {
            prefix,
            empty_message,
            unavailable_message,
            children: items,
        } => {
            let prefix = escape(prefix);
            let raw_pane = format!("{prefix}-raw-pane");
            let preview = format!("{prefix}-preview");
            let _ = write!(
                out,
                concat!(
                    "<div data-source-formats><div id=\"{prefix}-format\" class=\"fg-source-format\" data-source-format role=\"group\" aria-label=\"Markdown view\" hidden>",
                    "<button type=\"button\" id=\"{prefix}-raw\" data-source-mode=\"raw\" aria-controls=\"{raw_pane}\" aria-pressed=\"true\">Raw</button>",
                    "<button type=\"button\" id=\"{prefix}-display\" data-source-mode=\"display\" aria-controls=\"{preview}\" aria-pressed=\"false\">Display</button>",
                    "</div><p data-source-preview-unavailable class=\"fg-caption\" hidden>{}</p><div id=\"{raw_pane}\" data-source-raw>",
                ),
                escape(unavailable_message),
                prefix = prefix,
                raw_pane = raw_pane,
                preview = preview
            );
            children(items, out);
            let _ = write!(
                out,
                concat!(
                    "</div><div id=\"{preview}\" class=\"fg-source-preview\" data-source-preview data-source-preview-empty=\"{}\"",
                    " role=\"region\" aria-label=\"Markdown display\" tabindex=\"0\" hidden></div></div>",
                ),
                escape(empty_message),
                preview = preview
            );
        }
        Component::PaneLink { id, label, target } => {
            let _ = write!(out, "<button type=\"button\" id=\"{}\" class=\"fg-pane-link\" data-pane-reveal=\"{}\">{}</button>", escape(id), escape(target), escape(label));
        }
        Component::PaneHeaderLink {
            id,
            label,
            target,
            glyph,
        } => {
            let label = escape(label);
            let target = escape(target);
            let _ = write!(out, "<button type=\"button\" id=\"{}\" class=\"fg-button-icon fg-pane-header-link\" data-pane-reveal=\"{target}\" aria-controls=\"{target}\" aria-label=\"{label}\" title=\"{label}\"><span aria-hidden=\"true\" data-glyph=\"{}\"></span></button>", escape(id), escape(&glyph.to_string()));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generate::application::ide_document;

    #[test]
    fn presentation_copy_is_escaped_for_browser_bindings() {
        let mut output = String::new();
        emit(
            &crate::component!(transcript "messages", "Messages", "You & me", "\"Assistant\"", "<Empty>"),
            &mut output,
        );
        assert!(output.contains("data-transcript-user=\"You &amp; me\""));
        assert!(output.contains("data-transcript-assistant=\"&quot;Assistant&quot;\""));
        assert!(output.contains("data-transcript-empty=\"&lt;Empty&gt;\""));

        output.clear();
        emit(
            &crate::component!(source_formats "source", "\"Empty\"", "<Unavailable>"; [
                crate::component!(source "file", "Source"),
            ]),
            &mut output,
        );
        assert!(output.contains("data-source-preview-empty=\"&quot;Empty&quot;\""));
        assert!(output.contains("hidden>&lt;Unavailable&gt;</p>"));
    }

    #[test]
    fn design_opens_in_the_forge_tab_from_the_protected_navigation() {
        let document = ide_document();
        document.validate().unwrap();
        let navigation = document.children.iter().find(|component| {
            matches!(component, Component::Navigation { id, .. } if id == "ide-navigation")
        }).unwrap();
        let mut output = String::new();
        emit(navigation, &mut output);
        assert!(output.starts_with("<nav id=\"ide-navigation\" class=\"fg-toolbar fg-navigation\" aria-label=\"Forge IDE views\" data-protected hidden>"));
        let (_, after_chat) = output.split_once("id=\"open-chat\"").unwrap();
        let (_, after_workbench) = after_chat.split_once("id=\"open-workbench\"").unwrap();
        let (_, after_button) = after_workbench.split_once("</button>").unwrap();
        assert_eq!(after_button, "<a id=\"open-design\" class=\"fg-navigation-link\" href=\"/design\" data-workspace-navigation>Design</a></nav><p data-workspace-navigation-status role=\"status\" hidden></p>");
    }
}
