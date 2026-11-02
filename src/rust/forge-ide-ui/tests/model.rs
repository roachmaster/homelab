use forge_ide_ui::{protocol::*, *};
fn catalog() -> Catalog {
    Catalog {
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
fn connected() -> UiModel {
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
    let UiEffect::Inspect { request } = &o.effects[0] else {
        panic!("inspect")
    };
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
    let first = event(&mut m, result(&p, r));
    assert_eq!(first.view.lists[0].rows.len(), 256);
    assert!(
        !first
            .view
            .buttons
            .iter()
            .find(|b| b.id == "tree-more")
            .unwrap()
            .disabled
    );
    let expanded = event(
        &mut m,
        UiEvent::MoreTree {
            report: p.token.clone(),
        },
    );
    assert_eq!(expanded.view.lists[0].rows.len(), 44);
    let selected = event(
        &mut m,
        UiEvent::SelectEntry {
            report: p.token.clone(),
            value: 1,
        },
    );
    assert_eq!(selected.view.lists[1].rows.len(), 256);
    assert!(
        !selected
            .view
            .buttons
            .iter()
            .find(|b| b.id == "outline-more")
            .unwrap()
            .disabled
    );
    let expanded = event(
        &mut m,
        UiEvent::MoreOutline {
            report: p.token.clone(),
            entry: 1,
        },
    );
    assert_eq!(expanded.view.lists[1].rows.len(), 44);
    assert!(
        expanded
            .view
            .buttons
            .iter()
            .find(|b| b.id == "outline-more")
            .unwrap()
            .disabled
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
    assert_eq!(rows_named(&m, "files").len(), 256);
    event(
        &mut m,
        UiEvent::MoreTree {
            report: p.token.clone(),
        },
    );
    assert_eq!(rows_named(&m, "files").len(), 46);
    assert!(text_named(&m, "file-filter-summary").starts_with("Rows 257–302 of 302"));
    event(
        &mut m,
        UiEvent::PreviousTree {
            report: p.token.clone(),
        },
    );
    assert_eq!(rows_named(&m, "files").len(), 256);
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
    assert_eq!(rows_named(&m, "declarations"), rows_named(&m, "outline"));
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
    assert_eq!(rows_named(&m, "problems").len(), 256);
    event(
        &mut m,
        UiEvent::MoreProblems {
            report: p.token.clone(),
        },
    );
    assert_eq!(rows_named(&m, "problems").len(), 80);
    event(
        &mut m,
        UiEvent::PreviousProblems {
            report: p.token.clone(),
        },
    );
    assert_eq!(rows_named(&m, "problems").len(), 256);
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
fn deepest_full_safe_path_and_escaped_catalog_still_leave_room_for_all_four_lists() {
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
        e.name = NativeName::Utf8("\u{1f}".repeat(MAX_TEXT_BYTES));
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
    event(
        &mut m,
        UiEvent::SelectEntry {
            report: p.token.clone(),
            value: 64,
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
    assert!(output.view.lists.iter().all(|list| !list.rows.is_empty()));
    assert!(serde_json::to_vec(&output).unwrap().len() < MAX_MESSAGE_BYTES);
    let path = &output.view.tables[0]
        .rows
        .iter()
        .find(|p| p.label == "Path in report")
        .unwrap()
        .value;
    assert_eq!(path.split(" / ").count(), 65);
}
