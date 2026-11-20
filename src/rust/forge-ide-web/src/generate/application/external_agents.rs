//! Contact declarations, not native worker profiles or usage observations.
use crate::{component, generate::Component};

struct EmailContact {
    name: &'static str,
    recipient: &'static str,
    sender_label: &'static str,
    capability_note: &'static str,
}

// Account identities belong in operator-owned runtime/session configuration,
// not public authored assets. This directory uses a non-identifying label.
const MUSE: EmailContact = EmailContact {
    name: "Muse Email",
    recipient: "roachmaster@muse.ai",
    sender_label: "Your verified Gmail sender",
    capability_note: "Manual email contact. Sender identity verified; sending, replies and background task tracking are not integrated with Forge.",
};

pub(super) fn directory() -> Component {
    component!(region "agents-external-agents"; [
        component!(heading "agents-external-heading", "External agents"),
        component!(panel "agents-muse-email", MUSE.name; [
            component!(caption "agents-muse-email-transport", "Email · manual contact"),
            component!(text "agents-muse-email-recipient", format!("To: {}", MUSE.recipient)),
            component!(caption "agents-muse-email-sender", MUSE.sender_label),
            component!(text "agents-muse-email-capabilities", MUSE.capability_note),
        ]),
    ])
}
