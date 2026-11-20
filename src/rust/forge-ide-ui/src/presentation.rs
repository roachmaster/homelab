//! Host-selected UI availability, separate from admitted backend capabilities.
//! It is never supplied by a browser action or a saved workspace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PresentationPolicy {
    pub chat: bool,
}

pub const PORTAL_PRESENTATION: PresentationPolicy = PresentationPolicy { chat: false };
