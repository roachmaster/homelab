#![cfg(feature = "build-assets")]
use forge_bootstrap::Registry;
use forge_ide_web::{
    component,
    generate::{
        application::{ide_document, ide_transport, IDE_THEME},
        register_assets, AssetBundle, AssetConfig, Document, HeaderBinding, RequestBinding, Theme,
        Transport,
    },
};
use forge_provider::{Factory, InstanceProvider, Singleton};
use sha2::{Digest, Sha256};
fn config() -> AssetConfig {
    AssetConfig {
        document: ide_document(),
        theme_yaml: IDE_THEME.into(),
        transport: ide_transport(),
        wasm: vec![0, 97, 115, 109, 1, 0, 0, 0],
    }
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
    assert!(html.contains("Counter &lt;example&gt;"));
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
    assert!(Theme::parse(&IDE_THEME.replace("#0c131c", "red;display:none")).is_err());
    assert!(Theme::parse(&IDE_THEME.replace("font_px: 14", "font_px: 900")).is_err());
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
    c.wasm[0] = 1;
    assert!(register_assets("assets", c).is_err());
}
#[test]
fn generated_main_has_no_backend_fetch_or_raw_html_interpretation() {
    let a = build(config());
    let main = std::str::from_utf8(&a.files["main.js"]).unwrap();
    assert!(!main.contains("fetch("));
    assert!(!main.contains("innerHTML"));
    assert!(!main.contains("eval("));
    let html = std::str::from_utf8(&a.files["index.html"]).unwrap();
    assert!(!html.contains("<style"));
    assert!(!html.contains("style="));
    assert!(!html.contains("onclick="));
    let worker = std::str::from_utf8(&a.files["worker.js"]).unwrap();
    assert!(worker.contains("WebAssembly.instantiate"));
    assert!(worker.contains("/forge/v1/dispatch"));
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
        bindings: vec![RequestBinding {
            kind: "load_collection".into(),
            request_id: "request_id".into(),
            headers: vec![HeaderBinding::literal(
                "Forge-Destination",
                "collection.read",
            )],
            csrf: false,
            deadline: None,
            accept_events: vec!["collection_loaded".into()],
        }],
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
    assert!(Theme::parse(&IDE_THEME.replace("row_px: 28", "row_px: 1")).is_err());
    assert!(Theme::parse(&IDE_THEME.replace("control_px: 32", "control_px: 100")).is_err());
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
        "sessionStorage",
        "history.pushState",
        "fetch(",
        "innerHTML",
    ] {
        assert!(
            !main.contains(forbidden),
            "application semantics/unsafe storage in primitive: {forbidden}"
        );
    }
    assert!(main.contains("{...action,value}"));
    assert!(main.contains("window.addEventListener('pagehide',stop)"));
    assert!(
        main.contains("function start(){\n  if(worker||fatalState)return;\n  clearProtected();")
    );
    assert!(main.contains("if(old)old.terminate();clearProtected();"));
    assert!(main.contains("if(worker!==owned||fatalState)return"));
    assert!(main.contains("function fatal(message)"));
    assert!(main.contains("new ResizeObserver"));
    let worker = std::str::from_utf8(&assets.files["worker.js"]).unwrap();
    assert!(worker.contains("wasm.abi_version()!==2"));
    let manifest: serde_json::Value = serde_json::from_slice(&assets.manifest).unwrap();
    assert_eq!(manifest["abi_version"], 2);
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
    assert!(!css.contains("#selection-title"));
    assert!(css.contains("[data-tone=warning]"));
}


#[test]
fn source_shape_uses_declarative_primitives_and_report_local_actions() {
    let assets = build(config());
    let html = std::str::from_utf8(&assets.files["index.html"]).unwrap();
    for id in ["source-shape-panel", "source-shape-summary", "source-shape-details", "source-shape", "shape-previous", "shape-more"] {
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
