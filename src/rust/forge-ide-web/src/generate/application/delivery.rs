//! Delivery is read-only model projection, rendered through native primitives.
use crate::{component, generate::Component};

pub(super) fn panel(prefix: &str, heading: &str) -> Component {
    component!(region prefix; [
        component!(row [
            component!(heading format!("{prefix}-heading"), heading),
            component!(badge format!("{prefix}-state"), "Delivery status"),
        ]),
        component!(caption format!("{prefix}-summary"), ""),
        component!(disclosure format!("{prefix}-details"), "Verification, repair and release"; [
            stage(prefix, "verification", "Main verification"),
            stage(prefix, "repair", "Repair"),
            stage(prefix, "release", "Release"),
            component!(caption format!("{prefix}-source"), ""),
            component!(button format!("{prefix}-repair-task"), "Open repair task", "open_agent_task"),
        ]),
    ])
}

fn stage(prefix: &str, name: &str, label: &str) -> Component {
    component!(region format!("{prefix}-{name}"); [
        component!(row [
            component!(text format!("{prefix}-{name}-label"), label),
            component!(badge format!("{prefix}-{name}-state"), label),
        ]),
        component!(caption format!("{prefix}-{name}-summary"), ""),
    ])
}
