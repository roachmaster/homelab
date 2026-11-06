//! Typed reusable component declarations. No raw markup, stylesheet or script variant.
use std::collections::BTreeSet;

/// A pane's position is presentation, not application navigation or selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanePosition {
    Leading,
    Main,
    Trailing,
    Bottom,
}
impl PanePosition {
    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::Leading => "leading",
            Self::Main => "main",
            Self::Trailing => "trailing",
            Self::Bottom => "bottom",
        }
    }
}
/// Declarative text hierarchy; the generator chooses semantic HTML.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextStyle {
    Body,
    Caption,
    Context,
    Heading,
}
/// Emphasis is authored independently of action semantics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonEmphasis {
    Standard,
    Primary,
}
#[derive(Clone, Debug)]
pub enum Component {
    /// Native keyboard-accessible progressive disclosure; presentation only.
    Disclosure {
        id: String,
        title: String,
        children: Vec<Component>,
    },
    Screen {
        id: String,
        title: String,
        protected: bool,
        children: Vec<Component>,
    },
    Panel {
        id: String,
        title: String,
        children: Vec<Component>,
    },
    Region {
        id: String,
        children: Vec<Component>,
    },
    Toolbar(Vec<Component>),
    Row(Vec<Component>),
    Workspace(Vec<Component>),
    PaneLayout {
        id: String,
        /// Keep the two secondary tools mutually exclusive; main/leading remain independent.
        single_secondary: bool,
        children: Vec<Component>,
    },
    Pane {
        id: String,
        title: String,
        position: PanePosition,
        min: u16,
        initial: u16,
        max: u16,
        children: Vec<Component>,
    },
    Text {
        id: String,
        text: String,
        live: bool,
        style: TextStyle,
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
        emphasis: ButtonEmphasis,
    },
    Filter {
        id: String,
        label: String,
        action: String,
    },
    Reload {
        id: String,
        label: String,
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
    /// Read-only, bounded source window supplied by the accepted model view.
    Source {
        id: String,
        label: String,
    },
    /// Presentation-only route to a declared pane; no application event.
    PaneLink {
        id: String,
        label: String,
        target: String,
    },
    Details {
        id: String,
    },
    Facts {
        id: String,
    },
}
/// Component structure, not a string-template macro. Every text value is escaped.
#[macro_export]
macro_rules! component {
    (screen $id:expr, $title:expr, $protected:expr; [$($child:expr),* $(,)?]) => { $crate::generate::Component::Screen { id:$id.into(),title:$title.into(),protected:$protected,children:vec![$($child),*] } };
    (disclosure $id:expr, $title:expr; [$($child:expr),* $(,)?]) => { $crate::generate::Component::Disclosure { id:$id.into(),title:$title.into(),children:vec![$($child),*] } };
    (panel $id:expr, $title:expr; [$($child:expr),* $(,)?]) => { $crate::generate::Component::Panel { id:$id.into(),title:$title.into(),children:vec![$($child),*] } };
    (region $id:expr; [$($child:expr),* $(,)?]) => { $crate::generate::Component::Region { id:$id.into(),children:vec![$($child),*] } };
    (toolbar [$($child:expr),* $(,)?]) => { $crate::generate::Component::Toolbar(vec![$($child),*]) };
    (row [$($child:expr),* $(,)?]) => { $crate::generate::Component::Row(vec![$($child),*]) };
    (workspace [$($child:expr),* $(,)?]) => { $crate::generate::Component::Workspace(vec![$($child),*]) };
    (layout $id:expr; [$($child:expr),* $(,)?]) => { $crate::generate::Component::PaneLayout{id:$id.into(),single_secondary:false,children:vec![$($child),*]} };
    (focused_layout $id:expr; [$($child:expr),* $(,)?]) => { $crate::generate::Component::PaneLayout{id:$id.into(),single_secondary:true,children:vec![$($child),*]} };
    (pane $id:expr, $title:expr, $position:ident, $min:expr, $initial:expr, $max:expr; [$($child:expr),* $(,)?]) => { $crate::generate::Component::Pane{id:$id.into(),title:$title.into(),position:$crate::generate::PanePosition::$position,min:$min,initial:$initial,max:$max,children:vec![$($child),*]} };
    (text $id:expr, $text:expr) => { $crate::generate::Component::Text{id:$id.into(),text:$text.into(),live:false,style:$crate::generate::TextStyle::Body} };
    (caption $id:expr, $text:expr) => { $crate::generate::Component::Text{id:$id.into(),text:$text.into(),live:false,style:$crate::generate::TextStyle::Caption} };
    (context $id:expr, $text:expr) => { $crate::generate::Component::Text{id:$id.into(),text:$text.into(),live:false,style:$crate::generate::TextStyle::Context} };
    (heading $id:expr, $text:expr) => { $crate::generate::Component::Text{id:$id.into(),text:$text.into(),live:false,style:$crate::generate::TextStyle::Heading} };
    (status $id:expr, $text:expr) => { $crate::generate::Component::Text{id:$id.into(),text:$text.into(),live:true,style:$crate::generate::TextStyle::Body} };
    (select $id:expr, $label:expr, $action:expr) => { $crate::generate::Component::Select{id:$id.into(),label:$label.into(),action:$action.into()} };
    (button $id:expr, $label:expr, $action:expr) => { $crate::generate::Component::Button{id:$id.into(),label:$label.into(),action:$action.into(),emphasis:$crate::generate::ButtonEmphasis::Standard} };
    (primary $id:expr, $label:expr, $action:expr) => { $crate::generate::Component::Button{id:$id.into(),label:$label.into(),action:$action.into(),emphasis:$crate::generate::ButtonEmphasis::Primary} };
    (filter $id:expr, $label:expr, $action:expr) => { $crate::generate::Component::Filter{id:$id.into(),label:$label.into(),action:$action.into()} };
    (reload $id:expr, $label:expr) => { $crate::generate::Component::Reload{id:$id.into(),label:$label.into()} };
    (pair $id:expr, $label:expr, $action:expr) => { $crate::generate::Component::PairForm{id:$id.into(),label:$label.into(),action:$action.into()} };
    (tree $id:expr, $label:expr) => { $crate::generate::Component::Tree{id:$id.into(),label:$label.into()} };
    (source $id:expr, $label:expr) => { $crate::generate::Component::Source{id:$id.into(),label:$label.into()} };
    (pane_link $id:expr, $label:expr, $target:expr) => { $crate::generate::Component::PaneLink{id:$id.into(),label:$label.into(),target:$target.into()} };
    (facts $id:expr) => { $crate::generate::Component::Facts{id:$id.into()} };
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
    /// Bound declarations, generated IDs, pane geometry and action names before rendering.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.title.is_empty() || !text(&self.title) || !text(&self.subtitle) {
            return Err("invalid document label");
        }
        fn insert(ids: &mut BTreeSet<String>, id: &str) -> Result<(), &'static str> {
            if !key(id) || !ids.insert(id.into()) {
                Err("invalid or duplicate component id")
            } else {
                Ok(())
            }
        }
        fn visit(
            c: &Component,
            depth: usize,
            count: &mut usize,
            ids: &mut BTreeSet<String>,
            in_layout: bool,
        ) -> Result<(), &'static str> {
            *count += 1;
            if depth > 12 || *count > 256 {
                return Err("component declaration limit");
            }
            let (id, label, action) = match c {
                Component::Screen { id, title, .. }
                | Component::Panel { id, title, .. }
                | Component::Disclosure { id, title, .. }
                | Component::Pane { id, title, .. } => (Some(id), Some(title), None),
                Component::Row(_) | Component::Toolbar(_) | Component::Workspace(_) => {
                    (None, None, None)
                }
                Component::PaneLayout { id, .. } | Component::Region { id, .. } => {
                    (Some(id), None, None)
                }
                Component::Text { id, text, .. } => (Some(id), Some(text), None),
                Component::Select { id, label, action }
                | Component::Button {
                    id, label, action, ..
                }
                | Component::Filter { id, label, action }
                | Component::PairForm { id, label, action } => {
                    (Some(id), Some(label), Some(action))
                }
                Component::Tree { id, label }
                | Component::Reload { id, label }
                | Component::Source { id, label } => (Some(id), Some(label), None),
                Component::PaneLink { id, label, target } => (Some(id), Some(label), Some(target)),
                Component::Details { id } | Component::Facts { id } => (Some(id), None, None),
            };
            if let Some(id) = id {
                insert(ids, id)?;
            }
            if label.is_some_and(|s| !text(s)) || action.is_some_and(|s| !key(s)) {
                return Err("invalid component label/action");
            }
            match c {
                Component::Workspace(_) => insert(ids, "workspace")?,
                Component::PairForm { id, .. } => {
                    for suffix in ["form", "code"] {
                        insert(ids, &format!("{id}-{suffix}"))?;
                    }
                }
                Component::Tree { id, .. } => insert(ids, &format!("{id}-full-name"))?,
                Component::Pane {
                    id,
                    position,
                    min,
                    initial,
                    max,
                    ..
                } => {
                    if !in_layout
                        || !(120..=600).contains(min)
                        || min > initial
                        || initial > max
                        || *max > 960
                    {
                        return Err("invalid pane placement/geometry");
                    }
                    insert(ids, &format!("{id}-heading"))?;
                    insert(ids, &format!("{id}-switch"))?;
                    if *position != PanePosition::Main {
                        for suffix in ["toggle", "close", "separator"] {
                            insert(ids, &format!("{id}-{suffix}"))?;
                        }
                    }
                }
                Component::PaneLayout { id, children, .. } => {
                    for suffix in ["options", "reset"] {
                        insert(ids, &format!("{id}-{suffix}"))?;
                    }
                    let mut positions = BTreeSet::new();
                    for child in children {
                        if let Component::Pane { position, .. } = child {
                            if !positions.insert(position.key()) {
                                return Err("duplicate pane position");
                            }
                        } else {
                            return Err("pane layout requires panes");
                        }
                    }
                    if !positions.contains("main") || children.len() > 4 {
                        return Err("pane layout requires one main pane");
                    }
                }
                _ => {}
            }
            let children = match c {
                Component::Screen { children, .. }
                | Component::Panel { children, .. }
                | Component::Disclosure { children, .. }
                | Component::PaneLayout { children, .. }
                | Component::Region { children, .. }
                | Component::Pane { children, .. }
                | Component::Row(children)
                | Component::Toolbar(children)
                | Component::Workspace(children) => Some(children),
                _ => None,
            };
            if let Some(children) = children {
                for child in children {
                    visit(
                        child,
                        depth + 1,
                        count,
                        ids,
                        matches!(c, Component::PaneLayout { .. }),
                    )?;
                }
            }
            Ok(())
        }
        let mut ids = BTreeSet::from([
            "runtime-recovery".into(),
            "runtime-message".into(),
            "runtime-reload".into(),
        ]);
        let mut count = 0;
        for c in &self.children {
            visit(c, 0, &mut count, &mut ids, false)?;
        }
        Ok(())
    }
}
