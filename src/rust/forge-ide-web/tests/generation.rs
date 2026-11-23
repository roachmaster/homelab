#![cfg(feature = "build-assets")]
use forge_core::bootstrap::Registry;
use forge_core::provider::{Factory, InstanceProvider, Singleton};
use forge_ide_web::{
    component,
    generate::{
        application::{ide_document, ide_transport, IDE_THEME},
        register_assets, AssetBundle, AssetConfig, Document, HeaderBinding, RequestBinding,
        RequestPolicy, Theme, Transport,
    },
};
use sha2::{Digest, Sha256};
fn config() -> AssetConfig {
    AssetConfig {
        document: ide_document(),
        theme_yaml: IDE_THEME.into(),
        transport: ide_transport(),
        wasm: vec![0, 97, 115, 109, 1, 0, 0, 0],
    }
}
// Mutate the named token through YAML, not a stale copy of its current value.
fn theme_with(key: &str, value: &str) -> String {
    let mut theme: serde_yaml_ng::Value = serde_yaml_ng::from_str(IDE_THEME).unwrap();
    assert!(Theme::parse(IDE_THEME).is_ok());
    assert!(theme.get(key).is_some(), "missing fixture token {key}");
    let previous = theme[key].clone();
    theme[key] = serde_yaml_ng::from_str(value).unwrap();
    assert_ne!(theme[key], previous, "fixture must mutate token {key}");
    serde_yaml_ng::to_string(&theme).unwrap()
}
fn build(c: AssetConfig) -> AssetBundle {
    let group = register_assets("assets", c).unwrap();
    let frozen = Registry::new().register(group).unwrap().freeze().unwrap();
    let ((), factory) = frozen.into_factories();
    let provider = Singleton::new(factory.create().unwrap());
    provider.acquire().unwrap().clone()
}
#[test]
fn public_consumer_composes_registrar_factory_provider_deterministically() {
    let a = build(config());
    let b = build(config());
    assert_eq!(a, b);
    assert_eq!(a.files.len(), 5);
    let manifest: serde_json::Value = serde_json::from_slice(&a.manifest).unwrap();
    for entry in manifest["files"].as_array().unwrap() {
        let p = entry["path"].as_str().unwrap();
        assert_eq!(
            entry["sha256"].as_str().unwrap(),
            format!("{:x}", Sha256::digest(&a.files[p]))
        );
    }
}
#[test]
fn unrelated_component_document_uses_same_emitters_without_ide_business_code() {
    let mut c = config();
    c.document = Document {
        title: "Counter <example>".into(),
        subtitle: "A separate component fixture".into(),
        children: vec![
            component!(panel "counter","Counter";[component!(text "amount","0 < 1 & 2"),component!(button "increment","Increment","increment")]),
        ],
    };
    let assets = build(c);
    let html = std::str::from_utf8(&assets.files["index.html"]).unwrap();
    assert!(html.contains("<title>Counter &lt;example&gt;</title>"));
    assert!(html.contains("<h1>Counter &lt;example&gt;</h1>"));
    assert!(html.contains("0 &lt; 1 &amp; 2"));
    assert!(!html.contains("Project files"));
    assert!(!html.contains("onclick="));
    assert_eq!(assets.files["main.js"], build(config()).files["main.js"]);
}
#[test]
fn yaml_is_tokens_not_raw_css_or_script() {
    for extra in [
        "script: alert(1)\n",
        "css: body{}\n",
        "html: <script>bad</script>\n",
    ] {
        assert!(Theme::parse(&(IDE_THEME.to_owned() + extra)).is_err());
    }
    assert!(Theme::parse(&theme_with("background", "red;display:none")).is_err());
    assert!(Theme::parse(&theme_with("font_px", "900")).is_err());
}
#[test]
fn stylesheet_renders_theme_values_and_keeps_responsive_overrides_in_order() {
    let mut c = config();
    c.theme_yaml = theme_with("background", "'#123456'");
    let assets = build(c);
    let css = std::str::from_utf8(&assets.files["style.css"]).unwrap();
    assert!(css.starts_with(":root{\n--bg:#123456;"));
    assert!(css.contains("content:'';"));
    assert!(css.contains(".fg-depth-64{\nmargin-inline-start:768px;}"));

    let toolbar_rules: Vec<_> = css
        .match_indices(".fg-toolbar>p{")
        .map(|(i, _)| i)
        .collect();
    let mut breakpoints = css.match_indices("@media (max-width:800px){");
    let (first_breakpoint, _) = breakpoints.next().unwrap();
    let (second_breakpoint, _) = breakpoints.next().unwrap();
    assert_eq!(toolbar_rules.len(), 3);
    assert!(toolbar_rules[0] < first_breakpoint && first_breakpoint < toolbar_rules[1]);
    assert!(toolbar_rules[1] < second_breakpoint && second_breakpoint < toolbar_rules[2]);
}
#[test]
fn rejects_collisions_action_code_and_unsafe_transport_before_factory() {
    let mut c = config();
    c.document
        .children
        .push(component!(text "workspace","collision"));
    assert!(register_assets("assets", c).is_err());
    let mut c = config();
    c.document
        .children
        .push(component!(button "x","X","click();evil"));
    assert!(register_assets("assets", c).is_err());
    let mut c = config();
    let h = c.transport.bindings[0].headers[0].clone();
    c.transport.bindings[0].headers.push(h);
    assert!(register_assets("assets", c).is_err());
    let mut c = config();
    c.transport.bindings[0].headers[0].name = "Authorization".into();
    assert!(register_assets("assets", c).is_err());
    let mut c = config();
    c.transport.timers[0].effect_kind = c.transport.bindings[0].kind.clone();
    assert!(register_assets("assets", c).is_err());
    let mut c = config();
    c.transport.timers[0].delay_ms = 0;
    assert!(register_assets("assets", c).is_err());
    let mut c = config();
    c.transport.bindings[0].policy.wait_for_slot_ms = Some(30_001);
    assert!(register_assets("assets", c).is_err());
    let mut c = config();
    c.transport
        .entry_actions
        .insert("mail".into(), "open_mail();".into());
    assert!(register_assets("assets", c).is_err());
    let mut c = config();
    c.transport.entry_actions = (0..17)
        .map(|n| (format!("screen{n}"), "open_chat".into()))
        .collect();
    assert!(register_assets("assets", c).is_err());
    let mut c = config();
    c.wasm[0] = 1;
    assert!(register_assets("assets", c).is_err());
}
#[test]
fn generated_main_has_no_backend_fetch_or_eval_and_one_sanitized_markdown_sink() {
    let a = build(config());
    let main = std::str::from_utf8(&a.files["main.js"]).unwrap();
    assert!(!main.contains("fetch("));
    assert!(!main.contains("eval("));
    assert_eq!(main.matches("innerHTML").count(), 1);
    assert!(main.contains("node.innerHTML=html;"));
    assert!(main.contains("setMarkdown(body,message.rendered_html)"));
    assert!(main.contains("setMarkdown(format.preview,format.html)"));
    assert!(!main.contains("outerHTML"));
    assert!(!main.contains("insertAdjacentHTML"));
    let html = std::str::from_utf8(&a.files["index.html"]).unwrap();
    assert!(!html.contains("<style"));
    assert!(!html.contains("style="));
    assert!(!html.contains("onclick="));
    let worker = std::str::from_utf8(&a.files["worker.js"]).unwrap();
    assert!(worker.contains("WebAssembly.instantiate"));
    assert!(worker.contains("/forge/v1/dispatch"));
}

