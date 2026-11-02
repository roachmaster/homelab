//! Typed component declarations. No raw markup, stylesheet or script variant.
use std::collections::BTreeSet;
/// Semantic controls shared by applications; action names/fields are data, not JS.
#[derive(Clone, Debug)]
pub enum Component {
    Panel {
        id: String,
        title: String,
        children: Vec<Component>,
    },
    Row(Vec<Component>),
    Workspace(Vec<Component>),
    Text {
        id: String,
        text: String,
        live: bool,
    },
    Select {
        id: String,
        label: String,
        action: String,
    },
    Button {
        id: String,
        label: String,
        action: String,
    },
    PairForm {
        id: String,
        label: String,
        action: String,
    },
    Tree {
        id: String,
        label: String,
    },
    Details {
        id: String,
    },
}
/// Component structure, not a string-template macro. Every text value is escaped.
#[macro_export]
macro_rules! component {
    (panel $id:expr, $title:expr; [$($child:expr),* $(,)?]) => { $crate::generate::Component::Panel { id:$id.into(),title:$title.into(),children:vec![$($child),*] } };
    (row [$($child:expr),* $(,)?]) => { $crate::generate::Component::Row(vec![$($child),*]) };
    (workspace [$($child:expr),* $(,)?]) => { $crate::generate::Component::Workspace(vec![$($child),*]) };
    (text $id:expr, $text:expr) => { $crate::generate::Component::Text{id:$id.into(),text:$text.into(),live:false} };
    (status $id:expr, $text:expr) => { $crate::generate::Component::Text{id:$id.into(),text:$text.into(),live:true} };
    (select $id:expr, $label:expr, $action:expr) => { $crate::generate::Component::Select{id:$id.into(),label:$label.into(),action:$action.into()} };
    (button $id:expr, $label:expr, $action:expr) => { $crate::generate::Component::Button{id:$id.into(),label:$label.into(),action:$action.into()} };
    (pair $id:expr, $label:expr, $action:expr) => { $crate::generate::Component::PairForm{id:$id.into(),label:$label.into(),action:$action.into()} };
    (tree $id:expr, $label:expr) => { $crate::generate::Component::Tree{id:$id.into(),label:$label.into()} };
    (details $id:expr) => { $crate::generate::Component::Details{id:$id.into()} };
}
#[derive(Clone, Debug)]
pub struct Document {
    pub title: String,
    pub subtitle: String,
    pub children: Vec<Component>,
}
pub(crate) fn key(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
fn text(s: &str) -> bool {
    s.len() <= 512 && !s.contains('\0')
}
impl Document {
    /// Bound declarations and reject duplicate IDs or executable action syntax.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.title.is_empty() || !text(&self.title) || !text(&self.subtitle) {
            return Err("invalid document label");
        }
        fn visit(
            c: &Component,
            depth: usize,
            count: &mut usize,
            ids: &mut BTreeSet<String>,
        ) -> Result<(), &'static str> {
            *count += 1;
            if depth > 12 || *count > 256 {
                return Err("component declaration limit");
            }
            let (id, label, action) = match c {
                Component::Panel { id, title, .. } => (Some(id), Some(title), None),
                Component::Row(_) | Component::Workspace(_) => (None, None, None),
                Component::Text { id, text, .. } => (Some(id), Some(text), None),
                Component::Select { id, label, action }
                | Component::Button { id, label, action }
                | Component::PairForm { id, label, action } => {
                    (Some(id), Some(label), Some(action))
                }
                Component::Tree { id, label } => (Some(id), Some(label), None),
                Component::Details { id } => (Some(id), None, None),
            };
            if let Some(id) = id {
                if !key(id) || !ids.insert(id.clone()) {
                    return Err("invalid or duplicate component id");
                }
            }
            if label.is_some_and(|s| !text(s)) || action.is_some_and(|s| !key(s)) {
                return Err("invalid component label/action");
            }
            if matches!(c, Component::Workspace(_)) && !ids.insert("workspace".into()) {
                return Err("duplicate workspace id");
            }
            if let Component::PairForm { id, .. } = c {
                for extra in [format!("{id}-form"), format!("{id}-code")] {
                    if !key(&extra) || !ids.insert(extra) {
                        return Err("generated component id collision");
                    }
                }
            }
            match c {
                Component::Panel { children, .. }
                | Component::Row(children)
                | Component::Workspace(children) => {
                    for c in children {
                        visit(c, depth + 1, count, ids)?;
                    }
                }
                _ => {}
            }
            Ok(())
        }
        let mut ids = BTreeSet::new();
        let mut count = 0;
        for c in &self.children {
            visit(c, 0, &mut count, &mut ids)?;
        }
        Ok(())
    }
}
