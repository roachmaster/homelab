//! The IDE's authored UI: typed components plus a separate YAML theme.
//! No application HTML, CSS, JS, raw-code field or string template lives here.
use super::{Document, HeaderBinding, RequestBinding, Transport};
use crate::component;

pub fn ide_document() -> Document {
    Document {
        title: "Forge IDE".into(),
        subtitle: "Read-only structural inspection".into(),
        children: vec![
            component!(status "status", "Starting the Rust worker…"),
            component!(screen "connection", "Connection", false; [
                component!(panel "pair-panel", "Connect to your Forge host"; [
                    component!(text "pair-help", "Enter the one-time code shown by your local Forge web host. Agent keys never enter this page."),
                    component!(pair "pair", "Pairing code", "pair"),
                ]),
            ]),
            component!(screen "workbench", "Read-only workbench", true; [
                component!(toolbar [
                    component!(select "node", "Node", "select_node"),
                    component!(select "project", "Project", "select_project"),
                    component!(primary "inspect", "Inspect", "inspect"),
                    component!(button "cancel", "Cancel local wait", "cancel"),
                    component!(reload "reload-session", "Reload to check session"),
                    component!(caption "phase-label", "Checking session"),
                ]),
                component!(context "selection-context", ""),
                component!(caption "summary", "No report yet"),
                component!(layout "workspace"; [
                    component!(pane "explorer-panel", "Explorer", Leading, 180, 250, 400; [
                        component!(filter "file-filter", "Filter entry names", "set_file_filter"),
                        component!(caption "file-filter-summary", "Filter applies to the accepted bounded report."),
                        component!(tree "files", "Project files"),
                        component!(row [component!(button "tree-previous", "Previous page", "previous_tree"), component!(button "tree-more", "Next page", "more_tree")]),
                    ]),
                    component!(pane "workspace-panel", "File overview", Main, 240, 480, 960; [
                        component!(heading "selection-title", "Select a file to inspect its declarations"),
                        component!(text "workspace-summary", "Read-only structural inspection. No source body is requested."),
                        component!(panel "declaration-details-panel", "Selected declaration"; [
                            component!(details "declaration-details"),
                        ]),
                        component!(panel "file-metadata-panel", "Entry metadata"; [
                            component!(facts "details"),
                        ]),
                        component!(panel "file-declarations-panel", "Declarations in file"; [
                            component!(caption "declarations-summary", ""),
                            component!(tree "declarations", "File declarations"),
                        ]),
                    ]),
                    component!(pane "outline-panel", "Outline", Trailing, 220, 280, 480; [
                        component!(filter "symbol-filter", "Filter declarations", "set_symbol_filter"),
                        component!(caption "symbol-filter-summary", ""),
                        component!(tree "outline", "Rust declarations"),
                        component!(row [component!(button "outline-previous", "Previous page", "previous_outline"), component!(button "outline-more", "Next page", "more_outline")]),
                    ]),
                    component!(pane "problems-panel", "Problems", Bottom, 120, 200, 400; [
                        component!(caption "problems-summary", "Inspection diagnostics only; not compiler or test results."),
                        component!(tree "problems", "Inspection diagnostics"),
                        component!(row [component!(button "problems-previous", "Previous page", "previous_problems"), component!(button "problems-more", "Next page", "more_problems")]),
                    ]),
                ]),
            ]),
            component!(caption "footer", "Read-only inspection · No editing or execution · Cancel stops this browser wait, not remote computation"),
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