#[test]
fn guarded_edit_controls_share_the_generated_workbench_with_markdown_formats() {
    let assets = build(config());
    let html = std::str::from_utf8(&assets.files["index.html"]).unwrap();
    let source = html
        .split_once("id=\"source-panel\"")
        .unwrap()
        .1
        .split_once("</section>")
        .unwrap()
        .0;
    let (controls, content) = source
        .split_once("id=\"workbench-source-content\"")
        .unwrap();
    assert!(controls.contains("data-source-wrap aria-controls=\"source-view\""));
    assert!(!content.contains("data-source-wrap"));
    assert!(source.contains("id=\"source-format\""));
    assert!(source.contains("id=\"source-pagination\""));
    assert!(!source.contains("id=\"workbench-edit-diff\""));
    assert!(html.contains("<footer class=\"fg-pane-footer\" id=\"workbench-write-footer\" hidden>"));
    assert!(!html.contains("id=\"send-chat-with-file\""));
    let css = std::str::from_utf8(&assets.files["style.css"]).unwrap();
    assert!(css.contains(".fg-source[data-wrap=true] .fg-source-line{"));
    assert!(!css.contains(".fg-diff-line"));

    let transport = ide_transport();
    let (kind, destination, result) = ("edit_write", "ide.edit_write", "edit_write_result");
    let request = transport
        .bindings
        .iter()
        .find(|binding| binding.kind == kind)
        .unwrap();
    assert!(request.csrf);
    assert_eq!(request.request_id, "request.request_id");
    assert_eq!(request.accept_events, [result]);
    assert!(request
        .headers
        .iter()
        .any(|header| header.name == "Forge-Destination"
            && header.literal.as_deref() == Some(destination)));
}

