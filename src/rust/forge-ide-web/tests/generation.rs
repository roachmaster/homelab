#![cfg(feature = "build-assets")]
use forge_bootstrap::Registry;
use forge_ide_web::{
    component,
    generate::{
        application::{ide_document, ide_transport, IDE_THEME},
        register_assets, AssetBundle, AssetConfig, Document, Theme,
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
