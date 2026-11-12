use forge_ide_ui::{protocol::*, *};
fn catalog() -> Catalog {
    Catalog {
        recent_write: None,
        protocol_version: 1,
        session_epoch: "session-a".into(),
        timeout_ms: 1000,
        nodes: vec![NodeChoice {
            key: "lab".into(),
            label: "Lab".into(),
            node: "node-a".into(),
            incarnation: "boot-a".into(),
            projects: vec![
                ProjectChoice {
                    alias: "workspace".into(),
                    label: "Workspace".into(),
                },
                ProjectChoice {
                    alias: "other".into(),
                    label: "Other".into(),
                },
            ],
        }],
    }
}
fn event(m: &mut UiModel, e: UiEvent) -> Output {
    m.dispatch(Input {
        now_ms: 100,
        event: e,
    })
    .unwrap()
}
fn connected_chat() -> UiModel {
    let mut m = UiModel::default();
    let o = event(&mut m, UiEvent::Resume {});
    let UiEffect::Resume { request_id } = &o.effects[0] else {
        panic!("resume")
    };
    event(
        &mut m,
        UiEvent::Paired {
            request_id: request_id.clone(),
            catalog: catalog(),
        },
    );
    m
}
fn connected() -> UiModel {
    let mut model = connected_chat();
    event(&mut model, UiEvent::OpenWorkbench {});
    model
}
fn declaration(id: u32, parent: Option<u32>, kind: DeclarationKind, name: &str) -> DeclarationView {
    DeclarationView {
        id,
        parent,
        kind,
        name: name.into(),
        span: SourceSpan {
            start: Position { line: 1, column: 0 },
            end: Position { line: 3, column: 1 },
        },
        module_inline: None,
        implementation: None,
    }
}
fn report() -> ReportView {
    ReportView {
        format_version: 1,
        complete: true,
        files: 1,
        bytes_read: 50,
        entries: vec![
            EntryView {
                id: 0,
                parent: None,
                name: NativeName::Utf8("project".into()),
                kind: EntryKind::Directory,
                size_bytes: None,
                lines: None,
                content_status: String::new(),
                diagnostics: vec![],
                declarations: vec![],
            },
            EntryView {
                id: 1,
                parent: Some(0),
                name: NativeName::Utf8("lib.rs".into()),
                kind: EntryKind::File,
                size_bytes: Some(50),
                lines: Some(3),
                content_status: "text".into(),
                diagnostics: vec![],
                declarations: vec![
                    declaration(0, None, DeclarationKind::Trait, "Tool"),
                    declaration(1, Some(0), DeclarationKind::TraitMethod, "run"),
                ],
            },
        ],
    }
}
fn inspect(m: &mut UiModel) -> PreparedInspection {
    let o = event(m, UiEvent::Inspect {});
    let request = o
        .effects
        .iter()
        .find_map(|e| match e {
            UiEffect::Inspect { request } => Some(request),
            _ => None,
        })
        .expect("inspect");
    request.clone()
}
fn result(p: &PreparedInspection, r: ReportView) -> UiEvent {
    UiEvent::Result {
        result: ResultEvent {
            protocol_version: 1,
            token: p.token.clone(),
            node_key: p.node_key.clone(),
            project: p.project.clone(),
            outcome: Outcome::Report { report: r },
        },
    }
}
fn status(m: &UiModel) -> String {
    m.view()
        .texts
        .into_iter()
        .find(|t| t.id == "status")
        .unwrap()
        .text
}

#[test]
fn sole_project_opens_after_session_even_when_first_node_has_no_projects() {
    let mut m = UiModel::default();
    let resume = event(&mut m, UiEvent::Resume {});
    let UiEffect::Resume { request_id } = &resume.effects[0] else {
        panic!()
    };
    let mut c = catalog();
    c.nodes[0].projects.truncate(1);
    let mut empty = c.nodes[0].clone();
    empty.key = "empty".into();
    empty.projects.clear();
    c.nodes.insert(0, empty);
    let opened = event(
        &mut m,
        UiEvent::Paired {
            request_id: request_id.clone(),
            catalog: c,
        },
    );
    assert_eq!(opened.effects.len(), 1);
    let UiEffect::Inspect { request } = &opened.effects[0] else {
        panic!()
    };
    assert_eq!(request.node_key, "lab");
    assert_eq!(request.project, "workspace");
    assert_eq!(m.phase(), Phase::Loading);
    assert!(m
        .dispatch(Input {
            now_ms: 100,
            event: UiEvent::Inspect {}
        })
        .is_err());
    assert_eq!(
        connected().phase(),
        Phase::Idle,
        "multiple choices remain explicit"
    );
}

fn source_result(request: &PreparedSource, text: String) -> UiEvent {
    UiEvent::SourceResult {
        result: SourceResultEvent {
            protocol_version: PROTOCOL_VERSION,
            token: request.token.clone(),
            report: request.report.clone(),
            node_key: request.node_key.clone(),
            project: request.project.clone(),
            entry: request.entry,
            outcome: SourceOutcome::Source {
                document: SourceDocument {
                    text,
                    sha256: "a".repeat(64),
                },
            },
        },
    }
}
fn source_request(output: &Output) -> PreparedSource {
    output
        .effects
        .iter()
        .find_map(|e| {
            if let UiEffect::Source { request } = e {
                Some(request.clone())
            } else {
                None
            }
        })
        .expect("prepared source")
}

#[test]
fn project_search_crosses_files_and_pages_with_report_guarded_selection() {
    let mut m = connected();
    let p = inspect(&mut m);
    let mut r = report();
    let mut other = r.entries[1].clone();
    other.id = 2;
    other.name = NativeName::Utf8("Other.rs".into());
    other.declarations = (0..300)
        .map(|id| declaration(id, None, DeclarationKind::Function, &format!("Run{id}")))
        .collect();
    r.entries.push(other);
    r.files = 2;
    event(&mut m, result(&p, r));
    let searched = event(
        &mut m,
        UiEvent::SetProjectQuery {
            report: p.token.clone(),
            value: "RUN".into(),
        },
    );
    assert!(searched.effects.is_empty());
    let rows = rows_named(&m, "search-results");
    assert!(!rows.is_empty() && rows.len() <= 256);
    assert!(rows[1].secondary.contains("project / Other.rs"));
    assert!(
        !m.view()
            .visibility
            .iter()
            .find(|v| v.id == "files-panel")
            .unwrap()
            .visible
    );
    let all = collect_paged_rows(
        &mut m,
        "search-results",
        "search-summary",
        "search-more",
        "search-previous",
        301,
    );
    assert_eq!(
        all.iter().map(|r| r.label.clone()).collect::<Vec<_>>(),
        std::iter::once("run".to_string())
            .chain((0..300).map(|id| format!("Run{id}")))
            .collect::<Vec<_>>()
    );
    assert_eq!(rows_named(&m, "search-results"), rows);
    let opened = event(&mut m, action_event(&rows[1].action));
    let source = source_request(&opened);
    assert_eq!(source.entry, 2);
    assert_eq!(opened.view.sources[0].context.declaration, Some(0));
    assert!(opened
        .view
        .panes
        .iter()
        .find(|p| p.id == "workspace-panel")
        .unwrap()
        .reveal
        .is_some());
    let current = m.view();
    assert!(m
        .dispatch(Input {
            now_ms: 100,
            event: UiEvent::SetProjectQuery {
                report: p.token.clone(),
                value: "é".repeat(129)
            }
        })
        .is_err());
    assert_eq!(m.view(), current);
    let fresh = inspect(&mut m);
    event(&mut m, result(&fresh, report()));
    let current = m.view();
    assert!(event(&mut m, action_event(&rows[1].action))
        .effects
        .is_empty());
    assert_eq!(m.view(), current);
}

#[test]
fn source_identity_rejects_stale_files_and_refresh_then_reuses_accepted_document() {
    let mut m = connected();
    let p = inspect(&mut m);
    let mut r = report();
    let mut second = r.entries[1].clone();
    second.id = 2;
    second.name = NativeName::Utf8("other.rs".into());
    r.entries.push(second);
    r.files = 2;
    event(&mut m, result(&p, r));
    let first = source_request(&event(
        &mut m,
        UiEvent::SelectEntry {
            report: p.token.clone(),
            value: 1,
        },
    ));
    let changed = event(
        &mut m,
        UiEvent::SelectEntry {
            report: p.token.clone(),
            value: 2,
        },
    );
    assert!(changed
        .effects
        .iter()
        .any(|e| matches!(e, UiEffect::Abort { token } if *token == first.token)));
    let second = source_request(&changed);
    assert!(event(&mut m, source_result(&first, "wrong file".into()))
        .view
        .sources[0]
        .text
        .is_empty());
    let exact = "fn run() {\n    println!(\"<script>界</script>\");\n}\n";
    let ready = event(&mut m, source_result(&second, exact.into()));
    assert_eq!(ready.view.sources[0].text, exact);
    assert!(event(
        &mut m,
        UiEvent::SelectEntry {
            report: p.token.clone(),
            value: 2
        }
    )
    .effects
    .is_empty());
    let symbol = event(
        &mut m,
        UiEvent::SelectDeclaration {
            report: p.token.clone(),
            entry: 2,
            value: 1,
        },
    );
    assert!(symbol.effects.is_empty());
    assert!(symbol.view.sources[0].target_span.is_some());
    let fresh = inspect(&mut m);
    event(&mut m, result(&fresh, report()));
    let before = m.view();
    event(&mut m, source_result(&second, exact.into()));
    assert_eq!(m.view(), before);
}

