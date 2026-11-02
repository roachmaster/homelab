//! Typed presentation bindings for generated reusable DOM primitives.
use serde::Serialize;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ViewModel {
    pub texts: Vec<TextBinding>,
    pub choices: Vec<SelectBinding>,
    pub buttons: Vec<ButtonBinding>,
    pub lists: Vec<ListBinding>,
    pub tables: Vec<TableBinding>,
    pub visibility: Vec<VisibilityBinding>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TextBinding {
    pub id: &'static str,
    pub text: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OptionView {
    pub value: String,
    pub label: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SelectBinding {
    pub id: &'static str,
    pub value: String,
    pub disabled: bool,
    pub options: Vec<OptionView>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ButtonBinding {
    pub id: &'static str,
    pub disabled: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct VisibilityBinding {
    pub id: &'static str,
    pub visible: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ListBinding {
    pub id: &'static str,
    pub rows: Vec<RowView>,
    pub empty_message: &'static str,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RowView {
    pub id: String,
    pub label: String,
    pub secondary: String,
    pub depth: u16,
    pub selected: bool,
    pub expanded: Option<bool>,
    pub action: RowAction,
    pub toggle: Option<RowAction>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RowAction {
    SelectEntry { value: u32 },
    ToggleEntry { value: u32 },
    SelectDeclaration { value: u32 },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TableBinding {
    pub id: &'static str,
    pub rows: Vec<Property>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Property {
    pub label: String,
    pub value: String,
}