#[test]
fn application_can_change_worker_policy_without_changing_its_adapter() {
    let original = build(config());
    let mut updated = config();
    updated
        .transport
        .timers
        .iter_mut()
        .find(|timer| timer.effect_kind == "agents_watch")
        .unwrap()
        .delay_ms = 8_000;
    let polls = updated
        .transport
        .bindings
        .iter_mut()
        .find(|binding| binding.kind == "work_list")
        .unwrap();
    polls.policy.delay_ms = Some(2_000);
    polls.policy.wait_for_slot_ms = Some(6_000);
    let updated = build(updated);

    let (original_config, original_adapter) = worker_parts(&original);
    let (updated_config, updated_adapter) = worker_parts(&updated);
    assert_eq!(original_adapter, updated_adapter);
    assert_eq!(original.files["main.js"], updated.files["main.js"]);
    let timer = |config: &serde_json::Value| {
        config["timers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["effect_kind"] == "agents_watch")
            .unwrap()["delay_ms"]
            .as_u64()
            .unwrap()
    };
    assert_eq!(timer(&original_config), 2_500);
    assert_eq!(timer(&updated_config), 8_000);
    let polls = updated_config["bindings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|binding| binding["kind"] == "work_list")
        .unwrap();
    assert_eq!(polls["policy"]["delay_ms"], 2_000);
    assert_eq!(polls["policy"]["wait_for_slot_ms"], 6_000);
}

fn worker_parts(bundle: &AssetBundle) -> (serde_json::Value, &str) {
    let script = std::str::from_utf8(&bundle.files["worker.js"]).unwrap();
    let (config, adapter) = script.split_once('\n').unwrap();
    let config = config.strip_prefix("const CONFIG=").unwrap();
    (
        serde_json::from_str(config.strip_suffix(';').unwrap()).unwrap(),
        adapter,
    )
}

fn widget_document() -> Document {
    Document {
        title: "Collection review".into(),
        subtitle: "Independent generated component consumer".into(),
        children: vec![
            component!(screen "collection-connection", "Collection connection", false; [
                component!(text "collection-status", "Starting"),
            ]),
            component!(screen "collection-screen", "Collection", true; [
                component!(toolbar [
                    component!(select "collection-choice", "Collection", "choose_collection"),
                    component!(button "collection-load", "Load", "load_collection"),
                    component!(reload "collection-reload", "Reload"),
                ]),
                component!(layout "collection-layout"; [
                    component!(pane "collection-nav", "Items", Leading, 180, 240, 380; [
                        component!(filter "collection-filter", "Filter item names", "filter_collection"),
                        component!(tree "collection-items", "Collection items"),
                        component!(button "collection-next", "Next page", "next_collection"),
                    ]),
                    component!(pane "collection-detail", "Selected item", Main, 240, 480, 960; [
                        component!(text "collection-name", "Choose an item"),
                        component!(details "collection-properties"),
                    ]),
                    component!(pane "collection-related", "Related items", Trailing, 220, 260, 420; [
                        component!(tree "collection-relations", "Related items"),
                    ]),
                    component!(pane "collection-notes", "Notes", Bottom, 120, 160, 300; [
                        component!(text "collection-note", "No notes"),
                    ]),
                ]),
            ]),
        ],
    }
}

#[test]
fn unrelated_widget_fixture_exercises_real_macro_and_public_registrar() {
    let mut c = config();
    c.document = widget_document();
    // A distinct consumer supplies its own typed transport declarations too.
    c.transport = Transport {
        initial_event: "mount_collection".into(),
        preserve_workspace_chat: false,
        entry_actions: Default::default(),
        bindings: vec![RequestBinding {
            kind: "load_collection".into(),
            request_id: "request_id".into(),
            headers: vec![HeaderBinding::literal(
                "Forge-Destination",
                "collection.read",
            )],
            csrf: false,
            deadline: None,
            timeout_ms: None,
            error_events: vec![],
            accept_events: vec!["collection_loaded".into()],
            policy: RequestPolicy::default(),
        }],
        timers: vec![],
    };
    let assets = build(c);
    let html = std::str::from_utf8(&assets.files["index.html"]).unwrap();
    assert!(html.contains("data-protected hidden"));
    assert!(html.contains("id=\"collection-layout\""));
    assert!(html.contains("id=\"collection-nav-separator\""));
    assert!(html.contains("aria-valuemin=\"180\""));
    assert!(html.contains("aria-valuemax=\"380\""));
    assert!(html.contains("id=\"collection-nav-toggle\""));
    assert!(html.contains("id=\"collection-nav-switch\""));
    assert!(html.contains("id=\"collection-layout-reset\""));
    assert!(!html.contains("Project files"));
    assert!(!html.contains("Rust declarations"));
    assert_eq!(assets.files["main.js"], build(config()).files["main.js"]);
    assert_eq!(
        assets.files["style.css"],
        build(config()).files["style.css"]
    );
    // Optional test-artifact output for the independent real-browser widget harness.
    if let Some(path) = std::env::var_os("FORGE_WIDGET_FIXTURE_OUTPUT") {
        let path = std::path::PathBuf::from(path);
        std::fs::create_dir_all(&path).unwrap();
        for (name, bytes) in &assets.files {
            std::fs::write(path.join(name), bytes).unwrap();
        }
    }
}

#[test]
fn rejects_invalid_pane_geometry_positions_and_generated_id_collisions() {
    for pane in [
        component!(pane "nav", "Items", Leading, 180, 160, 400; []),
        component!(pane "nav", "Items", Leading, 180, 250, 1200; []),
    ] {
        let mut c = config();
        c.document.children = vec![component!(layout "layout"; [
            pane,
            component!(pane "main", "Main", Main, 240, 480, 960; []),
        ])];
        assert!(register_assets("assets", c).is_err());
    }
    let mut c = config();
    c.document.children = vec![component!(layout "layout"; [
        component!(pane "nav", "Items", Leading, 180, 250, 400; []),
    ])];
    assert!(register_assets("assets", c).is_err());
    let mut c = config();
    c.document.children = vec![component!(pane "orphan", "Items", Main, 240, 480, 960; [])];
    assert!(register_assets("assets", c).is_err());
    let mut c = config();
    c.document
        .children
        .push(component!(text "explorer-panel-separator", "Collision"));
    assert!(register_assets("assets", c).is_err());
    let mut c = config();
    c.document
        .children
        .push(component!(text "runtime-recovery", "Collision"));
    assert!(register_assets("assets", c).is_err());
    assert!(Theme::parse(&theme_with("row_px", "1")).is_err());
    assert!(Theme::parse(&theme_with("control_px", "100")).is_err());
    assert!(Theme::parse(&theme_with("touch_px", "30")).is_err());
}

#[test]
fn presentation_adapter_is_generic_and_recovery_clears_before_resume() {
    let assets = build(config());
    let main = std::str::from_utf8(&assets.files["main.js"]).unwrap();
    for forbidden in [
        "select_entry",
        "more_tree",
        "set_file_filter",
        "session_epoch",
        "localStorage",
        "history.pushState",
        "fetch(",
    ] {
        assert!(
            !main.contains(forbidden),
            "application semantics/unsafe storage in primitive: {forbidden}"
        );
    }
    assert_eq!(main.matches("sessionStorage.").count(), 5);
    assert!(main.contains("const NAVIGATION_CACHE_KEY='forge-workspace-navigation'"));
    assert!(main.contains("sessionStorage.getItem(NAVIGATION_CACHE_KEY)"));
    assert!(main.contains("sessionStorage.setItem(NAVIGATION_CACHE_KEY,cache)"));
    assert!(main.contains("sessionStorage.removeItem(NAVIGATION_CACHE_KEY)"));
    assert!(main.contains("sessionStorage.getItem(key)"));
    assert!(main.contains("sessionStorage.setItem(key,tab)"));
    assert!(main.contains("const key='forge-workspace-tab'"));
    assert!(main.contains("{...action,value}"));
    assert!(main.contains("window.addEventListener('pagehide',()=>{"));
    assert!(main.contains("if(leaveDialog?.open)leaveDialog.close();\n  stop();"));
    assert!(main.contains("leaving=false;\n  clearProtected();"));
    assert!(main.contains("if(old)old.terminate();clearProtected();"));
    assert!(main.contains("if(worker!==owned||fatalState)return"));
    assert!(main.contains("function fatal(message)"));
    assert!(main.contains("new ResizeObserver"));
    let worker = std::str::from_utf8(&assets.files["worker.js"]).unwrap();
    assert!(worker.contains("wasm.abi_version()!==15"));
    let manifest: serde_json::Value = serde_json::from_slice(&assets.manifest).unwrap();
    assert_eq!(manifest["abi_version"], 15);
}

#[test]
fn declarative_hierarchy_emphasis_and_facts_are_reusable_safe_components() {
    let mut c = config();
    let document = Document {
        title: "Record viewer".into(),
        subtitle: "Independent presentation consumer".into(),
        children: vec![
            component!(heading "record-title", "Record <one>"),
            component!(caption "record-hint", "Bounded metadata"),
            component!(context "record-context", "collection / record"),
            component!(primary "record-load", "Load", "load_record"),
            component!(facts "record-facts"),
        ],
    };
    c.document = document.clone();
    let assets = build(c);
    let html = std::str::from_utf8(&assets.files["index.html"]).unwrap();
    assert!(html.contains("<h3 id=\"record-title\""));
    assert!(html.contains("Record &lt;one&gt;"));
    assert!(html.contains("class=\"fg-caption\""));
    assert!(html.contains("class=\"fg-context\""));
    assert!(html.contains("class=\"fg-button fg-button-primary\""));
    assert!(html.contains("class=\"fg-details fg-facts\""));
    assert_eq!(assets.files["main.js"], build(config()).files["main.js"]);
    let mut c = config();
    c.document = document;
    c.document.children.push(component!(facts "record-title"));
    assert!(register_assets("assets", c).is_err());
    let css = std::str::from_utf8(&assets.files["style.css"]).unwrap();
    assert!(!css.contains("p:first-child"));
    assert!(css.contains("[data-tone=warning]"));
}

#[test]
fn source_shape_uses_declarative_primitives_and_report_local_actions() {
    let assets = build(config());
    let html = std::str::from_utf8(&assets.files["index.html"]).unwrap();
    for id in [
        "source-shape-panel",
        "source-shape-summary",
        "source-shape-details",
        "source-shape",
        "shape-previous",
        "shape-more",
    ] {
        assert!(html.contains(&format!("id=\"{id}\"")), "missing {id}");
    }
    assert!(html.contains("previous_shape"));
    assert!(html.contains("more_shape"));
    assert!(html.contains("not a complexity score"));
    assert!(html.contains("not resolved implementations"));
    for name in ["main.js", "worker.js"] {
        let js = std::str::from_utf8(&assets.files[name]).unwrap();
        assert!(!js.contains("Cargo.toml"));
        assert!(!js.contains("candidate_structs"));
    }
}

#[test]
fn repeated_row_actions_preserve_distinct_stable_focus_identity() {
    let assets = build(config());
    let main = std::str::from_utf8(&assets.files["main.js"]).unwrap();
    assert!(main.contains("b.id+':'+key+':select:'+JSON.stringify(row.action)"));
    assert!(main.contains("b.id+':'+key+':toggle:'+JSON.stringify(row.toggle)"));
}

#[test]
fn focused_layout_and_disclosure_are_declarative_reusable_primitives() {
    let mut c = config();
    c.document = Document {
        title: "Review".into(),
        subtitle: "Fixture".into(),
        children: vec![component!(focused_layout "review"; [
            component!(pane "review-main", "Main", Main, 240, 480, 960; [
                component!(disclosure "review-extra", "Details <safe>"; [
                    component!(text "review-text", "Private <text>"),
                ]),
            ]),
            component!(pane "review-related", "Related", Trailing, 220, 280, 480; []),
            component!(pane "review-errors", "Errors", Bottom, 120, 200, 400; []),
        ])],
    };
    let document = c.document.clone();
    let a = build(c);
    let mut repeated = config();
    repeated.document = document;
    assert_eq!(a, build(repeated));
    let html = std::str::from_utf8(&a.files["index.html"]).unwrap();
    assert!(html.contains("data-single-secondary=\"true\""));
    assert!(html.contains("<details id=\"review-extra\" class=\"fg-disclosure\"><summary>Details &lt;safe&gt;</summary>"));
    assert!(html.contains("Private &lt;text&gt;"));
    assert!(!html.contains("onclick="));
    assert_eq!(a.files["main.js"], build(config()).files["main.js"]);
    let mut c = config();
    c.document
        .children
        .push(component!(disclosure "workspace", "Collision"; []));
    assert!(register_assets("assets", c).is_err());
}

#[test]
fn workbench_keeps_project_selects_in_a_closed_native_sheet() {
    let assets = build(config());
    let html = std::str::from_utf8(&assets.files["index.html"]).unwrap();
    let (_, header) = html.split_once("<header").unwrap();
    let (header, _) = header.split_once("</header>").unwrap();
    assert!(header.contains("id=\"workbench-context\""));
    assert!(header.contains("data-sheet-open=\"project-options\""));
    assert!(header.contains("data-sheet-open=\"workspace-options\""));
    for id in ["project-options", "workspace-options"] {
        let (_, sheet) = html.split_once(&format!("<dialog id=\"{id}\"")).unwrap();
        let (attributes, contents) = sheet.split_once('>').unwrap();
        assert!(!attributes.split_whitespace().any(|part| part == "open"));
        assert!(attributes.contains("data-sheet"));
        let (contents, _) = contents.split_once("</dialog>").unwrap();
        assert!(contents.contains(&format!("id=\"{id}-close\"")));
        if id == "project-options" {
            assert!(contents.contains("id=\"node\""));
            assert!(contents.contains("id=\"project\""));
            assert!(!contents.contains("id=\"inspect\""));
        } else {
            assert!(contents.contains("id=\"workspace-reset\""));
        }
    }
    assert!(!html.contains("id=\"file-filter\""));
    assert!(html.contains("id=\"explorer-filter-mode\""));
}

#[test]
fn presentation_sheets_escape_labels_and_reject_derived_id_collisions() {
    let mut c = config();
    c.document = Document {
        title: "Sheet fixture".into(),
        subtitle: "Independent presentation consumer".into(),
        children: vec![
            component!(sheet_trigger "context-open", "Choose <context>", "context-sheet"),
            component!(sheet "context-sheet", "Choose <context>", "context-input"; [
                component!(text_field "context-input", "Context", "set_context"),
            ]),
        ],
    };
    let document = c.document.clone();
    let assets = build(c);
    let html = std::str::from_utf8(&assets.files["index.html"]).unwrap();
    assert!(html.contains("aria-haspopup=\"dialog\""));
    assert!(html.contains("aria-label=\"Choose &lt;context&gt;\""));
    assert!(html.contains("data-initial-focus=\"context-input\""));
    assert!(html.contains("data-sheet-close=\"context-sheet\""));
    assert_eq!(assets.files["main.js"], build(config()).files["main.js"]);
    let mut c = config();
    c.document = document;
    c.document
        .children
        .push(component!(button "context-sheet-close", "Collision", "noop"));
    assert!(register_assets("assets", c).is_err());
}

#[test]
fn workbench_refresh_follows_pane_navigation_as_an_action() {
    let assets = build(config());
    let html = std::str::from_utf8(&assets.files["index.html"]).unwrap();
    let (_, controls) = html.split_once("<div class=\"fg-pane-actions\">").unwrap();
    let (navigation, actions) = controls.split_once("</nav>").unwrap();
    let (_, last_switch) = navigation.rsplit_once("<button").unwrap();
    assert!(last_switch.contains("id=\"problems-panel-switch\""));
    assert!(!navigation.contains("id=\"inspect\""));

    let (refresh, _) = actions.split_once("</button>").unwrap();
    assert!(refresh.starts_with("<button"));
    assert!(refresh.contains("id=\"inspect\""));
    assert!(refresh.contains("data-action=\"inspect\""));
    assert!(refresh.contains("aria-label=\"Refresh project\""));
    assert!(refresh.contains("title=\"Refresh project\""));
    let (_, glyph) = refresh.split_once('>').unwrap();
    assert!(glyph.starts_with("<span"));
    assert!(glyph.contains("aria-hidden=\"true\""));
    assert!(glyph.contains("data-glyph=\"↻\""));
    assert!(glyph.ends_with("></span>"));
    assert!(!refresh.contains("data-pane-switch"));
}

#[test]
fn workbench_places_report_phase_in_inspection_details() {
    let assets = build(config());
    let html = std::str::from_utf8(&assets.files["index.html"]).unwrap();
    let (_, details) = html
        .split_once("<details id=\"inspection-details\"")
        .unwrap();
    let (details, _) = details.split_once("</details>").unwrap();
    assert!(details.contains("id=\"phase-label\""));
    assert!(html.contains("id=\"inspection-actions\""));
}

#[test]
fn source_and_pane_navigation_are_reusable_safe_declarations() {
    let mut c = config();
    c.document = Document {
        title: "Document reader".into(),
        subtitle: "Independent read-only window".into(),
        children: vec![component!(layout "reader"; [
            component!(pane "reader-nav", "Items", Leading, 180, 240, 380; [
                component!(region "reader-results"; [component!(tree "reader-list", "Items")]),
            ]),
            component!(pane "reader-main", "Document", Main, 240, 480, 960; [
                component!(pane_link "reader-back", "Back <safe>", "reader-nav"),
                component!(source "reader-text", "Source <safe>"),
            ]),
        ])],
    };
    let assets = build(c);
    let html = std::str::from_utf8(&assets.files["index.html"]).unwrap();
    assert!(html.contains("data-pane-reveal=\"reader-nav\""));
    assert!(html.contains("Back &lt;safe&gt;"));
    assert!(html.contains("aria-label=\"Source &lt;safe&gt;\""));
    assert!(html.contains("data-source-view tabindex=\"0\""));
    assert!(!html.contains("contenteditable"));
    assert_eq!(assets.files["main.js"], build(config()).files["main.js"]);
    let main = std::str::from_utf8(&assets.files["main.js"]).unwrap();
    assert!(!main.contains("select_search_result"));
    assert!(!main.contains("ide.source"));
    assert!(main.contains("text.textContent=lines[i];"));
    let transport = ide_transport();
    let source = transport
        .bindings
        .iter()
        .find(|b| b.kind == "source")
        .unwrap();
    assert!(source.csrf);
    assert_eq!(source.accept_events, ["source_result"]);
    assert_eq!(source.deadline.as_deref(), Some("request.expires_unix_ms"));
}

#[test]
fn agents_keep_intent_in_rust_and_require_local_checkpoint_before_mutations() {
    let c = config();
    assert_eq!(
        c.transport.entry_actions.get("agents").map(String::as_str),
        Some("open_agents")
    );
    for kind in [
        "work_submit",
        "work_cancel",
        "work_accept",
        "work_resume",
        "work_plan_decide",
        "work_review_decide",
        "work_repair",
        "work_publication_retry",
    ] {
        let binding = c
            .transport
            .bindings
            .iter()
            .find(|b| b.kind == kind)
            .unwrap();
        assert!(binding.csrf);
        assert_eq!(
            binding.policy.local_checkpoint_path.as_deref(),
            Some("retry_intent")
        );
        assert!(binding.policy.expire_session_on_unauthenticated);
        assert!(
            !binding.policy.expire_session_on_denial,
            "a 403 grant failure keeps the browser session"
        );
    }
    let bundle = build(c);
    let main = std::str::from_utf8(&bundle.files["main.js"]).unwrap();
    let worker = std::str::from_utf8(&bundle.files["worker.js"]).unwrap();
    assert!(!main.contains("fetch("));
    assert!(main.contains("navigation_checkpoint_saved"));
    assert!(worker.contains("!await localCheckpoint.pending"));
    assert!(worker.contains("restore_agents_navigation"));
    assert!(worker.contains("wasm.abi_version()!==15"));
}

#[test]
fn component_growth_and_checkpoint_paths_stay_bounded() {
    let mut c = config();
    c.document.children = (0..385)
        .map(|i| component!(text format!("limit-{i}"), "Bounded"))
        .collect();
    assert!(register_assets("assets", c).is_err());
    let mut c = config();
    let binding = c
        .transport
        .bindings
        .iter_mut()
        .find(|b| b.kind == "work_submit")
        .unwrap();
    binding.policy.local_checkpoint_path = Some("retry_intent;execute()".into());
    assert!(register_assets("assets", c).is_err());
}

#[test]
fn agents_replace_mail_without_generating_mail_controls_or_requests() {
    let c = config();
    assert_eq!(
        c.transport.entry_actions.get("mail").map(String::as_str),
        Some("open_agents")
    );
    assert!(c
        .transport
        .bindings
        .iter()
        .all(|b| !b.kind.starts_with("mail_")));
    assert!(c
        .transport
        .timers
        .iter()
        .all(|t| t.effect_kind != "mail_watch"));
    let assets = build(c);
    let html = std::str::from_utf8(&assets.files["index.html"]).unwrap();
    assert!(html.contains("id=\"open-agents\""));
    for removed in ["id=\"open-mail\"", "id=\"mail\"", "id=\"mail-send\""] {
        assert!(
            !html.contains(removed),
            "retired Mail control remains: {removed}"
        );
    }
    let css = std::str::from_utf8(&assets.files["style.css"]).unwrap();
    assert!(!css.contains("#mail"));
    let worker = std::str::from_utf8(&assets.files["worker.js"]).unwrap();
    assert!(worker.contains("if(matchesCache)savedMail=cache.mail??null;"));
}

#[test]
fn native_checkbox_uses_generic_escaped_presentation_contract() {
    let mut c = config();
    c.document = Document {
        title: "Checkbox".into(),
        subtitle: String::new(),
        children: vec![
            component!(checkbox "owner-opt-in","Allow <repairs> & verify", "set_enabled"),
        ],
    };
    let assets = build(c);
    let html = std::str::from_utf8(&assets.files["index.html"]).unwrap();
    assert!(html.contains("type=\"checkbox\""));
    assert!(html.contains("Allow &lt;repairs&gt; &amp; verify"));
    assert!(!html.contains("onclick="));
    let js = std::str::from_utf8(&assets.files["main.js"]).unwrap();
    assert!(js.contains("view.checks||[]"));
    assert!(js.contains("sendAction(n,n.checked,sequence)"));
    assert!(js.contains("container.hidden=b.visible===false"));
}

#[test]
fn autonomous_workflow_uses_typed_regions_and_protected_same_tab_document_link() {
    let assets = build(config());
    let html = std::str::from_utf8(&assets.files["index.html"]).unwrap();
    for id in [
        "agents-manual-assignment",
        "agents-assignment-workflow",
        "agents-workflow",
        "agents-workflow-revisions",
    ] {
        assert!(html.contains(&format!("id=\"{id}\"")));
    }
    assert!(html.contains("href=\"/design/autonomous-agents\" data-workspace-navigation"));
    let main = std::str::from_utf8(&assets.files["main.js"]).unwrap();
    assert!(!main.contains("workflow_policy_digest"));
    assert!(!main.contains("code_review"));
}

#[test]
fn portal_chat_retirement_removes_entry_controls_and_transport_but_preserves_workspace_contract() {
    let config = config();
    assert!(config.transport.preserve_workspace_chat);
    assert_eq!(
        config
            .transport
            .entry_actions
            .get("chat")
            .map(String::as_str),
        Some("open_agents")
    );
    assert!(config
        .transport
        .bindings
        .iter()
        .all(|b| b.kind != "chat" && !b.kind.starts_with("chat_") && b.kind != "propose_edit"));
    let assets = build(config);
    let html = std::str::from_utf8(&assets.files["index.html"]).unwrap();
    for retired in [
        "chat",
        "open-chat",
        "chat-header-actions",
        "chat-draft",
        "workbench-chat",
        "send-workbench-chat",
        "propose-workbench-edit",
    ] {
        assert!(
            !html.contains(&format!("id=\"{retired}\"")),
            "retired control {retired}"
        );
    }
    for retained in [
        "open-agents",
        "open-workbench",
        "open-home",
        "open-design",
        "document-text",
        "save-document",
        "workbench-write-footer",
        "check-workbench-write",
        "undo-workbench-edit",
    ] {
        assert!(
            html.contains(&format!("id=\"{retained}\"")),
            "retained control {retained}"
        );
    }
    let (wire, _) = worker_parts(&assets);
    assert_eq!(wire["preserve_workspace_chat"], true);
}

#[test]
fn external_muse_directory_has_no_native_actions_or_fabricated_usage() {
    let config = config();
    let bindings = config
        .transport
        .bindings
        .iter()
        .map(|b| b.kind.clone())
        .collect::<Vec<_>>();
    let assets = build(config);
    let html = std::str::from_utf8(&assets.files["index.html"]).unwrap();
    let card = html
        .split_once("id=\"agents-muse-db\"")
        .unwrap()
        .1
        .split_once("</section>")
        .unwrap()
        .0;
    for text in [
        "Muse",
        "PostgreSQL · external runner",
        "No runner heartbeat",
        "Muse owns its runner.",
    ] {
        assert!(card.contains(text), "contact detail {text}");
    }
    for unsupported in [
        "<button",
        "<meter",
        "data-action",
        "Idle",
        "Ready",
        "Token",
        "Quota",
    ] {
        assert!(
            !card.contains(unsupported),
            "directory must not imply {unsupported}"
        );
    }
    assert!(bindings
        .iter()
        .all(|b| !b.contains("email") && !b.contains("muse")));
    assert!(bindings.iter().any(|b| b == "work_submit"));
    assert!(bindings.iter().any(|b| b == "usage_overview"));
}

fn minimal_transport() -> Transport {
    Transport {
        initial_event: "mount".into(),
        preserve_workspace_chat: false,
        entry_actions: Default::default(),
        bindings: vec![RequestBinding {
            kind: "load".into(),
            request_id: "request_id".into(),
            headers: vec![HeaderBinding::literal(
                "Forge-Destination",
                "collection.read",
            )],
            csrf: false,
            deadline: None,
            timeout_ms: None,
            error_events: vec![],
            accept_events: vec!["loaded".into()],
            policy: RequestPolicy::default(),
        }],
        timers: vec![],
    }
}

#[test]
fn header_bindings_carry_exactly_one_value_source() {
    let literal = HeaderBinding::literal("Forge-Destination", "collection.read");
    assert_eq!(literal.name, "Forge-Destination");
    assert_eq!(literal.literal.as_deref(), Some("collection.read"));
    assert_eq!(literal.field, None);
    let field = HeaderBinding::field("Forge-Destination", "request.destination");
    assert_eq!(field.name, "Forge-Destination");
    assert_eq!(field.literal, None);
    assert_eq!(field.field.as_deref(), Some("request.destination"));
}

#[test]
fn transport_validation_names_each_failure_exactly() {
    assert_eq!(minimal_transport().validate(), Ok(()));
    let mut transport = minimal_transport();
    transport.bindings.clear();
    assert_eq!(transport.validate(), Err("invalid transport declarations"));
    let mut transport = minimal_transport();
    transport.bindings[0].kind = "abort".into();
    assert_eq!(transport.validate(), Err("invalid request binding"));
    let mut transport = minimal_transport();
    transport.bindings[0].timeout_ms = Some(50);
    assert_eq!(transport.validate(), Err("invalid request binding"));
    let mut transport = minimal_transport();
    transport.bindings[0].policy.delay_ms = Some(0);
    assert_eq!(transport.validate(), Err("invalid request policy"));
    let mut transport = minimal_transport();
    transport.bindings[0].policy.expire_session_on_denial = true;
    assert_eq!(transport.validate(), Err("invalid request policy"));
    let mut transport = minimal_transport();
    transport.bindings[0].headers[0].name = "Authorization".into();
    assert_eq!(transport.validate(), Err("invalid header binding"));
    let mut transport = minimal_transport();
    transport.bindings[0].headers[0].name = "Forge-CSRF".into();
    assert_eq!(transport.validate(), Err("invalid header binding"));
    let mut transport = minimal_transport();
    transport.bindings[0]
        .headers
        .push(HeaderBinding::literal("Forge-Destination", "other.read"));
    assert_eq!(transport.validate(), Err("invalid header binding"));
    let mut transport = minimal_transport();
    transport.bindings[0].headers[0].literal = Some(String::new());
    assert_eq!(
        transport.validate(),
        Err("invalid header value declaration")
    );
    let mut transport = minimal_transport();
    transport
        .timers
        .push(forge_ide_web::generate::TimerBinding {
            effect_kind: "watch".into(),
            event_kind: "tick".into(),
            argument_field: "kind".into(),
            delay_ms: 1_000,
        });
    assert_eq!(transport.validate(), Err("invalid timer binding"));
    let mut transport = minimal_transport();
    transport
        .timers
        .push(forge_ide_web::generate::TimerBinding {
            effect_kind: "watch".into(),
            event_kind: "tick".into(),
            argument_field: "at".into(),
            delay_ms: 0,
        });
    assert_eq!(transport.validate(), Err("invalid timer binding"));
    // A timer that reuses a binding kind collides with the request namespace.
    let mut transport = minimal_transport();
    transport
        .timers
        .push(forge_ide_web::generate::TimerBinding {
            effect_kind: "load".into(),
            event_kind: "tick".into(),
            argument_field: "at".into(),
            delay_ms: 1_000,
        });
    assert_eq!(transport.validate(), Err("invalid timer binding"));
}

#[test]
fn document_and_theme_validation_name_each_failure_exactly() {
    let mut document = Document {
        title: String::new(),
        subtitle: "Fixture".into(),
        children: vec![],
    };
    assert_eq!(document.validate(), Err("invalid document label"));
    document.title = "Fixture".into();
    document.children.push(component!(text "same", "First"));
    document.children.push(component!(text "same", "Second"));
    assert_eq!(
        document.validate(),
        Err("invalid or duplicate component id")
    );
    assert_eq!(
        Theme::parse(&"x".repeat(8193)).unwrap_err(),
        "theme byte limit"
    );
    assert_eq!(
        Theme::parse(&theme_with("background", "'red'")).unwrap_err(),
        "theme colors require six hex digits"
    );
    assert_eq!(
        Theme::parse(&theme_with("font_px", "900")).unwrap_err(),
        "theme numeric limit"
    );
}

#[test]
fn asset_error_display_names_each_failure_exactly() {
    use forge_ide_web::generate::AssetError;
    use std::error::Error;
    assert_eq!(
        AssetError::Configuration("WASM magic/version/byte limit").to_string(),
        "asset configuration: WASM magic/version/byte limit"
    );
    assert_eq!(
        AssetError::Template(forge_template::TemplateError::Limit("output bytes")).to_string(),
        "template output bytes limit exceeded"
    );
    assert!(AssetError::Configuration("x").source().is_none());
    assert!(
        AssetError::Template(forge_template::TemplateError::Limit("output bytes"))
            .source()
            .is_some()
    );
}

#[test]
fn flow_diagram_component_validates_renders_deterministically_and_clears_on_teardown() {
    let mut bad_id = config();
    bad_id.document = Document {
        title: "Fixture".into(),
        subtitle: "Flow".into(),
        children: vec![component!(flow_diagram "bad id", "Valid label")],
    };
    assert!(register_assets("assets", bad_id).is_err());

    let mut bad_label = config();
    bad_label.document = Document {
        title: "Fixture".into(),
        subtitle: "Flow".into(),
        children: vec![component!(flow_diagram "stage-flow", "   ")],
    };
    assert!(register_assets("assets", bad_label).is_err());

    let first = build(config());
    let second = build(config());
    assert_eq!(
        first, second,
        "flow diagram generation must be deterministic"
    );
    let html = std::str::from_utf8(&first.files["index.html"]).unwrap();
    let css = std::str::from_utf8(&first.files["style.css"]).unwrap();
    let main = std::str::from_utf8(&first.files["main.js"]).unwrap();
    let worker = std::str::from_utf8(&first.files["worker.js"]).unwrap();
    let manifest: serde_json::Value = serde_json::from_slice(&first.manifest).unwrap();
    assert!(html.contains(
        "<section id=\"agents-stage-diagram\" class=\"fg-flow-diagram\" aria-label=\"Task event-stream stage diagram\"></section>"
    ));
    assert!(css.contains(".fg-flow-track"));
    assert!(css.contains("@media (max-width:639px)"));
    assert!(css.contains(".fg-flow-node,.fg-flow-trace>summary,.fg-flow-trace>summary::after"));
    assert!(main.contains("function renderFlowDiagram(binding)"));
    assert!(main.contains(".fg-flow-diagram"));
    assert!(main.contains("flowDiagramViews=new WeakMap()"));
    assert!(worker.contains("wasm.abi_version()!==15"));
    assert_eq!(manifest["abi_version"], 15);
}
