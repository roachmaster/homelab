//! The IDE's authored UI: typed components plus a separate YAML theme.
//! No application HTML, CSS, JS, raw-code field or string template lives here.
use super::{Document, HeaderBinding, RequestBinding, Transport};
use crate::component;

pub fn ide_document() -> Document {
    Document {
        title: "Forge IDE".into(),
        subtitle: "Read-only project intelligence · Rust declarations, not guesswork".into(),
        children: vec![
            component!(panel "connection", "Workspace connection"; [
                component!(status "status", "Starting the Rust worker…"),
                component!(panel "pair-panel", "Pair with your local host"; [
                    component!(text "pair-help", "Enter the one-time code shown by your local Forge web host. Agent keys never enter this page."),
                    component!(pair "pair", "Pairing code", "pair"),
                ]),
                component!(row [
                    component!(select "node", "Node", "select_node"),
                    component!(select "project", "Project", "select_project"),
                    component!(button "inspect", "Inspect project", "inspect"),
                    component!(button "cancel", "Cancel local wait", "cancel"),
                ]),
                component!(text "summary", "No report yet"),
            ]),
            component!(workspace [
                component!(panel "explorer-panel", "Explorer"; [
                    component!(tree "files", "Project files"),
                    component!(button "tree-more", "Show more entries", "more_tree"),
                ]),
                component!(panel "outline-panel", "Rust outline"; [
                    component!(text "selection-title", "Select a file to inspect its declarations"),
                    component!(tree "outline", "Rust declarations"),
                    component!(button "outline-more", "Show more declarations", "more_outline"),
                ]),
                component!(panel "details-panel", "Details"; [component!(details "details")]),
            ]),
            component!(text "footer", "Inspection only · No file editing or shell execution · Cancel stops this browser wait, not remote computation"),
        ],
    }
}
pub fn ide_transport() -> Transport {
    let common = vec![
        HeaderBinding::literal("Forge-Envelope-Version", "1"),
        HeaderBinding::literal("Forge-Contract-Version", "1"),
    ];
    let mut pair = common.clone();
    pair.push(HeaderBinding::literal(
        "Forge-Destination",
        "ui.session.open",
    ));
    pair.push(HeaderBinding::field("Forge-Request-Id", "request_id"));
    let mut resume = common.clone();
    resume.push(HeaderBinding::literal(
        "Forge-Destination",
        "ui.session.resume",
    ));
    resume.push(HeaderBinding::field("Forge-Request-Id", "request_id"));
    let mut inspect = common;
    inspect.extend([
        HeaderBinding::field("Forge-Destination", "request.destination"),
        HeaderBinding::field("Forge-Request-Id", "request.token.request_id"),
        HeaderBinding::field("Forge-Target-Node", "request.target_node"),
        HeaderBinding::field("Forge-Target-Incarnation", "request.target_incarnation"),
        HeaderBinding::field("Forge-Expires-Unix-Ms", "request.expires_unix_ms"),
    ]);
    Transport {
        initial_event: "resume".into(),
        bindings: vec![
            RequestBinding {
                kind: "pair".into(),
                request_id: "request_id".into(),
                headers: pair,
                csrf: false,
                deadline: None,
                accept_events: vec!["paired".into(), "pair_failed".into()],
            },
            RequestBinding {
                kind: "resume".into(),
                request_id: "request_id".into(),
                headers: resume,
                csrf: false,
                deadline: None,
                accept_events: vec!["paired".into(), "pair_failed".into()],
            },
            RequestBinding {
                kind: "inspect".into(),
                request_id: "request.token.request_id".into(),
                headers: inspect,
                csrf: true,
                deadline: Some("request.expires_unix_ms".into()),
                accept_events: vec!["result".into()],
            },
        ],
    }
}
pub const IDE_THEME: &str = include_str!("../../ui/theme.yaml");