#[test]
fn source_pages_are_exact_bounded_and_guarded_and_failures_keep_the_report() {
    let mut m = connected();
    let p = inspect(&mut m);
    let mut r = report();
    r.entries[1].lines = Some(602);
    r.entries[1].declarations[1].span = SourceSpan {
        start: Position {
            line: 600,
            column: 0,
        },
        end: Position {
            line: 601,
            column: 1,
        },
    };
    event(&mut m, result(&p, r));
    let unchanged = m.view();
    assert_eq!(
        m.dispatch(Input {
            now_ms: 9_007_199_254_740_991,
            event: UiEvent::SelectEntry {
                report: p.token.clone(),
                value: 1
            }
        }),
        Err(UiError::InvalidClock)
    );
    assert_eq!(m.view(), unchanged);
    let request = source_request(&event(
        &mut m,
        UiEvent::SelectEntry {
            report: p.token.clone(),
            value: 1,
        },
    ));
    let text = format!("{}tail\n", "line界\n".repeat(600));
    let first = event(&mut m, source_result(&request, text.clone()));
    assert_eq!(first.view.sources[0].first_line, 1);
    let mut reconstructed = first.view.sources[0].text.clone();
    while !m
        .view()
        .buttons
        .iter()
        .find(|b| b.id == "source-more")
        .unwrap()
        .disabled
    {
        let next = event(
            &mut m,
            UiEvent::MoreSource {
                report: p.token.clone(),
                entry: 1,
            },
        );
        reconstructed.push_str(&next.view.sources[0].text);
        assert!(serde_json::to_vec(&next).unwrap().len() < MAX_MESSAGE_BYTES);
    }
    assert_eq!(reconstructed, text);
    event(
        &mut m,
        UiEvent::SelectDeclaration {
            report: p.token.clone(),
            entry: 1,
            value: 1,
        },
    );
    assert!(m.view().sources[0].first_line > 256);
    event(
        &mut m,
        UiEvent::SelectEntry {
            report: p.token.clone(),
            value: 0,
        },
    );
    let retried = source_request(&event(
        &mut m,
        UiEvent::SelectEntry {
            report: p.token.clone(),
            value: 1,
        },
    ));
    let mut mismatched = match source_result(&retried, "bad".into()) {
        UiEvent::SourceResult { result } => result,
        _ => unreachable!(),
    };
    mismatched.entry = 0;
    let rejected = event(&mut m, UiEvent::SourceResult { result: mismatched });
    assert!(rejected.view.sources[0].text.is_empty());
    assert_eq!(m.phase(), Phase::Ready);
    assert!(text_named(&m, "source-status").contains("mismatched"));
    let retry = source_request(&event(
        &mut m,
        UiEvent::SelectEntry {
            report: p.token.clone(),
            value: 1,
        },
    ));
    event(
        &mut m,
        source_result(&retry, "x".repeat(MAX_SOURCE_BYTES + 1)),
    );
    assert!(m.view().sources[0].text.is_empty());
    assert!(text_named(&m, "source-status").contains("oversized"));
}
#[test]
fn session_bootstrap_and_network_failure_do_not_request_a_pairing_code() {
    let mut model = UiModel::default();
    let pair_visible = |m: &UiModel| {
        m.view()
            .visibility
            .iter()
            .find(|v| v.id == "pair-panel")
            .unwrap()
            .visible
    };
    assert!(!pair_visible(&model));
    let output = event(&mut model, UiEvent::Resume {});
    let UiEffect::Resume { request_id } = &output.effects[0] else {
        panic!()
    };
    assert!(!pair_visible(&model));
    event(
        &mut model,
        UiEvent::NetworkFailed {
            request_id: request_id.clone(),
            category: FailureKind::Unavailable,
        },
    );
    assert!(!pair_visible(&model));
    let view = model.view();
    let status = &view.texts.iter().find(|v| v.id == "status").unwrap().text;
    assert!(status.contains("Refresh"));
    assert!(!status.contains("pairing"));
    // A subsequent automatic connection still uses Resume, with no code input.
    let output = event(&mut model, UiEvent::Resume {});
    let UiEffect::Resume { request_id } = &output.effects[0] else {
        panic!()
    };
    event(
        &mut model,
        UiEvent::Paired {
            request_id: request_id.clone(),
            catalog: catalog(),
        },
    );
    assert_eq!(model.phase(), Phase::Idle);
    assert!(!pair_visible(&model));
}
#[test]
fn resume_is_a_rust_effect_and_failure_returns_pairing_view() {
    let mut m = UiModel::default();
    let o = event(&mut m, UiEvent::Resume {});
    let UiEffect::Resume { request_id } = &o.effects[0] else {
        panic!()
    };
    assert_eq!(m.phase(), Phase::Pairing);
    event(
        &mut m,
        UiEvent::NetworkFailed {
            request_id: request_id.clone(),
            category: FailureKind::Denied,
        },
    );
    assert_eq!(m.phase(), Phase::Unpaired);
    assert!(
        m.view()
            .visibility
            .iter()
            .find(|v| v.id == "pair-panel")
            .unwrap()
            .visible
    );
}
#[test]
fn pairing_secret_is_only_in_prepared_effect_not_model_view() {
    let mut m = UiModel::default();
    let secret = "e".repeat(64);
    let o = event(
        &mut m,
        UiEvent::Pair {
            value: secret.clone(),
        },
    );
    assert!(matches!(&o.effects[0],UiEffect::Pair{code,..} if code==&secret));
    assert!(!serde_json::to_string(&m.view()).unwrap().contains(&secret));
    let UiEffect::Pair { request_id, .. } = &o.effects[0] else {
        panic!()
    };
    event(
        &mut m,
        UiEvent::Paired {
            request_id: request_id.clone(),
            catalog: catalog(),
        },
    );
    assert_eq!(m.phase(), Phase::Idle);
}
#[test]
fn bad_catalog_cannot_connect_and_can_retry() {
    let mut m = UiModel::default();
    let o = event(&mut m, UiEvent::Resume {});
    let UiEffect::Resume { request_id } = &o.effects[0] else {
        panic!()
    };
    let mut c = catalog();
    let duplicate = c.nodes[0].projects[0].clone();
    c.nodes[0].projects.push(duplicate);
    event(
        &mut m,
        UiEvent::Paired {
            request_id: request_id.clone(),
            catalog: c,
        },
    );
    assert_eq!(m.phase(), Phase::Unpaired);
    assert!(m
        .dispatch(Input {
            now_ms: 101,
            event: UiEvent::Pair {
                value: "code".into()
            }
        })
        .is_ok());
}
#[test]
fn prepares_only_configured_typed_inspection_and_disables_repeat() {
    let mut m = connected();
    let p = inspect(&mut m);
    assert_eq!(p.destination, "ide.inspect");
    assert_eq!(p.contract_version, 1);
    assert_eq!(p.target_node, "node-a");
    assert_eq!(p.project, "workspace");
    assert_eq!(p.expires_unix_ms, 1100);
    assert_eq!(
        m.dispatch(Input {
            now_ms: 101,
            event: UiEvent::Inspect {}
        }),
        Err(UiError::Busy)
    );
    assert!(
        m.view()
            .buttons
            .iter()
            .find(|b| b.id == "inspect")
            .unwrap()
            .disabled
    );
}
#[test]
fn real_outline_and_details_preserve_hierarchy_lines_and_spans() {
    let mut m = connected();
    let p = inspect(&mut m);
    event(&mut m, result(&p, report()));
    assert_eq!(m.phase(), Phase::Ready);
    event(
        &mut m,
        UiEvent::SelectEntry {
            report: p.token.clone(),
            value: 1,
        },
    );
    let v = m.view();
    let o = v.lists.iter().find(|l| l.id == "outline").unwrap();
    assert_eq!(o.rows.len(), 2);
    assert_eq!(o.rows[1].depth, 1);
    assert_eq!(o.rows[1].label, "run");
    assert!(v
        .texts
        .iter()
        .find(|t| t.id == "summary")
        .unwrap()
        .text
        .contains("1 traits"));
    assert!(v
        .texts
        .iter()
        .find(|t| t.id == "summary")
        .unwrap()
        .text
        .contains("0 free functions · 1 methods"));
    event(
        &mut m,
        UiEvent::SelectDeclaration {
            report: p.token.clone(),
            entry: 1,
            value: 1,
        },
    );
    let v = m.view();
    let details = &v.tables[0].rows;
    assert!(details.iter().any(|p| p.label == "Lines" && p.value == "3"));
    assert!(v.tables[1]
        .rows
        .iter()
        .any(|p| p.label == "Span" && p.value.contains("1:0–3:1")));
}
#[test]
fn selection_change_aborts_and_stale_reply_never_repaints() {
    let mut m = connected();
    let old = inspect(&mut m);
    let o = event(
        &mut m,
        UiEvent::SelectProject {
            value: "other".into(),
        },
    );
    assert!(matches!(&o.effects[0],UiEffect::Abort{token} if token==&old.token));
    let new = inspect(&mut m);
    event(&mut m, result(&old, report()));
    assert_eq!(m.phase(), Phase::Loading);
    assert_eq!(m.pending(), Some(&new));
    event(&mut m, result(&new, report()));
    assert_eq!(m.phase(), Phase::Ready);
}
#[test]
fn cancel_invalidates_before_local_abort_and_retry_is_new_request() {
    let mut m = connected();
    let p = inspect(&mut m);
    let o = event(&mut m, UiEvent::Cancel {});
    assert!(matches!(o.effects[0], UiEffect::Abort { .. }));
    assert_eq!(m.phase(), Phase::Cancelled);
    assert!(status(&m).contains("Remote inspection may still"));
    event(&mut m, result(&p, report()));
    assert_eq!(m.phase(), Phase::Cancelled);
    let next = inspect(&mut m);
    assert_ne!(next.token, p.token);
}
#[test]
fn partial_reports_remain_partial_and_diagnostics_remain_text() {
    let mut m = connected();
    let p = inspect(&mut m);
    let mut r = report();
    r.complete = false;
    r.entries[1]
        .diagnostics
        .push("<img src=x onerror=evil()>".into());
    event(&mut m, result(&p, r));
    assert_eq!(m.phase(), Phase::Partial);
    event(
        &mut m,
        UiEvent::SelectEntry {
            report: p.token.clone(),
            value: 1,
        },
    );
    assert!(m.view().tables[0]
        .rows
        .iter()
        .any(|p| p.value == "<img src=x onerror=evil()>"));
}
#[test]
fn wrong_target_or_expired_success_fails_without_retaining_report() {
    let mut m = connected();
    let p = inspect(&mut m);
    let UiEvent::Result { result: mut wrong } = result(&p, report()) else {
        panic!()
    };
    wrong.project = "other".into();
    event(&mut m, UiEvent::Result { result: wrong });
    assert_eq!(m.phase(), Phase::Failed);
    assert!(m.view().lists[0].rows.is_empty());
    let p = inspect(&mut m);
    m.dispatch(Input {
        now_ms: 1100,
        event: result(&p, report()),
    })
    .unwrap();
    assert_eq!(m.phase(), Phase::Failed);
    assert!(status(&m).contains("Deadline"));
}
#[test]
fn failed_network_request_has_no_hidden_retry_and_stale_failures_are_ignored() {
    let mut m = connected();
    let p = inspect(&mut m);
    let o = event(
        &mut m,
        UiEvent::NetworkFailed {
            request_id: p.token.request_id,
            category: FailureKind::Unavailable,
        },
    );
    assert!(o.effects.is_empty());
    assert_eq!(m.phase(), Phase::Failed);
    let p = inspect(&mut m);
    event(
        &mut m,
        UiEvent::NetworkFailed {
            request_id: "old".into(),
            category: FailureKind::Timeout,
        },
    );
    assert_eq!(m.pending(), Some(&p));
}
#[test]
fn tree_expansion_is_computed_in_rust() {
    let mut m = connected();
    let p = inspect(&mut m);
    event(&mut m, result(&p, report()));
    assert_eq!(m.view().lists[0].rows.len(), 2);
    event(
        &mut m,
        UiEvent::ToggleEntry {
            report: p.token.clone(),
            value: 0,
        },
    );
    assert_eq!(m.view().lists[0].rows.len(), 1);
    event(
        &mut m,
        UiEvent::ToggleEntry {
            report: p.token.clone(),
            value: 0,
        },
    );
    assert_eq!(m.view().lists[0].rows.len(), 2);
}
#[test]
fn invalid_selection_has_no_effect_or_state_change() {
    let mut m = connected();
    let v = m.view();
    assert_eq!(
        m.dispatch(Input {
            now_ms: 100,
            event: UiEvent::SelectNode {
                value: "unknown".into()
            }
        }),
        Err(UiError::InvalidSelection)
    );
    assert_eq!(m.view(), v);
}
#[test]
fn native_names_are_explicit_and_distinct_not_lossy() {
    assert_eq!(
        name_label(&NativeName::UnixBytes(vec![255, 254])),
        "⟦unix bytes: ff fe⟧"
    );
    assert_eq!(
        name_label(&NativeName::WindowsWide(vec![0xd800])),
        "⟦UTF-16 units: d800⟧"
    );
    assert_eq!(name_label(&NativeName::Utf8("a\nb".into())), "a\\nb");
}
#[test]
fn bad_report_indices_counts_and_spans_are_rejected() {
    let mut r = report();
    r.entries[1].parent = Some(1);
    assert!(validate_report(&r).is_err());
    let mut r = report();
    r.files = 2;
    assert!(validate_report(&r).is_err());
    let mut r = report();
    r.entries[1].declarations[1].parent = Some(1);
    assert!(validate_report(&r).is_err());
    let mut r = report();
    r.entries[1].declarations[1].span.start.line = 0;
    assert!(validate_report(&r).is_err());
}
#[test]
fn report_text_and_count_limits_are_enforced() {
    let mut r = report();
    r.entries[1].content_status = "x".repeat(MAX_TEXT_BYTES + 1);
    assert_eq!(validate_report(&r), Err(UiError::Limit));
    let mut r = report();
    r.entries[1].diagnostics = vec!["x".into(); 17];
    assert_eq!(validate_report(&r), Err(UiError::Limit));
    let mut r = report();
    r.entries.resize(MAX_ENTRIES + 1, r.entries[0].clone());
    assert!(validate_report(&r).is_err());
}
#[test]
fn serde_rejects_unknown_fields_invalid_tags_and_negative_ids() {
    assert!(serde_json::from_str::<Input>(
        r#"{"now_ms":0,"event":{"kind":"inspect","url":"http://evil"}}"#
    )
    .is_err());
    assert!(serde_json::from_str::<Input>(
        r#"{"now_ms":0,"event":{"kind":"select_entry","value":-1}}"#
    )
    .is_err());
    assert!(
        serde_json::from_str::<Input>(r#"{"now_ms":0,"event":{"kind":"delete_file"}}"#).is_err()
    );
}

#[test]
fn unreadable_root_is_only_an_incomplete_single_entry_report() {
    let mut r = report();
    r.complete = false;
    r.files = 0;
    r.entries.truncate(1);
    r.entries[0].kind = EntryKind::Unreadable;
    r.entries[0].diagnostics.push("permission denied".into());
    assert_eq!(validate_report(&r), Ok(()));
    let mut m = connected();
    let p = inspect(&mut m);
    event(&mut m, result(&p, r.clone()));
    assert_eq!(m.phase(), Phase::Partial);
    r.complete = true;
    assert_eq!(validate_report(&r), Err(UiError::InvalidReport));
}

#[test]
fn preorder_rejects_reentry_into_closed_entry_and_declaration_branches() {
    let mut r = report();
    r.files = 1;
    let mut branch = r.entries[0].clone();
    branch.id = 1;
    branch.parent = Some(0);
    let mut sibling = branch.clone();
    sibling.id = 2;
    let mut leaf = r.entries[1].clone();
    leaf.id = 3;
    leaf.parent = Some(1);
    r.entries = vec![r.entries[0].clone(), branch, sibling, leaf];
    assert_eq!(validate_report(&r), Err(UiError::InvalidReport));
    let mut r = report();
    r.entries[1].declarations = vec![
        declaration(0, None, DeclarationKind::Trait, "A"),
        declaration(1, None, DeclarationKind::Trait, "B"),
        declaration(2, Some(0), DeclarationKind::TraitMethod, "closed"),
    ];
    assert_eq!(validate_report(&r), Err(UiError::InvalidReport));
}

#[test]
fn tree_and_outline_render_in_bounded_pages_without_dropping_the_report() {
    let mut r = report();
    let template = r.entries[1].clone();
    for id in 2..300 {
        let mut file = template.clone();
        file.id = id;
        file.name = NativeName::Utf8(format!("file{id}.rs"));
        file.declarations.clear();
        r.entries.push(file);
    }
    r.files = 299;
    r.entries[1].declarations = (0..300)
        .map(|id| declaration(id, None, DeclarationKind::Function, &format!("f{id}")))
        .collect();
    let mut m = connected();
    let p = inspect(&mut m);
    event(&mut m, result(&p, r));
    let tree = collect_paged_rows(
        &mut m,
        "files",
        "file-filter-summary",
        "tree-more",
        "tree-previous",
        300,
    );
    assert_eq!(
        tree.iter().map(|r| r.id.clone()).collect::<Vec<_>>(),
        (0..300).map(|id| format!("entry-{id}")).collect::<Vec<_>>()
    );
    event(
        &mut m,
        UiEvent::SelectEntry {
            report: p.token.clone(),
            value: 1,
        },
    );
    let outline = collect_paged_rows(
        &mut m,
        "outline",
        "symbol-filter-summary",
        "outline-more",
        "outline-previous",
        300,
    );
    assert_eq!(
        outline.iter().map(|r| r.id.clone()).collect::<Vec<_>>(),
        (0..300)
            .map(|id| format!("declaration-1-{id}"))
            .collect::<Vec<_>>()
    );
}

#[test]
fn catalog_aggregate_text_and_js_clock_are_bounded() {
    let mut c = catalog();
    c.nodes.clear();
    for i in 0..5 {
        let mut n = catalog().nodes.remove(0);
        n.key = format!("node{i}");
        n.projects = (0..128)
            .map(|j| ProjectChoice {
                alias: format!("p{j}"),
                label: "x".repeat(MAX_TEXT_BYTES),
            })
            .collect();
        c.nodes.push(n);
    }
    assert_eq!(validate_catalog(&c), Err(UiError::Limit));
    assert_eq!(
        UiModel::default().dispatch(Input {
            now_ms: 9_007_199_254_740_992,
            event: UiEvent::Resume {}
        }),
        Err(UiError::InvalidClock)
    );
}

#[test]
fn counts_distinguish_impl_kind_and_methods_from_free_functions() {
    let mut r = report();
    let mut inherent = declaration(2, None, DeclarationKind::Impl, "Thing");
    inherent.implementation = Some(Implementation {
        target: "Thing".into(),
        trait_path: None,
        negative: false,
        is_unsafe: false,
    });
    let mut trait_impl = declaration(4, None, DeclarationKind::Impl, "Tool for Thing");
    trait_impl.implementation = Some(Implementation {
        target: "Thing".into(),
        trait_path: Some("Tool".into()),
        negative: false,
        is_unsafe: false,
    });
    r.entries[1].declarations.extend([
        inherent,
        declaration(3, Some(2), DeclarationKind::ImplMethod, "new"),
        trait_impl,
        declaration(5, Some(4), DeclarationKind::ImplMethod, "run"),
        declaration(6, None, DeclarationKind::Function, "entry"),
    ]);
    let mut m = connected();
    let p = inspect(&mut m);
    let o = event(&mut m, result(&p, r));
    let summary = &o
        .view
        .texts
        .iter()
        .find(|t| t.id == "summary")
        .unwrap()
        .text;
    for expected in [
        "1 traits",
        "1 trait impls",
        "1 inherent impls",
        "1 free functions",
        "3 methods",
    ] {
        assert!(summary.contains(expected), "{summary}");
    }
}

#[test]
fn file_status_fields_and_incompleteness_must_agree() {
    for state in ["rust_unavailable", "unreadable", "too_large"] {
        let mut r = report();
        r.entries[1].content_status = state.into();
        r.entries[1].declarations.clear();
        if state != "rust_unavailable" {
            r.entries[1].lines = None;
        }
        assert_eq!(validate_report(&r), Err(UiError::InvalidReport));
        r.complete = false;
        assert_eq!(validate_report(&r), Ok(()));
    }
    let mut r = report();
    r.entries[1].content_status = "made_up_status".into();
    assert_eq!(validate_report(&r), Err(UiError::InvalidReport));
    r.entries[1].content_status = "text".into();
    r.entries[1].lines = None;
    assert_eq!(validate_report(&r), Err(UiError::InvalidReport));
    r.entries[1].content_status = "binary".into();
    assert_eq!(validate_report(&r), Err(UiError::InvalidReport));
    r.entries[1].declarations.clear();
    assert_eq!(validate_report(&r), Ok(()));
    r.entries[1].size_bytes = None;
    assert_eq!(validate_report(&r), Err(UiError::InvalidReport));
    let mut r = report();
    r.entries[1] = r.entries[0].clone();
    r.entries[1].id = 1;
    r.entries[1].parent = Some(0);
    r.entries[1].kind = EntryKind::Unreadable;
    r.files = 0;
    assert_eq!(validate_report(&r), Err(UiError::InvalidReport));
    r.complete = false;
    assert_eq!(validate_report(&r), Ok(()));
    r.entries[1].kind = EntryKind::Excluded;
    r.complete = true;
    assert_eq!(validate_report(&r), Ok(()));
}

#[test]
fn rejected_input_error_is_visible_without_state_or_effect_mutation() {
    let mut m = connected();
    let request = inspect(&mut m);
    let prior = m.view();
    let error = m
        .dispatch(Input {
            now_ms: 100,
            event: UiEvent::Inspect {},
        })
        .unwrap_err();
    let output = m.output(Some(error.to_string()));
    assert!(output.effects.is_empty());
    assert_eq!(m.pending(), Some(&request));
    assert_eq!(m.phase(), Phase::Loading);
    assert_eq!(m.view(), prior);
    assert!(output
        .view
        .texts
        .iter()
        .find(|t| t.id == "status")
        .unwrap()
        .text
        .contains("request already pending"));
    assert_eq!(output.view.buttons, prior.buttons);
    assert_eq!(output.view.lists, prior.lists);
}

fn local_intents(key: &RequestToken) -> Vec<UiEvent> {
    vec![
        UiEvent::SelectEntry {
            report: key.clone(),
            value: 1,
        },
        UiEvent::ToggleEntry {
            report: key.clone(),
            value: 0,
        },
        UiEvent::SelectDeclaration {
            report: key.clone(),
            entry: 1,
            value: 1,
        },
        UiEvent::SelectProblem {
            report: key.clone(),
            entry: 1,
            value: 0,
        },
        UiEvent::SetFileFilter {
            report: key.clone(),
            value: "lib".into(),
        },
        UiEvent::SetSymbolFilter {
            report: key.clone(),
            entry: 1,
            value: "run".into(),
        },
        UiEvent::MoreTree {
            report: key.clone(),
        },
        UiEvent::MoreOutline {
            report: key.clone(),
            entry: 1,
        },
        UiEvent::MoreShape {
            report: key.clone(),
        },
        UiEvent::PreviousShape {
            report: key.clone(),
        },
        UiEvent::MoreProblems {
            report: key.clone(),
        },
        UiEvent::PreviousTree {
            report: key.clone(),
        },
        UiEvent::PreviousOutline {
            report: key.clone(),
            entry: 1,
        },
        UiEvent::PreviousProblems {
            report: key.clone(),
        },
    ]
}
fn text_named(m: &UiModel, id: &str) -> String {
    m.view()
        .texts
        .into_iter()
        .find(|t| t.id == id)
        .unwrap()
        .text
}
fn rows_named(m: &UiModel, id: &str) -> Vec<view::RowView> {
    m.view()
        .lists
        .into_iter()
        .find(|t| t.id == id)
        .unwrap()
        .rows
}
fn action_event(action: &view::RowAction) -> UiEvent {
    serde_json::from_str(&serde_json::to_string(action).unwrap()).unwrap()
}
/// Exercise the public paging controls without assuming which limit (row count
/// or serialized bytes) ends a page. Prove complete, disjoint coverage, exact
/// range captions, effect-free transitions and reversible cursor history.
fn collect_paged_rows(
    m: &mut UiModel,
    list: &str,
    summary: &str,
    more: &str,
    previous: &str,
    expected: usize,
) -> Vec<view::RowView> {
    let mut all = Vec::new();
    let mut pages = Vec::new();
    loop {
        let rows = rows_named(m, list);
        assert!(!rows.is_empty() && rows.len() <= 256);
        assert!(serde_json::to_vec(&rows).unwrap().len() <= 192 * 1024);
        assert!(text_named(m, summary).starts_with(&format!(
            "Rows {}–{} of {expected}",
            all.len() + 1,
            all.len() + rows.len()
        )));
        all.extend(rows.clone());
        pages.push(rows);
        assert!(
            all.len() <= expected,
            "paging must progress without repeating rows"
        );
        let button = m.view().buttons.into_iter().find(|b| b.id == more).unwrap();
        if button.disabled {
            break;
        }
        assert!(event(m, action_event(button.action.as_ref().unwrap()))
            .effects
            .is_empty());
    }
    assert_eq!(all.len(), expected);
    let unique = all
        .iter()
        .map(|r| &r.id)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        unique.len(),
        expected,
        "no duplicate or omitted rows across windows"
    );
    assert!(pages.len() > 1, "fixture must exercise real pagination");
    for page in pages[..pages.len() - 1].iter().rev() {
        let button = m
            .view()
            .buttons
            .into_iter()
            .find(|b| b.id == previous)
            .unwrap();
        assert!(!button.disabled);
        assert!(event(m, action_event(button.action.as_ref().unwrap()))
            .effects
            .is_empty());
        assert_eq!(&rows_named(m, list), page);
    }
    assert!(
        m.view()
            .buttons
            .iter()
            .find(|b| b.id == previous)
            .unwrap()
            .disabled
    );
    all
}
#[test]
fn every_report_intent_is_keyed_and_old_reused_ids_cannot_mutate_any_local_state() {
    let mut m = connected();
    let old = inspect(&mut m);
    event(&mut m, result(&old, report()));
    let queued = local_intents(&old.token);
    let new = inspect(&mut m);
    assert!(m.view().lists.iter().all(|list| list.rows.is_empty()));
    let loading = m.view();
    for e in &queued {
        assert!(event(&mut m, e.clone()).effects.is_empty());
        assert_eq!(m.view(), loading);
    }
    event(&mut m, result(&new, report()));
    let prior = m.view();
    for e in queued {
        assert!(event(&mut m, e).effects.is_empty());
        assert_eq!(m.view(), prior);
    }
    // Full equality includes all three correlation fields, not only revision.
    for key in [
        RequestToken {
            session_epoch: "other-session".into(),
            ..new.token.clone()
        },
        RequestToken {
            request_id: "other-request".into(),
            ..new.token.clone()
        },
        RequestToken {
            revision: new.token.revision + 1,
            ..new.token.clone()
        },
    ] {
        for e in local_intents(&key) {
            event(&mut m, e);
            assert_eq!(m.view(), prior);
        }
    }
}
#[test]
fn local_wire_actions_require_keys_and_projected_action_codecs_are_typed() {
    for (kind, fields) in [
        ("select_entry", r#", "value":0"#),
        ("toggle_entry", r#", "value":0"#),
        ("select_declaration", r#", "entry":1,"value":0"#),
        ("select_problem", r#", "entry":1,"value":0"#),
        ("set_file_filter", r#", "value":"x"#),
        ("set_symbol_filter", r#", "entry":1,"value":"x"#),
        ("more_tree", ""),
        ("more_outline", r#", "entry":1"#),
        ("more_problems", ""),
        ("more_shape", ""),
        ("previous_shape", ""),
        ("previous_tree", ""),
        ("previous_outline", r#", "entry":1"#),
        ("previous_problems", ""),
    ] {
        assert!(
            serde_json::from_str::<UiEvent>(&format!(r#"{{"kind":"{kind}"{fields}}}"#)).is_err(),
            "{kind} accepted missing report key"
        );
    }
    let mut m = connected();
    let p = inspect(&mut m);
    event(&mut m, result(&p, report()));
    for row in rows_named(&m, "files") {
        event(&mut m, action_event(&row.action));
    }
    let v = m.view();
    for row in &v.lists.iter().find(|l| l.id == "outline").unwrap().rows {
        assert!(
            matches!(action_event(&row.action),UiEvent::SelectDeclaration{report,entry:1,..} if report==p.token)
        );
    }
    for b in v.buttons {
        if let Some(a) = b.action {
            let _: UiEvent = action_event(&a);
        }
    }
}
#[test]
fn old_outline_selection_filter_and_pagination_do_not_follow_a_file_change() {
    let mut r = report();
    let mut second = r.entries[1].clone();
    second.id = 2;
    second.name = NativeName::Utf8("other.rs".into());
    r.entries.push(second);
    r.files = 2;
    let mut m = connected();
    let p = inspect(&mut m);
    event(&mut m, result(&p, r));
    event(
        &mut m,
        UiEvent::SelectEntry {
            report: p.token.clone(),
            value: 1,
        },
    );
    event(
        &mut m,
        UiEvent::SetSymbolFilter {
            report: p.token.clone(),
            entry: 1,
            value: "run".into(),
        },
    );
    event(
        &mut m,
        UiEvent::SelectDeclaration {
            report: p.token.clone(),
            entry: 1,
            value: 1,
        },
    );
    event(
        &mut m,
        UiEvent::SelectEntry {
            report: p.token.clone(),
            value: 2,
        },
    );
    assert!(m
        .view()
        .inputs
        .iter()
        .find(|i| i.id == "symbol-filter")
        .unwrap()
        .value
        .is_empty());
    assert!(m.view().tables[1].rows.is_empty());
    let prior = m.view();
    for e in [
        UiEvent::SelectDeclaration {
            report: p.token.clone(),
            entry: 1,
            value: 1,
        },
        UiEvent::SetSymbolFilter {
            report: p.token.clone(),
            entry: 1,
            value: "old".into(),
        },
        UiEvent::MoreOutline {
            report: p.token.clone(),
            entry: 1,
        },
        UiEvent::PreviousOutline {
            report: p.token.clone(),
            entry: 1,
        },
    ] {
        assert!(event(&mut m, e).effects.is_empty());
        assert_eq!(m.view(), prior);
    }
}
#[test]
fn file_filter_searches_beyond_pages_reveals_ancestors_and_preserves_hidden_selection() {
    let mut r = report();
    let template = r.entries[1].clone();
    let mut dir = r.entries[0].clone();
    dir.id = 1;
    dir.parent = Some(0);
    dir.name = NativeName::Utf8("nested".into());
    r.entries[1] = dir;
    for id in 2..302 {
        let mut e = template.clone();
        e.id = id;
        e.parent = Some(1);
        e.declarations.clear();
        e.name = NativeName::Utf8(format!("file-{id}.rs"));
        r.entries.push(e);
    }
    r.files = 300;
    let mut m = connected();
    let p = inspect(&mut m);
    event(&mut m, result(&p, r));
    event(
        &mut m,
        UiEvent::SelectEntry {
            report: p.token.clone(),
            value: 2,
        },
    );
    event(
        &mut m,
        UiEvent::SetFileFilter {
            report: p.token.clone(),
            value: "file-301".into(),
        },
    );
    let rows = rows_named(&m, "files");
    assert_eq!(
        rows.iter().map(|r| r.label.as_str()).collect::<Vec<_>>(),
        ["project", "nested", "file-301.rs"]
    );
    assert_eq!(rows[2].depth, 2);
    assert!(rows[0].toggle.is_none());
    assert_eq!(text_named(&m, "selection-title"), "file-2.rs");
    assert!(text_named(&m, "workspace-summary").contains("hidden by the file filter"));
    event(
        &mut m,
        UiEvent::SetFileFilter {
            report: p.token.clone(),
            value: "FILE-301".into(),
        },
    );
    assert!(rows_named(&m, "files").is_empty());
    event(
        &mut m,
        UiEvent::SetFileFilter {
            report: p.token.clone(),
            value: String::new(),
        },
    );
    // Filtering does not mutate the user's saved directory expansion.
    assert_eq!(rows_named(&m, "files").len(), 2);
    event(
        &mut m,
        UiEvent::ToggleEntry {
            report: p.token.clone(),
            value: 1,
        },
    );
    let first_page = rows_named(&m, "files");
    let all = collect_paged_rows(
        &mut m,
        "files",
        "file-filter-summary",
        "tree-more",
        "tree-previous",
        302,
    );
    assert_eq!(
        all.iter().map(|r| r.id.clone()).collect::<Vec<_>>(),
        (0..302).map(|id| format!("entry-{id}")).collect::<Vec<_>>()
    );
    assert_eq!(rows_named(&m, "files"), first_page);
}
#[test]
fn filters_are_case_sensitive_safe_display_names_and_bounded_utf8_bytes() {
    let mut r = report();
    r.entries[1].name = NativeName::UnixBytes(vec![0xff, 0xfe]);
    let mut m = connected();
    let p = inspect(&mut m);
    event(&mut m, result(&p, r));
    for (query, count) in [("unix bytes: ff", 2), ("FF", 0), (".rs", 0)] {
        event(
            &mut m,
            UiEvent::SetFileFilter {
                report: p.token.clone(),
                value: query.into(),
            },
        );
        assert_eq!(rows_named(&m, "files").len(), count);
    }
    event(
        &mut m,
        UiEvent::SetFileFilter {
            report: p.token.clone(),
            value: "é".repeat(128),
        },
    );
    let prior = m.view();
    assert_eq!(
        m.dispatch(Input {
            now_ms: 100,
            event: UiEvent::SetFileFilter {
                report: p.token.clone(),
                value: "é".repeat(129)
            }
        }),
        Err(UiError::Limit)
    );
    assert_eq!(m.view(), prior);
    event(
        &mut m,
        UiEvent::SelectEntry {
            report: p.token.clone(),
            value: 1,
        },
    );
    event(
        &mut m,
        UiEvent::SetSymbolFilter {
            report: p.token.clone(),
            entry: 1,
            value: "é".repeat(128),
        },
    );
    let prior = m.view();
    assert_eq!(
        m.dispatch(Input {
            now_ms: 100,
            event: UiEvent::SetSymbolFilter {
                report: p.token,
                entry: 1,
                value: "é".repeat(129)
            }
        }),
        Err(UiError::Limit)
    );
    assert_eq!(m.view(), prior);
    assert_eq!(text_named(&m, "selection-title"), "⟦unix bytes: ff fe⟧");
}
#[test]
fn symbol_filter_retains_ancestor_context_and_hidden_selected_structural_details() {
    let mut r = report();
    r.entries[1].declarations.extend(
        (2..300).map(|id| declaration(id, None, DeclarationKind::Function, &format!("name{id}"))),
    );
    let mut m = connected();
    let p = inspect(&mut m);
    event(&mut m, result(&p, r));
    event(
        &mut m,
        UiEvent::SelectEntry {
            report: p.token.clone(),
            value: 1,
        },
    );
    event(
        &mut m,
        UiEvent::SelectDeclaration {
            report: p.token.clone(),
            entry: 1,
            value: 1,
        },
    );
    event(
        &mut m,
        UiEvent::SetSymbolFilter {
            report: p.token.clone(),
            entry: 1,
            value: "name299".into(),
        },
    );
    assert_eq!(rows_named(&m, "outline").len(), 1);
    assert_eq!(rows_named(&m, "outline")[0].label, "name299");
    assert!(m.view().tables[1].rows.iter().any(|p| p.value == "run"));
    assert!(text_named(&m, "symbol-filter-summary").contains("hidden by filter"));
    event(
        &mut m,
        UiEvent::SetSymbolFilter {
            report: p.token.clone(),
            entry: 1,
            value: "run".into(),
        },
    );
    assert_eq!(
        rows_named(&m, "outline")
            .iter()
            .map(|r| r.label.as_str())
            .collect::<Vec<_>>(),
        ["Tool", "run"]
    );
    assert!(
        rows_named(&m, "declarations").is_empty(),
        "Source is the center; Outline owns declaration navigation"
    );
    assert_eq!(
        text_named(&m, "declarations-summary"),
        text_named(&m, "symbol-filter-summary")
    );
}
#[test]
fn problem_navigation_is_indexed_supplied_text_without_invented_severity_or_span() {
    let mut r = report();
    r.complete = false;
    r.entries[1].diagnostics = vec![
        "<script>not executable</script>".into(),
        "E123: supplied text, not a compiler classification".into(),
    ];
    let mut m = connected();
    let p = inspect(&mut m);
    event(&mut m, result(&p, r));
    let rows = rows_named(&m, "problems");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].label, "<script>not executable</script>");
    assert_eq!(rows[0].kind_label, "supplied diagnostic");
    event(&mut m, action_event(&rows[1].action));
    assert_eq!(text_named(&m, "selection-title"), "lib.rs");
    assert!(m.view().tables[0]
        .rows
        .iter()
        .any(|p| p.label == "Selected diagnostic" && p.value.starts_with("E123:")));
    assert!(m.view().tables[1].rows.is_empty());
    assert_eq!(m.phase(), Phase::Partial);
    assert!(text_named(&m, "problems-summary").contains("partial"));
    let prior = m.view();
    assert_eq!(
        m.dispatch(Input {
            now_ms: 100,
            event: UiEvent::SelectProblem {
                report: p.token.clone(),
                entry: 1,
                value: 2
            }
        }),
        Err(UiError::InvalidAction)
    );
    assert_eq!(m.view(), prior);
    event(
        &mut m,
        UiEvent::SelectDeclaration {
            report: p.token.clone(),
            entry: 1,
            value: 0,
        },
    );
    assert!(!rows_named(&m, "problems").iter().any(|r| r.selected));
    event(&mut m, action_event(&rows[0].action));
    assert!(m.view().tables[1].rows.is_empty());
    let next = inspect(&mut m);
    assert!(text_named(&m, "problems-summary").contains("unavailable"));
    assert!(!text_named(&m, "problems-summary").contains("0 supplied"));
    let mut partial = report();
    partial.complete = false;
    event(&mut m, result(&next, partial));
    assert!(m
        .view()
        .lists
        .iter()
        .find(|l| l.id == "problems")
        .unwrap()
        .empty_message
        .contains("unknown"));
}
#[test]
fn problems_paginate_all_strings_and_context_changes_reset_local_view_state() {
    let mut r = report();
    let template = r.entries[1].clone();
    r.entries.truncate(1);
    for id in 1..22 {
        let mut e = template.clone();
        e.id = id;
        e.diagnostics = (0..16).map(|i| format!("Diagnostic {id}:{i}")).collect();
        e.declarations.clear();
        r.entries.push(e);
    }
    r.files = 21;
    let mut m = connected();
    let p = inspect(&mut m);
    event(&mut m, result(&p, r));
    let all = collect_paged_rows(
        &mut m,
        "problems",
        "problems-summary",
        "problems-more",
        "problems-previous",
        336,
    );
    assert_eq!(
        all.iter().map(|r| r.label.clone()).collect::<Vec<_>>(),
        (1..22)
            .flat_map(|id| (0..16).map(move |i| format!("Diagnostic {id}:{i}")))
            .collect::<Vec<_>>()
    );
    event(
        &mut m,
        UiEvent::SetFileFilter {
            report: p.token.clone(),
            value: "lib".into(),
        },
    );
    event(
        &mut m,
        UiEvent::SelectProject {
            value: "other".into(),
        },
    );
    let prior = m.view();
    assert!(prior
        .inputs
        .iter()
        .all(|i| i.value.is_empty() && i.disabled));
    for e in local_intents(&p.token) {
        assert!(event(&mut m, e).effects.is_empty());
        assert_eq!(m.view(), prior);
    }
    let next = inspect(&mut m);
    event(&mut m, result(&next, report()));
    assert!(m.view().inputs.iter().all(|i| i.value.is_empty()));
}
#[test]
fn generic_denial_stays_in_workbench_and_content_conditions_do_not_invent_success() {
    let mut m = connected();
    let p = inspect(&mut m);
    event(
        &mut m,
        UiEvent::Result {
            result: ResultEvent {
                protocol_version: 1,
                token: p.token,
                node_key: p.node_key,
                project: p.project,
                outcome: Outcome::Error {
                    category: FailureKind::Denied,
                    message: "Not allowed".into(),
                },
            },
        },
    );
    assert_eq!(m.phase(), Phase::Failed);
    assert!(
        m.view()
            .visibility
            .iter()
            .find(|v| v.id == "workbench")
            .unwrap()
            .visible
    );
    assert!(!status(&m).contains("session expired"));
    for (state, expected) in [
        ("rust_unavailable", "unavailable"),
        ("binary", "Binary"),
        ("unreadable", "unknown"),
        ("too_large", "unknown"),
    ] {
        let mut r = report();
        r.complete = state == "binary";
        r.entries[1].content_status = state.into();
        r.entries[1].declarations.clear();
        if state != "rust_unavailable" {
            r.entries[1].lines = None;
        }
        let p = inspect(&mut m);
        event(&mut m, result(&p, r));
        event(
            &mut m,
            UiEvent::SelectEntry {
                report: p.token,
                value: 1,
            },
        );
        assert!(m
            .view()
            .lists
            .iter()
            .find(|l| l.id == "outline")
            .unwrap()
            .empty_message
            .contains(expected));
        assert!(!m.view().tables[0]
            .rows
            .iter()
            .any(|p| p.label == "Declarations in report"));
    }
    let mut r = report();
    r.entries.truncate(1);
    r.files = 0;
    r.bytes_read = 0;
    let p = inspect(&mut m);
    event(&mut m, result(&p, r));
    assert_eq!(m.phase(), Phase::Ready);
    assert_eq!(rows_named(&m, "files").len(), 1);
}
#[test]
fn every_bounded_page_including_native_names_and_escaped_diagnostics_fits_abi_limit() {
    let mut c = catalog();
    c.session_epoch = "s".repeat(128);
    c.nodes[0].label = "\0".repeat(MAX_TEXT_BYTES);
    c.nodes[0].projects = (0..128)
        .map(|i| ProjectChoice {
            alias: format!("project{i}"),
            label: "\0".repeat(MAX_TEXT_BYTES),
        })
        .collect();
    for i in 1..32 {
        let mut n = c.nodes[0].clone();
        n.key = format!("node{i}");
        n.projects.clear();
        c.nodes.push(n);
    }
    let mut m = UiModel::default();
    let startup = event(&mut m, UiEvent::Resume {});
    let UiEffect::Resume { request_id } = &startup.effects[0] else {
        panic!()
    };
    event(
        &mut m,
        UiEvent::Paired {
            request_id: request_id.clone(),
            catalog: c,
        },
    );
    let mut r = report();
    let template = r.entries[1].clone();
    r.entries.truncate(1);
    for id in 1..201 {
        let mut e = template.clone();
        e.id = id;
        e.name = NativeName::UnixBytes(vec![0xff; MAX_TEXT_BYTES]);
        e.declarations.clear();
        if id <= 10 {
            e.diagnostics = vec!["\0".repeat(MAX_TEXT_BYTES); 10];
        }
        r.entries.push(e);
    }
    r.files = 200;
    r.entries[1].declarations = (0..100)
        .map(|i| {
            declaration(
                i,
                None,
                DeclarationKind::Function,
                &"\0".repeat(MAX_TEXT_BYTES),
            )
        })
        .collect();
    let p = inspect(&mut m);
    event(&mut m, result(&p, r));
    event(
        &mut m,
        UiEvent::SelectEntry {
            report: p.token.clone(),
            value: 1,
        },
    );
    event(
        &mut m,
        UiEvent::SelectDeclaration {
            report: p.token.clone(),
            entry: 1,
            value: 0,
        },
    );
    let mut pages = 0;
    loop {
        let output = m.output(None);
        assert!(serde_json::to_vec(&output).unwrap().len() < MAX_MESSAGE_BYTES);
        let next = output
            .view
            .buttons
            .iter()
            .find(|b| matches!(b.id, "tree-more" | "outline-more" | "problems-more") && !b.disabled)
            .and_then(|b| b.action.as_ref())
            .map(action_event);
        match next {
            Some(e) => {
                event(&mut m, e);
                pages += 1;
                assert!(pages < 100);
            }
            None => break,
        }
    }
    assert!(pages > 5, "long names should force byte-bounded pages");
}

#[test]
fn duplicate_basenames_have_diagnostic_context_and_complete_selected_report_path() {
    let mut r = report();
    let root = r.entries[0].clone();
    let file = r.entries[1].clone();
    r.entries.truncate(1);
    for (directory_id, file_id, label) in [(1, 2, "src"), (3, 4, "tests")] {
        let mut directory = root.clone();
        directory.id = directory_id;
        directory.parent = Some(0);
        directory.name = NativeName::Utf8(label.into());
        r.entries.push(directory);
        let mut e = file.clone();
        e.id = file_id;
        e.parent = Some(directory_id);
        e.diagnostics = vec!["supplied diagnostic".into()];
        r.entries.push(e);
    }
    r.files = 2;
    let mut m = connected();
    let p = inspect(&mut m);
    event(&mut m, result(&p, r));
    let problems = rows_named(&m, "problems");
    assert!(problems[0].secondary.contains("src / lib.rs"));
    assert!(problems[1].secondary.contains("tests / lib.rs"));
    assert_ne!(problems[0].secondary, problems[1].secondary);
    event(&mut m, action_event(&problems[1].action));
    assert!(m.view().tables[0]
        .rows
        .iter()
        .any(|p| p.label == "Path in report" && p.value == "project / tests / lib.rs"));
}
#[test]
fn deepest_full_safe_path_and_escaped_catalog_still_leave_room_for_all_five_lists() {
    let mut c = catalog();
    c.nodes[0].label = "\u{1f}".repeat(MAX_TEXT_BYTES);
    c.nodes[0].projects = (0..128)
        .map(|i| ProjectChoice {
            alias: format!("project{i}"),
            label: "\u{1f}".repeat(MAX_TEXT_BYTES),
        })
        .collect();
    for i in 1..32 {
        let mut n = c.nodes[0].clone();
        n.key = format!("node{i}");
        n.projects.clear();
        c.nodes.push(n);
    }
    let mut m = UiModel::default();
    let start = event(&mut m, UiEvent::Resume {});
    let UiEffect::Resume { request_id } = &start.effects[0] else {
        panic!()
    };
    event(
        &mut m,
        UiEvent::Paired {
            request_id: request_id.clone(),
            catalog: c,
        },
    );
    let mut r = report();
    let root = r.entries[0].clone();
    let file = r.entries[1].clone();
    r.entries.clear();
    for id in 0..64 {
        let mut e = root.clone();
        e.id = id;
        e.parent = id.checked_sub(1);
        e.name = NativeName::Utf8("\u{1f}".repeat(MAX_TEXT_BYTES));
        r.entries.push(e);
    }
    for id in 64..200 {
        let mut e = file.clone();
        e.id = id;
        e.parent = Some(63);
        e.name = NativeName::Utf8(format!("{}.rs", "\u{1f}".repeat(MAX_TEXT_BYTES - 3)));
        e.declarations.clear();
        r.entries.push(e);
    }
    r.files = 136;
    r.entries[64].declarations = (0..100)
        .map(|id| {
            declaration(
                id,
                None,
                DeclarationKind::Function,
                &"\u{1f}".repeat(MAX_TEXT_BYTES),
            )
        })
        .collect();
    r.entries[64].diagnostics = vec!["\u{1f}".repeat(MAX_TEXT_BYTES); 16];
    r.entries[64].declarations[0].kind = DeclarationKind::Impl;
    r.entries[64].declarations[0].implementation = Some(Implementation {
        target: "\u{1f}".repeat(MAX_TEXT_BYTES),
        trait_path: Some("\u{1f}".repeat(MAX_TEXT_BYTES)),
        negative: false,
        is_unsafe: false,
    });
    let p = inspect(&mut m);
    event(&mut m, result(&p, r));
    event(
        &mut m,
        UiEvent::SetFileFilter {
            report: p.token.clone(),
            value: "\\u{1f}".into(),
        },
    );
    let selected = event(
        &mut m,
        UiEvent::SelectEntry {
            report: p.token.clone(),
            value: 64,
        },
    );
    let source = source_request(&selected);
    event(
        &mut m,
        source_result(&source, "\u{1f}".repeat(MAX_SOURCE_BYTES)),
    );
    event(
        &mut m,
        UiEvent::SetProjectQuery {
            report: p.token.clone(),
            value: "\u{1f}".into(),
        },
    );
    event(
        &mut m,
        UiEvent::SelectDeclaration {
            report: p.token,
            entry: 64,
            value: 0,
        },
    );
    let output = m.output(None);
    assert!(output
        .view
        .lists
        .iter()
        .filter(|list| !["declarations"].contains(&list.id))
        .all(|list| !list.rows.is_empty()));
    let bytes = serde_json::to_vec(&output).unwrap().len();
    assert!(
        bytes < MAX_MESSAGE_BYTES,
        "complete projected output is {bytes} bytes"
    );
    let path = &output.view.tables[0]
        .rows
        .iter()
        .find(|p| p.label == "Path in report")
        .unwrap()
        .value;
    assert_eq!(path.split(" / ").count(), 65);
}

#[test]
fn status_tones_follow_typed_phase_and_empty_project_availability() {
    use forge_ide_ui::view::Tone;
    let tone = |m: &UiModel| {
        m.view()
            .texts
            .into_iter()
            .find(|t| t.id == "status")
            .unwrap()
            .tone
    };
    let mut m = connected();
    assert_eq!(tone(&m), Tone::Neutral);
    let p = inspect(&mut m);
    assert_eq!(tone(&m), Tone::Accent);
    let mut partial = report();
    partial.complete = false;
    event(&mut m, result(&p, partial));
    assert_eq!(tone(&m), Tone::Warning);
    let p = inspect(&mut m);
    event(&mut m, result(&p, report()));
    assert_eq!(tone(&m), Tone::Accent);
    let rejected = m.output(Some("invalid local input".into()));
    let status = rejected
        .view
        .texts
        .iter()
        .find(|t| t.id == "status")
        .unwrap();
    assert_eq!(status.tone, Tone::Warning);
    assert!(status.text.starts_with("Input rejected:"));
    assert_eq!(
        rejected
            .view
            .texts
            .iter()
            .find(|t| t.id == "phase-label")
            .unwrap()
            .tone,
        Tone::Accent
    );
    assert_eq!(m.phase(), Phase::Ready);
    let p = inspect(&mut m);
    event(
        &mut m,
        UiEvent::NetworkFailed {
            request_id: p.token.request_id,
            category: FailureKind::Unavailable,
        },
    );
    assert_eq!(tone(&m), Tone::Error);

    let mut empty = UiModel::default();
    let output = event(&mut empty, UiEvent::Resume {});
    let UiEffect::Resume { request_id } = &output.effects[0] else {
        panic!("resume")
    };
    let mut c = catalog();
    c.nodes[0].projects.clear();
    event(
        &mut empty,
        UiEvent::Paired {
            request_id: request_id.clone(),
            catalog: c,
        },
    );
    assert_eq!(tone(&empty), Tone::Warning);
    assert_eq!(text_named(&empty, "phase-label"), "No allowed project");
    assert!(
        empty
            .view()
            .buttons
            .iter()
            .find(|b| b.id == "inspect")
            .unwrap()
            .disabled
    );
    assert!(
        !empty
            .view()
            .visibility
            .iter()
            .find(|v| v.id == "cancel")
            .unwrap()
            .visible
    );
}

#[test]
fn source_shape_pagination_is_bounded_reversible_guarded_and_cleared() {
    let mut r = report();
    let template = r.entries[1].clone();
    r.entries.truncate(1);
    for id in 1..301 {
        let mut entry = template.clone();
        entry.id = id;
        entry.name = NativeName::Utf8(format!("source-{id}.rs"));
        entry.declarations.clear();
        r.entries.push(entry);
    }
    r.files = 300;
    r.complete = false;
    let mut m = connected();
    let p = inspect(&mut m);
    event(&mut m, result(&p, r));
    let first = rows_named(&m, "source-shape");
    assert!(!first.is_empty() && first.len() <= 256);
    assert!(text_named(&m, "source-shape-summary").contains("Partial"));
    assert!(first.iter().all(|row| matches!(
        row.action,
        forge_ide_ui::view::RowAction::SelectEntry { .. }
    )));
    let all = collect_paged_rows(
        &mut m,
        "source-shape",
        "source-shape-summary",
        "shape-more",
        "shape-previous",
        301,
    );
    assert_eq!(
        all[1..]
            .iter()
            .map(|row| match row.action {
                view::RowAction::SelectEntry { value, .. } => value,
                _ => panic!("source file selection"),
            })
            .collect::<Vec<_>>(),
        (1..301).collect::<Vec<_>>()
    );
    assert_eq!(rows_named(&m, "source-shape"), first);
    assert!(event(&mut m, action_event(&first[1].action))
        .effects
        .iter()
        .any(|e| matches!(e, UiEffect::Source { .. })));
    assert!(m
        .view()
        .tables
        .iter()
        .find(|table| table.id == "source-shape-details")
        .unwrap()
        .rows
        .iter()
        .any(|p| p.label == "Observed Rust files" && p.value == "300"));
    let next = inspect(&mut m);
    assert!(rows_named(&m, "source-shape").is_empty());
    assert!(m
        .view()
        .tables
        .iter()
        .find(|table| table.id == "source-shape-details")
        .unwrap()
        .rows
        .is_empty());
    event(&mut m, result(&next, report()));
    let prior = m.view();
    event(
        &mut m,
        UiEvent::MoreShape {
            report: p.token.clone(),
        },
    );
    event(&mut m, UiEvent::PreviousShape { report: p.token });
    assert_eq!(m.view(), prior);
}

#[test]
fn workspace_presentation_context_tracks_report_entry_and_declaration() {
    let mut m = connected();
    let p = inspect(&mut m);
    event(&mut m, result(&p, report()));
    let context = |m: &UiModel| {
        m.view()
            .panes
            .into_iter()
            .find(|pane| pane.id == "workspace-panel")
            .unwrap()
            .context
    };
    let root = context(&m);
    assert_eq!(root.report, Some(p.token.clone()));
    assert_eq!(root.entry, Some(0));
    event(
        &mut m,
        UiEvent::SelectEntry {
            report: p.token.clone(),
            value: 1,
        },
    );
    let file = context(&m);
    assert_ne!(file, root);
    event(
        &mut m,
        UiEvent::SelectDeclaration {
            report: p.token.clone(),
            entry: 1,
            value: 0,
        },
    );
    let declaration = context(&m);
    assert_eq!(declaration.declaration, Some(0));
    assert_ne!(declaration, file);
    event(&mut m, UiEvent::MoreShape { report: p.token });
    assert_eq!(context(&m), declaration);
    inspect(&mut m);
    assert_eq!(context(&m).report, None);
    assert_eq!(context(&m).entry, None);
    assert_eq!(context(&m).declaration, None);
}

#[test]
fn shape_navigation_rows_do_not_pretend_multiple_symbols_are_selected() {
    let mut r = report();
    r.entries[1]
        .declarations
        .push(declaration(2, None, DeclarationKind::Struct, "Sample"));
    let mut implementation = declaration(3, None, DeclarationKind::Impl, "Tool for Sample");
    implementation.implementation = Some(Implementation {
        target: "Sample".into(),
        trait_path: Some("Tool".into()),
        negative: false,
        is_unsafe: false,
    });
    r.entries[1].declarations.push(implementation);
    let mut m = connected();
    let p = inspect(&mut m);
    event(&mut m, result(&p, r));
    let candidate = rows_named(&m, "source-shape")
        .into_iter()
        .find(|row| row.kind_label == "candidate struct")
        .unwrap();
    event(&mut m, action_event(&candidate.action));
    let rows = rows_named(&m, "source-shape");
    let selected: Vec<_> = rows.iter().filter(|row| row.selected).collect();
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].id, "shape-source-1");
}

#[test]
fn synthetic_root_uses_admitted_project_label_consistently() {
    let mut model = connected();
    let request = inspect(&mut model);
    let mut accepted = report();
    accepted.entries[0].name = NativeName::Utf8(".".into());
    event(&mut model, result(&request, accepted));
    assert_eq!(rows_named(&model, "files")[0].label, "Workspace");
    let output = event(
        &mut model,
        UiEvent::SelectEntry {
            report: request.token.clone(),
            value: 0,
        },
    );
    assert!(output.effects.is_empty());
    assert_eq!(text_named(&model, "selection-title"), "Workspace");
    let output = event(
        &mut model,
        UiEvent::SetFileFilter {
            report: request.token,
            value: "Workspace".into(),
        },
    );
    assert!(output.effects.is_empty());
    assert_eq!(rows_named(&model, "files").len(), 1);
    assert_eq!(rows_named(&model, "files")[0].label, "Workspace");
}

#[test]
fn chat_is_the_landing_view_and_in_flight_replies_follow_their_conversation() {
    let mut model = connected_chat();
    let visible = |view: &forge_ide_ui::view::ViewModel, id: &str| {
        view.visibility
            .iter()
            .any(|binding| binding.id == id && binding.visible)
    };
    assert!(visible(&model.view(), "chat"));
    assert!(!visible(&model.view(), "workbench"));
    event(
        &mut model,
        UiEvent::SetChatDraft {
            conversation: 0,
            value: "First prompt".into(),
        },
    );
    let sent = event(&mut model, UiEvent::SendChat {});
    let UiEffect::Chat { request } = &sent.effects[0] else {
        panic!("chat effect")
    };
    assert_eq!(request.messages.len(), 1);
    let request_id = request.request_id.clone();
    event(&mut model, UiEvent::NewChat {});
    event(
        &mut model,
        UiEvent::SetChatDraft {
            conversation: 1,
            value: "Other draft".into(),
        },
    );
    let received = event(
        &mut model,
        UiEvent::ChatResult {
            request_id,
            outcome: ChatOutcome::Answer {
                content: "First answer".into(),
            },
        },
    );
    assert_eq!(received.view.transcripts[0].conversation, 1);
    assert!(received.view.transcripts[0].messages.is_empty());
    assert!(received
        .view
        .inputs
        .iter()
        .any(|b| b.id == "chat-draft" && b.value == "Other draft"));

    event(&mut model, UiEvent::OpenWorkbench {});
    assert!(visible(&model.view(), "workbench"));
    event(&mut model, UiEvent::OpenChat {});
    assert!(visible(&model.view(), "chat"));
    let first = event(&mut model, UiEvent::SelectChat { value: "0".into() });
    assert_eq!(
        first.view.transcripts[0].messages[1].content,
        "First answer"
    );
    event(
        &mut model,
        UiEvent::SetChatDraft {
            conversation: 0,
            value: "Follow-up".into(),
        },
    );
    let second = event(&mut model, UiEvent::SendChat {});
    let UiEffect::Chat { request } = &second.effects[0] else {
        panic!("chat effect")
    };
    assert_eq!(
        request
            .messages
            .iter()
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>(),
        ["First prompt", "First answer", "Follow-up"]
    );
    let request_id = request.request_id.clone();
    let failure = event(
        &mut model,
        UiEvent::NetworkFailed {
            request_id: request_id.clone(),
            category: FailureKind::Timeout,
        },
    );
    assert_eq!(failure.view.transcripts[0].messages.len(), 2);
    assert!(failure
        .view
        .inputs
        .iter()
        .any(|b| b.id == "chat-draft" && b.value == "Follow-up"));
    let late = event(
        &mut model,
        UiEvent::ChatResult {
            request_id,
            outcome: ChatOutcome::Answer {
                content: "Late answer".into(),
            },
        },
    );
    assert_eq!(late.view.transcripts[0].messages.len(), 2);
}
