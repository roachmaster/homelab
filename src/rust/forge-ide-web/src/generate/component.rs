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
/// A document served by the same portal origin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortalDocument {
    Home,
    Design,
}
impl PortalDocument {
    pub(crate) fn href(self) -> &'static str {
        match self {
            Self::Home => "/home",
            Self::Design => "/design",
        }
    }
}
#[derive(Clone, Debug)]
pub enum Component {
    /// Document identity linking to a portal document in the current tab.
    BrandLink {
        id: String,
        label: String,
        document: PortalDocument,
    },
    /// Protected text projected next to the document title.
    BrandContext {
        id: String,
    },
    /// Protected application controls projected into the document header.
    HeaderActions {
        id: String,
        children: Vec<Component>,
    },
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
    Dialog {
        id: String,
        title: String,
        close_action: String,
        initial_focus: String,
        children: Vec<Component>,
    },
    /// Presentation-only native dialog; it does not dispatch an application event.
    Sheet {
        id: String,
        title: String,
        initial_focus: String,
        children: Vec<Component>,
    },
    SheetTrigger {
        id: String,
        label: String,
        target: String,
        glyph: Option<char>,
    },
    Toolbar(Vec<Component>),
    CompactToolbar {
        id: String,
        label: String,
        children: Vec<Component>,
    },
    Navigation {
        id: String,
        title: String,
        children: Vec<Component>,
    },
    /// Open a portal document in the current tab.
    NavigationLink {
        id: String,
        label: String,
        document: PortalDocument,
    },
    ReadingColumn(Vec<Component>),
    ReaderLayout(Vec<Component>),
    Transcript {
        id: String,
        label: String,
        user_label: String,
        assistant_label: String,
        empty_message: String,
    },
    TextField {
        id: String,
        label: String,
        action: String,
    },
    /// Native boolean input; the application owns its accepted value.
    Checkbox {
        id: String,
        label: String,
        action: String,
    },
    TextArea {
        id: String,
        label: String,
        action: String,
        placeholder: Option<String>,
    },
    Row(Vec<Component>),
    Workspace(Vec<Component>),
    PaneLayout {
        id: String,
        /// Keep the two secondary tools mutually exclusive; main/leading remain independent.
        single_secondary: bool,
        /// Put layout options in a header-triggered sheet.
        header_options: bool,
        children: Vec<Component>,
    },
    /// Optional declarations placed next to a pane layout's built-in view options.
    LayoutControls(Vec<Component>),
    /// An optional button adjacent to the pane navigation, outside its landmark.
    PaneAction(Box<Component>),
    Pane {
        id: String,
        title: String,
        position: PanePosition,
        min: u16,
        initial: u16,
        max: u16,
        children: Vec<Component>,
    },
    PaneFooter {
        id: String,
        children: Vec<Component>,
    },
    Text {
        id: String,
        text: String,
        live: bool,
        style: TextStyle,
    },
    Badge {
        id: String,
        label: String,
    },
    MetricCards {
        id: String,
        label: String,
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
        check_input: Option<String>,
        controls: Option<String>,
        uuid_field: Option<String>,
    },
    /// A button whose visual glyph is decorative and whose label names the action.
    IconButton {
        id: String,
        label: String,
        action: String,
        emphasis: ButtonEmphasis,
        glyph: char,
        check_input: Option<String>,
        uuid_field: Option<String>,
    },
    DateInput {
        id: String,
        label: String,
        action: String,
    },
    Filter {
        id: String,
        label: String,
        action: String,
        placeholder: Option<String>,
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
        stacked: bool,
    },
    /// Read-only, bounded source window supplied by the accepted model view.
    Source {
        id: String,
        label: String,
    },
    Diff {
        id: String,
        label: String,
    },
    SourceWrap {
        id: String,
        label: String,
        target: String,
    },
    /// Optional raw and Markdown presentations around one read-only source view.
    SourceFormats {
        prefix: String,
        empty_message: String,
        unavailable_message: String,
        children: Vec<Component>,
    },
    /// Presentation-only route to a declared pane; no application event.
    PaneLink {
        id: String,
        label: String,
        target: String,
    },
    /// A narrow-layout pane link rendered before the pane heading.
    PaneHeaderLink {
        id: String,
        label: String,
        target: String,
        glyph: char,
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
    (brand_link $id:expr, $label:expr, $document:ident) => { $crate::generate::Component::BrandLink { id:$id.into(),label:$label.into(),document:$crate::generate::PortalDocument::$document } };
    (header_actions $id:expr; [$($child:expr),* $(,)?]) => { $crate::generate::Component::HeaderActions { id:$id.into(),children:vec![$($child),*] } };
    (brand_context $id:expr) => { $crate::generate::Component::BrandContext { id:$id.into() } };
    (screen $id:expr, $title:expr, $protected:expr; [$($child:expr),* $(,)?]) => { $crate::generate::Component::Screen { id:$id.into(),title:$title.into(),protected:$protected,children:vec![$($child),*] } };
    (disclosure $id:expr, $title:expr; [$($child:expr),* $(,)?]) => { $crate::generate::Component::Disclosure { id:$id.into(),title:$title.into(),children:vec![$($child),*] } };
    (panel $id:expr, $title:expr; [$($child:expr),* $(,)?]) => { $crate::generate::Component::Panel { id:$id.into(),title:$title.into(),children:vec![$($child),*] } };
    (region $id:expr; [$($child:expr),* $(,)?]) => { $crate::generate::Component::Region { id:$id.into(),children:vec![$($child),*] } };
    (sheet $id:expr, $title:expr, $focus:expr; [$($child:expr),* $(,)?]) => { $crate::generate::Component::Sheet { id:$id.into(),title:$title.into(),initial_focus:$focus.into(),children:vec![$($child),*] } };
    (sheet_trigger $id:expr, $label:expr, $target:expr) => { $crate::generate::Component::SheetTrigger { id:$id.into(),label:$label.into(),target:$target.into(),glyph:None } };
    (sheet_icon $id:expr, $label:expr, $target:expr, $glyph:expr) => { $crate::generate::Component::SheetTrigger { id:$id.into(),label:$label.into(),target:$target.into(),glyph:Some($glyph) } };
    (dialog $id:expr, $title:expr, $close:expr, $focus:expr; [$($child:expr),* $(,)?]) => { $crate::generate::Component::Dialog{id:$id.into(),title:$title.into(),close_action:$close.into(),initial_focus:$focus.into(),children:vec![$($child),*]} };
    (navigation $id:expr, $title:expr; [$($child:expr),* $(,)?]) => { $crate::generate::Component::Navigation { id:$id.into(),title:$title.into(),children:vec![$($child),*] } };
    (navigation_link $id:expr, $label:expr, $document:ident) => { $crate::generate::Component::NavigationLink { id:$id.into(),label:$label.into(),document:$crate::generate::PortalDocument::$document } };
    (reader_layout [$sidebar:expr, $body:expr $(,)?]) => { $crate::generate::Component::ReaderLayout(vec![$sidebar,$body]) };
    (reading_column [$($child:expr),* $(,)?]) => { $crate::generate::Component::ReadingColumn(vec![$($child),*]) };
    (transcript $id:expr, $label:expr) => {
        $crate::component!(transcript $id, $label, "You", "Forge", "Send a message to start this conversation.")
    };
    (transcript $id:expr, $label:expr, $user:expr, $assistant:expr, $empty:expr) => {
        $crate::generate::Component::Transcript {
            id: $id.into(),
            label: $label.into(),
            user_label: $user.into(),
            assistant_label: $assistant.into(),
            empty_message: $empty.into(),
        }
    };
    (text_field $id:expr, $label:expr, $action:expr) => { $crate::generate::Component::TextField{id:$id.into(),label:$label.into(),action:$action.into()} };
    (checkbox $id:expr, $label:expr, $action:expr) => { $crate::generate::Component::Checkbox{id:$id.into(),label:$label.into(),action:$action.into()} };
    (textarea $id:expr, $label:expr, $action:expr) => { $crate::generate::Component::TextArea{id:$id.into(),label:$label.into(),action:$action.into(),placeholder:None} };
    (textarea $id:expr, $label:expr, $action:expr, $placeholder:expr) => { $crate::generate::Component::TextArea{id:$id.into(),label:$label.into(),action:$action.into(),placeholder:Some($placeholder.into())} };
    (toolbar [$($child:expr),* $(,)?]) => { $crate::generate::Component::Toolbar(vec![$($child),*]) };
    (compact_toolbar $id:expr, $label:expr; [$($child:expr),* $(,)?]) => { $crate::generate::Component::CompactToolbar{id:$id.into(),label:$label.into(),children:vec![$($child),*]} };
    (row [$($child:expr),* $(,)?]) => { $crate::generate::Component::Row(vec![$($child),*]) };
    (workspace [$($child:expr),* $(,)?]) => { $crate::generate::Component::Workspace(vec![$($child),*]) };
    (layout $id:expr; [$($child:expr),* $(,)?]) => { $crate::generate::Component::PaneLayout{id:$id.into(),single_secondary:false,header_options:false,children:vec![$($child),*]} };
    (focused_layout $id:expr; [$($child:expr),* $(,)?]) => { $crate::generate::Component::PaneLayout{id:$id.into(),single_secondary:true,header_options:false,children:vec![$($child),*]} };
    (compact_layout $id:expr; [$($child:expr),* $(,)?]) => { $crate::generate::Component::PaneLayout{id:$id.into(),single_secondary:true,header_options:true,children:vec![$($child),*]} };
    (layout_controls [$($child:expr),* $(,)?]) => { $crate::generate::Component::LayoutControls(vec![$($child),*]) };
    (pane_action $button:expr) => { $crate::generate::Component::PaneAction(Box::new($button)) };
    (pane $id:expr, $title:expr, $position:ident, $min:expr, $initial:expr, $max:expr; [$($child:expr),* $(,)?]) => { $crate::generate::Component::Pane{id:$id.into(),title:$title.into(),position:$crate::generate::PanePosition::$position,min:$min,initial:$initial,max:$max,children:vec![$($child),*]} };
    (pane_footer $id:expr; [$($child:expr),* $(,)?]) => { $crate::generate::Component::PaneFooter{id:$id.into(),children:vec![$($child),*]} };
    (text $id:expr, $text:expr) => { $crate::generate::Component::Text{id:$id.into(),text:$text.into(),live:false,style:$crate::generate::TextStyle::Body} };
    (badge $id:expr, $label:expr) => { $crate::generate::Component::Badge{id:$id.into(),label:$label.into()} };
    (metric_cards $id:expr, $label:expr) => { $crate::generate::Component::MetricCards{id:$id.into(),label:$label.into()} };
    (caption $id:expr, $text:expr) => { $crate::generate::Component::Text{id:$id.into(),text:$text.into(),live:false,style:$crate::generate::TextStyle::Caption} };
    (context $id:expr, $text:expr) => { $crate::generate::Component::Text{id:$id.into(),text:$text.into(),live:false,style:$crate::generate::TextStyle::Context} };
    (heading $id:expr, $text:expr) => { $crate::generate::Component::Text{id:$id.into(),text:$text.into(),live:false,style:$crate::generate::TextStyle::Heading} };
    (status $id:expr, $text:expr) => { $crate::generate::Component::Text{id:$id.into(),text:$text.into(),live:true,style:$crate::generate::TextStyle::Body} };
    (select $id:expr, $label:expr, $action:expr) => { $crate::generate::Component::Select{id:$id.into(),label:$label.into(),action:$action.into()} };
    (button $id:expr, $label:expr, $action:expr) => { $crate::generate::Component::Button{id:$id.into(),label:$label.into(),action:$action.into(),emphasis:$crate::generate::ButtonEmphasis::Standard,check_input:None,controls:None,uuid_field:None} };
    (primary $id:expr, $label:expr, $action:expr) => { $crate::generate::Component::Button{id:$id.into(),label:$label.into(),action:$action.into(),emphasis:$crate::generate::ButtonEmphasis::Primary,check_input:None,controls:None,uuid_field:None} };
    (primary_icon $id:expr, $label:expr, $action:expr, $glyph:expr) => { $crate::generate::Component::IconButton{id:$id.into(),label:$label.into(),action:$action.into(),emphasis:$crate::generate::ButtonEmphasis::Primary,glyph:$glyph,check_input:None,uuid_field:None} };
    (uuid_icon $id:expr, $label:expr, $action:expr, $input:expr, $uuid_field:expr, $glyph:expr) => { $crate::generate::Component::IconButton{id:$id.into(),label:$label.into(),action:$action.into(),emphasis:$crate::generate::ButtonEmphasis::Primary,glyph:$glyph,check_input:Some($input.into()),uuid_field:Some($uuid_field.into())} };
    (checked_button $id:expr, $label:expr, $action:expr, $input:expr) => { $crate::generate::Component::Button{id:$id.into(),label:$label.into(),action:$action.into(),emphasis:$crate::generate::ButtonEmphasis::Primary,check_input:Some($input.into()),controls:None,uuid_field:None} };
    (uuid_button $id:expr, $label:expr, $action:expr, $input:expr, $uuid_field:expr) => { $crate::generate::Component::Button{id:$id.into(),label:$label.into(),action:$action.into(),emphasis:$crate::generate::ButtonEmphasis::Primary,check_input:Some($input.into()),controls:None,uuid_field:Some($uuid_field.into())} };
    (uuid_action $id:expr, $label:expr, $action:expr, $uuid_field:expr) => { $crate::generate::Component::Button{id:$id.into(),label:$label.into(),action:$action.into(),emphasis:$crate::generate::ButtonEmphasis::Standard,check_input:None,controls:None,uuid_field:Some($uuid_field.into())} };
    (toggle_panel $id:expr, $label:expr, $action:expr, $controls:expr) => { $crate::generate::Component::Button{id:$id.into(),label:$label.into(),action:$action.into(),emphasis:$crate::generate::ButtonEmphasis::Standard,check_input:None,controls:Some($controls.into()),uuid_field:None} };
    (date_input $id:expr, $label:expr, $action:expr) => { $crate::generate::Component::DateInput{id:$id.into(),label:$label.into(),action:$action.into()} };
    (filter $id:expr, $label:expr, $action:expr) => { $crate::generate::Component::Filter{id:$id.into(),label:$label.into(),action:$action.into(),placeholder:None} };
    (filter $id:expr, $label:expr, $action:expr, $placeholder:expr) => { $crate::generate::Component::Filter{id:$id.into(),label:$label.into(),action:$action.into(),placeholder:Some($placeholder.into())} };
    (reload $id:expr, $label:expr) => { $crate::generate::Component::Reload{id:$id.into(),label:$label.into()} };
    (pair $id:expr, $label:expr, $action:expr) => { $crate::generate::Component::PairForm{id:$id.into(),label:$label.into(),action:$action.into()} };
    (tree $id:expr, $label:expr) => { $crate::generate::Component::Tree{id:$id.into(),label:$label.into(),stacked:false} };
    (stacked_tree $id:expr, $label:expr) => { $crate::generate::Component::Tree{id:$id.into(),label:$label.into(),stacked:true} };
    (source $id:expr, $label:expr) => { $crate::generate::Component::Source{id:$id.into(),label:$label.into()} };
    (diff $id:expr, $label:expr) => { $crate::generate::Component::Diff{id:$id.into(),label:$label.into()} };
    (source_wrap $id:expr, $label:expr, $target:expr) => { $crate::generate::Component::SourceWrap{id:$id.into(),label:$label.into(),target:$target.into()} };
    (source_formats $prefix:expr; [$($child:expr),* $(,)?]) => {
        $crate::component!(source_formats $prefix, "Empty Markdown file.",
            "Markdown display is unavailable for this file size."; [$($child),*])
    };
    (source_formats $prefix:expr, $empty:expr, $unavailable:expr; [$($child:expr),* $(,)?]) => {
        $crate::generate::Component::SourceFormats {
            prefix: $prefix.into(),
            empty_message: $empty.into(),
            unavailable_message: $unavailable.into(),
            children: vec![$($child),*],
        }
    };
    (pane_link $id:expr, $label:expr, $target:expr) => { $crate::generate::Component::PaneLink{id:$id.into(),label:$label.into(),target:$target.into()} };
    (pane_header_link $id:expr, $label:expr, $target:expr, $glyph:expr) => { $crate::generate::Component::PaneHeaderLink{id:$id.into(),label:$label.into(),target:$target.into(),glyph:$glyph} };
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
            parent: Option<&Component>,
        ) -> Result<(), &'static str> {
            let in_layout = matches!(parent, Some(Component::PaneLayout { .. }));
            *count += 1;
            if depth > 12 || *count > 384 {
                return Err("component declaration limit");
            }
            let (id, label, action) = match c {
                Component::Screen { id, title, .. }
                | Component::Navigation { id, title, .. }
                | Component::Panel { id, title, .. }
                | Component::Dialog { id, title, .. }
                | Component::Sheet { id, title, .. }
                | Component::Disclosure { id, title, .. }
                | Component::Pane { id, title, .. } => (Some(id), Some(title), None),
                Component::Row(_)
                | Component::Toolbar(_)
                | Component::ReadingColumn(_)
                | Component::ReaderLayout(_)
                | Component::LayoutControls(_)
                | Component::PaneAction(_)
                | Component::SourceFormats { .. }
                | Component::Workspace(_) => (None, None, None),
                Component::HeaderActions { id, .. }
                | Component::PaneLayout { id, .. }
                | Component::Region { id, .. }
                | Component::PaneFooter { id, .. } => (Some(id), None, None),
                Component::Text { id, text, .. } => (Some(id), Some(text), None),
                Component::Select { id, label, action }
                | Component::Button {
                    id, label, action, ..
                }
                | Component::IconButton {
                    id, label, action, ..
                }
                | Component::TextField { id, label, action }
                | Component::Checkbox { id, label, action }
                | Component::TextArea {
                    id, label, action, ..
                }
                | Component::DateInput { id, label, action }
                | Component::Filter {
                    id, label, action, ..
                }
                | Component::PairForm { id, label, action } => {
                    (Some(id), Some(label), Some(action))
                }
                Component::Badge { id, label }
                | Component::MetricCards { id, label }
                | Component::Transcript { id, label, .. }
                | Component::Tree { id, label, .. }
                | Component::Reload { id, label }
                | Component::CompactToolbar { id, label, .. }
                | Component::Diff { id, label }
                | Component::BrandLink { id, label, .. }
                | Component::NavigationLink { id, label, .. }
                | Component::Source { id, label } => (Some(id), Some(label), None),
                Component::SheetTrigger {
                    id, label, target, ..
                }
                | Component::PaneLink { id, label, target }
                | Component::SourceWrap { id, label, target }
                | Component::PaneHeaderLink {
                    id, label, target, ..
                } => (Some(id), Some(label), Some(target)),
                Component::Details { id }
                | Component::Facts { id }
                | Component::BrandContext { id } => (Some(id), None, None),
            };
            if let Some(id) = id {
                insert(ids, id)?;
            }
            if label.is_some_and(|s| !text(s)) || action.is_some_and(|s| !key(s)) {
                return Err("invalid component label/action");
            }
            match c {
                Component::ReaderLayout(children)
                    if children.len() != 2
                        || children
                            .iter()
                            .any(|child| !matches!(child, Component::Region { .. })) =>
                {
                    return Err("reader layout requires two regions");
                }
                Component::NavigationLink { label, .. }
                    if !matches!(parent, Some(Component::Navigation { .. }))
                        || label.trim().is_empty() =>
                {
                    return Err("navigation link requires a navigation parent and nonempty label");
                }
                Component::Transcript {
                    user_label,
                    assistant_label,
                    empty_message,
                    ..
                } if ![user_label, assistant_label, empty_message]
                    .into_iter()
                    .all(|label| text(label)) =>
                {
                    return Err("invalid transcript copy");
                }
                Component::TextArea {
                    placeholder: Some(placeholder),
                    ..
                }
                | Component::Filter {
                    placeholder: Some(placeholder),
                    ..
                } if !text(placeholder) => {
                    return Err("invalid input placeholder");
                }
                Component::CompactToolbar { id, .. } => insert(ids, &format!("{id}-toggle"))?,
                Component::Button {
                    check_input,
                    controls,
                    uuid_field,
                    ..
                } if [check_input, controls, uuid_field]
                    .into_iter()
                    .flatten()
                    .any(|id| !key(id)) =>
                {
                    return Err("invalid button reference or UUID field");
                }
                Component::IconButton {
                    check_input,
                    uuid_field,
                    ..
                } if [check_input, uuid_field]
                    .into_iter()
                    .flatten()
                    .any(|id| !key(id)) =>
                {
                    return Err("invalid button reference or UUID field");
                }
                Component::Dialog {
                    id,
                    close_action,
                    initial_focus,
                    ..
                } => {
                    if !key(close_action) || !key(initial_focus) {
                        return Err("invalid dialog action or focus reference");
                    }
                    insert(ids, &format!("{id}-title"))?;
                }
                Component::Sheet {
                    id, initial_focus, ..
                } => {
                    if !key(initial_focus) {
                        return Err("invalid sheet focus reference");
                    }
                    for suffix in ["title", "close"] {
                        insert(ids, &format!("{id}-{suffix}"))?;
                    }
                }
                Component::SheetTrigger {
                    glyph: Some(glyph),
                    label,
                    ..
                }
                | Component::IconButton { label, glyph, .. }
                | Component::PaneHeaderLink { label, glyph, .. }
                    if label.trim().is_empty() || glyph.is_control() || glyph.is_whitespace() =>
                {
                    return Err("icon button needs a nonempty label and visible glyph");
                }
                Component::HeaderActions { .. } if depth != 0 => {
                    return Err("header actions must be a document child");
                }
                Component::BrandContext { .. } if depth != 0 => {
                    return Err("brand context must be a document child");
                }
                Component::BrandLink { .. } if depth != 0 => {
                    return Err("brand link must be a document child");
                }
                Component::PaneHeaderLink { .. }
                    if !matches!(
                        parent,
                        Some(Component::Pane { .. } | Component::Region { .. })
                    ) =>
                {
                    return Err("pane header link requires a pane or region parent");
                }
                Component::SourceFormats {
                    prefix,
                    empty_message,
                    unavailable_message,
                    children,
                } => {
                    if !text(empty_message) || !text(unavailable_message) {
                        return Err("invalid source preview copy");
                    }
                    if !key(prefix)
                        || children
                            .iter()
                            .filter(|child| matches!(child, Component::Source { .. }))
                            .count()
                            != 1
                    {
                        return Err("source formats require one direct source view");
                    }
                    for suffix in [
                        "formats",
                        "format",
                        "raw",
                        "raw-pane",
                        "display",
                        "preview",
                        "preview-unavailable",
                    ] {
                        insert(ids, &format!("{prefix}-{suffix}"))?;
                    }
                }
                Component::LayoutControls(_) if !in_layout => {
                    return Err("layout controls must be a direct child of a pane layout");
                }
                Component::PaneAction(button)
                    if !in_layout
                        || !matches!(
                            button.as_ref(),
                            Component::Button { .. } | Component::IconButton { .. }
                        ) =>
                {
                    return Err("pane action requires a button directly in a pane layout");
                }
                Component::PaneAction(button) => visit(button, depth + 1, count, ids, Some(c))?,
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
                    children,
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
                    if children
                        .iter()
                        .filter(|child| matches!(child, Component::PaneHeaderLink { .. }))
                        .count()
                        > 1
                    {
                        return Err("pane allows one header link");
                    }
                    insert(ids, &format!("{id}-heading"))?;
                    insert(ids, &format!("{id}-switch"))?;
                    if *position != PanePosition::Main {
                        for suffix in ["toggle", "close", "separator"] {
                            insert(ids, &format!("{id}-{suffix}"))?;
                        }
                    }
                }
                Component::PaneLayout {
                    id,
                    children,
                    header_options,
                    ..
                } => {
                    for suffix in ["options", "reset"] {
                        insert(ids, &format!("{id}-{suffix}"))?;
                    }
                    if *header_options {
                        for suffix in ["options-title", "options-close"] {
                            insert(ids, &format!("{id}-{suffix}"))?;
                        }
                    }
                    let mut positions = BTreeSet::new();
                    let mut has_controls = false;
                    let mut has_action = false;
                    for child in children {
                        match child {
                            Component::Pane { position, .. } => {
                                if !positions.insert(position.key()) {
                                    return Err("duplicate pane position");
                                }
                            }
                            Component::LayoutControls(_) if !has_controls => has_controls = true,
                            Component::PaneAction(_) if !has_action => has_action = true,
                            _ => {
                                return Err(
                                    "pane layout requires panes and at most one controls group and action",
                                )
                            }
                        }
                    }
                    if !positions.contains("main") {
                        return Err("pane layout requires one main pane");
                    }
                }
                _ => {}
            }
            let children = match c {
                Component::HeaderActions { children, .. }
                | Component::Navigation { children, .. }
                | Component::CompactToolbar { children, .. }
                | Component::Screen { children, .. }
                | Component::Panel { children, .. }
                | Component::Dialog { children, .. }
                | Component::Sheet { children, .. }
                | Component::Disclosure { children, .. }
                | Component::PaneLayout { children, .. }
                | Component::Region { children, .. }
                | Component::Pane { children, .. }
                | Component::PaneFooter { children, .. }
                | Component::SourceFormats { children, .. }
                | Component::Row(children)
                | Component::LayoutControls(children)
                | Component::ReadingColumn(children)
                | Component::ReaderLayout(children)
                | Component::Toolbar(children)
                | Component::Workspace(children) => Some(children),
                _ => None,
            };
            if let Some(children) = children {
                for child in children {
                    visit(child, depth + 1, count, ids, Some(c))?;
                }
            }
            Ok(())
        }
        let mut ids = BTreeSet::from([
            "runtime-recovery".into(),
            "runtime-message".into(),
            "runtime-reload".into(),
            "leave-confirmation".into(),
            "leave-heading".into(),
            "leave-description".into(),
            "leave-stay".into(),
            "leave-confirm".into(),
        ]);
        let mut count = 0;
        let mut has_brand_link = false;
        let mut has_brand_context = false;
        let mut has_header_actions = false;
        for c in &self.children {
            if matches!(c, Component::HeaderActions { .. }) {
                if has_header_actions {
                    return Err("document allows one header actions group");
                }
                has_header_actions = true;
            }
            if matches!(c, Component::BrandLink { .. }) {
                if has_brand_link {
                    return Err("document allows one brand link");
                }
                has_brand_link = true;
            }
            if matches!(c, Component::BrandContext { .. }) {
                if has_brand_context {
                    return Err("document allows one brand context");
                }
                has_brand_context = true;
            }
            visit(c, 0, &mut count, &mut ids, None)?;
        }
        Ok(())
    }
}
