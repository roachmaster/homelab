use std::fmt::Write;

pub(crate) fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

pub(super) type Styles = &'static [(&'static str, &'static [(&'static str, &'static str)])];

pub(super) fn stylesheet(styles: Styles) -> String {
    let mut css = String::new();
    for (selector, declarations) in styles {
        let _ = writeln!(css, "{selector} {{");
        for (property, value) in *declarations {
            let _ = writeln!(css, "  {property}: {value};");
        }
        css.push_str("}\n");
    }
    css
}
