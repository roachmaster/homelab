//! External transport declaration; availability comes from the scoped catalog.
use crate::{component, generate::Component};

pub(super) fn directory() -> Component {
    component!(region "agents-external-agents"; [
        component!(heading "agents-external-heading", "External agents"),
        component!(panel "agents-muse-db", "Muse"; [
            component!(caption "agents-muse-db-transport", "PostgreSQL · external runner"),
            component!(badge "agents-muse-db-state", "No runner heartbeat"),
            component!(text "agents-muse-db-capabilities", "Muse owns its runner. Forge dispatches assignments and receives plans and review reports through PostgreSQL."),
            component!(caption "agents-muse-db-help", "Select Muse in New task when its runner is available. Refresh checks its advertised capabilities."),
        ]),
    ])
}
