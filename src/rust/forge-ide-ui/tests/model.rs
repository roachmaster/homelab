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
    event(&mut m, UiEvent::SelectEntry { value: 1 });
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
    event(&mut m, UiEvent::SelectDeclaration { value: 1 });
    let v = m.view();
    let details = &v.tables[0].rows;
    assert!(details.iter().any(|p| p.label == "Lines" && p.value == "3"));
    assert!(details
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
    event(&mut m, UiEvent::SelectEntry { value: 1 });
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
    event(&mut m, UiEvent::ToggleEntry { value: 0 });
    assert_eq!(m.view().lists[0].rows.len(), 1);
    event(&mut m, UiEvent::ToggleEntry { value: 0 });
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
    let expanded = event(&mut m, UiEvent::MoreTree {});
    assert_eq!(expanded.view.lists[0].rows.len(), 300);
    let selected = event(&mut m, UiEvent::SelectEntry { value: 1 });
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
    let expanded = event(&mut m, UiEvent::MoreOutline {});
    assert_eq!(expanded.view.lists[1].rows.len(), 300);
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
