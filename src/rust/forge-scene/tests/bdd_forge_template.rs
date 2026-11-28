//! Step bindings for features/forge_template_renderers.feature, over the real
//! `forge_template::assembly` registrar and forge-core providers.

use forge_core::bootstrap::{RegistrationError, Registry};
use forge_core::provider::{Factory, InstanceProvider, Singleton, Transient};
use forge_scene::bdd::{ensure, verify, Steps};
use forge_template::assembly::{register_renderer, RenderConfig, Renderer};
use forge_template::{Limits, MethodRegistry, Schema, Value};

const GREETING: &str = "Hello, {{ .trim().to_uppercase() }}!";

fn config(source: &str) -> RenderConfig {
    RenderConfig::Inline {
        source: source.to_string(),
        schema: Schema::Text,
        limits: Limits::default(),
    }
}

#[derive(Default)]
struct World {
    pages: Vec<(usize, String)>,
    source: String,
    froze: bool,
    first_request: Option<Result<String, String>>,
    rejected: Option<(RegistrationError, usize, String)>,
}

fn renderer(source: &str, methods: MethodRegistry) -> Result<Renderer, String> {
    let group = register_renderer("templates.greeting", config(source), methods)
        .map_err(|e| e.to_string())?;
    let frozen = Registry::new()
        .register(group)
        .map_err(|e| e.to_string())?
        .freeze()
        .map_err(|e| e.to_string())?;
    let ((), factory) = frozen.into_factories();
    factory.create().map_err(|e| e.to_string())
}

fn steps() -> Steps<World> {
    Steps::new()
        .given(
            "a renderer can be shared by every page",
            |_: &mut World, _| Ok(()),
        )
        .given(
            "a template can be registered before it is parsed",
            |_, _| Ok(()),
        )
        .given("a renderer may have custom methods", |_, _| Ok(()))
        .given("a host can register many templates", |_, _| Ok(()))
        .when("two pages are rendered", |w, _| {
            let shared = Singleton::new(renderer(GREETING, MethodRegistry::new())?);
            for _ in 0..2 {
                let Ok(lease) = shared.acquire();
                let page = lease
                    .render(&Value::Text(" Forge ".into()))
                    .map_err(|e| e.to_string())?;
                w.pages.push((lease as *const Renderer as usize, page));
            }
            Ok(())
        })
        .when("its source is broken", |w, _| {
            w.source = "Hello, {{#open}}".into();
            Ok(())
        })
        .when("the host configures none", |w, _| {
            w.source = "{{ .to_uppercase() }}".into();
            Ok(())
        })
        .when("two templates claim the name {}", |w, name| {
            let first = register_renderer(name[0].clone(), config(GREETING), MethodRegistry::new())
                .map_err(|e| e.to_string())?;
            let second = register_renderer(name[0].clone(), config("Bye"), MethodRegistry::new())
                .map_err(|e| e.to_string())?;
            let registry = Registry::new().register(first).map_err(|e| e.to_string())?;
            match registry.register(second) {
                Ok(_) => Err("the second template was accepted".into()),
                Err(rejected) => {
                    let original = rejected.original();
                    w.rejected = Some((
                        rejected.reason().clone(),
                        original.group_count(),
                        original.groups()[0].id().to_string(),
                    ));
                    Ok(())
                }
            }
        })
        .then("both pages come from the same renderer", |w, _| {
            ensure(
                w.pages.len() == 2 && w.pages[0].0 == w.pages[1].0,
                "pages used different renderers",
            )
        })
        .then("both pages read {}", |w, expected| {
            ensure(
                w.pages.iter().all(|(_, page)| page == &expected[0]),
                format!("pages were {:?}", w.pages),
            )
        })
        .then("startup still freezes", |w, _| {
            let group =
                register_renderer("templates.broken", config(&w.source), MethodRegistry::new())
                    .map_err(|e| e.to_string())?;
            let frozen = Registry::new()
                .register(group)
                .map_err(|e| e.to_string())?
                .freeze();
            w.froze = frozen.is_ok();
            let ((), factory) = frozen.map_err(|e| e.to_string())?.into_factories();
            let per_request = Transient::new(factory);
            w.first_request = Some(per_request.acquire().map_err(|e| e.to_string()).and_then(
                |r| {
                    r.render(&Value::Text("x".into()))
                        .map_err(|e| e.to_string())
                },
            ));
            ensure(w.froze, "startup refused a template it has not parsed yet")
        })
        .then("the first page request fails", |w, _| {
            ensure(
                matches!(w.first_request, Some(Err(_))),
                format!("{:?}", w.first_request),
            )
        })
        .then("the builtin methods still work", |w, _| {
            let page = renderer(&w.source, MethodRegistry::new())?
                .render(&Value::Text("quiet".into()))
                .map_err(|e| e.to_string())?;
            ensure(page == "QUIET", format!("rendered {page:?}"))
        })
        .then("the second one is refused", |w, _| match &w.rejected {
            Some((RegistrationError::DuplicateGroup(id), _, _)) => {
                ensure(id == "greeting", format!("refused {id}"))
            }
            other => Err(format!("refusal was {other:?}")),
        })
        .then("the first one keeps its place", |w, _| match &w.rejected {
            Some((_, 1, id)) if id == "greeting" => Ok(()),
            other => Err(format!("registry after refusal: {other:?}")),
        })
}

#[test]
fn forge_template_renderers_feature() {
    let report = verify(
        include_str!("../features/forge_template_renderers.feature"),
        &steps(),
    )
    .unwrap_or_else(|errors| panic!("forge_template_renderers.feature failed:\n{errors}"));
    assert_eq!(report.scenarios, 4);
}
