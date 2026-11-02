//! Audited semantic HTML emitter shared by component documents. Application
//! labels escape here; only this generator owns markup syntax.
use super::Component;
use std::fmt::Write;
pub(crate) fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
pub(crate) fn emit(c: &Component, out: &mut String) {
    match c {
        Component::Panel {
            id,
            title,
            children,
        } => {
            let _ = write!(
                out,
                "<section class=\"fg-panel\" id=\"{}\"><h2>{}</h2>",
                escape(id),
                escape(title)
            );
            for c in children {
                emit(c, out);
            }
            out.push_str("</section>");
        }
        Component::Row(children) | Component::Workspace(children) => {
            out.push_str(if matches!(c, Component::Row(_)) {
                "<div class=\"fg-row\">"
            } else {
                "<main class=\"fg-workspace\" id=\"workspace\">"
            });
            for c in children {
                emit(c, out);
            }
            out.push_str(if matches!(c, Component::Row(_)) {
                "</div>"
            } else {
                "</main>"
            });
        }
        Component::Text { id, text, live } => {
            let role = if *live {
                " role=\"status\" aria-live=\"polite\""
            } else {
                ""
            };
            let _ = write!(out, "<p id=\"{}\"{role}>{}</p>", escape(id), escape(text));
        }
        Component::Select { id, label, action } => {
            let _=write!(out,"<label class=\"fg-field\" for=\"{id}\"><span>{}</span><select id=\"{id}\" data-action=\"{}\" disabled><option value=\"\">Not connected</option></select></label>",escape(label),escape(action));
        }
        Component::Button { id, label, action } => {
            let _ = write!(
                out,
                "<button type=\"button\" id=\"{}\" data-action=\"{}\" disabled>{}</button>",
                escape(id),
                escape(action),
                escape(label)
            );
        }
        Component::PairForm { id, label, action } => {
            let _=write!(out,"<form id=\"{id}-form\" data-action=\"{}\" class=\"fg-row\"><label class=\"fg-field\" for=\"{id}-code\"><span>{}</span><input id=\"{id}-code\" name=\"value\" type=\"password\" maxlength=\"128\" autocomplete=\"off\" required></label><button id=\"{id}\" type=\"submit\" disabled>Connect</button></form>",escape(action),escape(label));
        }
        Component::Tree { id, label } => {
            let _=write!(out,"<div id=\"{}\" class=\"fg-tree\" role=\"tree\" aria-label=\"{}\"><p class=\"fg-empty\">No report yet</p></div>",escape(id),escape(label));
        }
        Component::Details { id } => {
            let _ = write!(
                out,
                "<dl id=\"{}\" class=\"fg-details\"><dt>Selection</dt><dd>No selection</dd></dl>",
                escape(id)
            );
        }
    }
}
// Generic document primitive, not authored application HTML. No executable
// inline script/style is interpolated; content only comes from the typed emitter.
pub(crate) const SHELL:&str="<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>{{title}}</title><link rel=\"stylesheet\" href=\"/style.css\"><script src=\"/main.js\" defer></script></head><body><header class=\"fg-header\"><div class=\"fg-brand\"><span class=\"fg-mark\" aria-hidden=\"true\">F</span><h1>{{title}}</h1></div><p>{{subtitle}}</p></header><div class=\"fg-app\">{{{content}}}</div><noscript>This application requires JavaScript and WebAssembly.</noscript></body></html>";
