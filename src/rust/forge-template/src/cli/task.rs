//! Command-owned orchestration. I/O clients receive prepared boundary requests;
//! a narrow typed construction port is injected instead of a service locator.
use super::{
    input::{RenderRequest, Scalar, Source},
    io::{ReadSource, SourceError},
    CliError,
};
use crate::{
    assembly::{RenderConfig, Renderer},
    Field, Limits, Schema, Value,
};
use forge_io_client::IoClient;
use forge_task::Task;
use std::{collections::BTreeMap, rc::Rc};

pub(super) trait RendererFactory {
    fn create(&self, config: RenderConfig) -> Result<Renderer, CliError>;
}
pub(super) struct RenderTask<C, F> {
    pub source: Rc<C>,
    pub factory: F,
}
impl<C, F> Task for RenderTask<C, F>
where
    C: IoClient<Request = ReadSource, Response = String, Error = SourceError>,
    F: RendererFactory,
{
    type Input = RenderRequest;
    type Output = String;
    type Error = CliError;
    fn run(&self, request: RenderRequest) -> Result<String, CliError> {
        let limits = Limits::default();
        if request.bindings.len() > 128 {
            return Err(CliError::Usage("too many bindings".into()));
        }
        let mut fields = BTreeMap::new();
        let mut values = BTreeMap::new();
        for binding in request.bindings {
            let (schema, value) = match binding.value {
                Scalar::Text(v) => (Schema::Text, Value::Text(v)),
                Scalar::Int(v) => (Schema::Int, Value::Int(v)),
                Scalar::Float(v) => (Schema::Float, Value::Float(v)),
                Scalar::Bool(v) => (Schema::Bool, Value::Bool(v)),
                Scalar::Null => (Schema::Null, Value::Null),
            };
            if fields
                .insert(
                    binding.name.clone(),
                    Field {
                        schema,
                        required: true,
                    },
                )
                .is_some()
            {
                return Err(CliError::Usage("duplicate binding name".into()));
            }
            values.insert(binding.name, value);
        }
        let schema = Schema::Object {
            fields,
            allow_unknown: false,
        };
        let context = Value::Object(values);
        crate::schema::validate(&schema, &context, limits).map_err(CliError::Template)?;
        let source = match request.source {
            Source::Inline(source) => source,
            Source::File(path) => self
                .source
                .execute(ReadSource {
                    path,
                    max_bytes: limits.max_source_bytes,
                })
                .map_err(CliError::Source)?,
        };
        let renderer = self.factory.create(RenderConfig::Inline {
            source,
            schema,
            limits,
        })?;
        renderer.render(&context).map_err(CliError::Template)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{assembly::register_renderer, MethodRegistry};
    use forge_bootstrap::Registry;
    use forge_provider::Factory;
    use std::cell::RefCell;
    struct SourceProbe {
        order: Rc<RefCell<Vec<&'static str>>>,
        fail: bool,
    }
    impl IoClient for SourceProbe {
        type Request = ReadSource;
        type Response = String;
        type Error = SourceError;
        fn execute(&self, _: ReadSource) -> Result<String, SourceError> {
            self.order.borrow_mut().push("read");
            if self.fail {
                Err(SourceError::InvalidUtf8)
            } else {
                Ok("{{ name.decorate() }}".into())
            }
        }
    }
    struct FactoryProbe {
        order: Rc<RefCell<Vec<&'static str>>>,
        fail: bool,
    }
    impl RendererFactory for FactoryProbe {
        fn create(&self, config: RenderConfig) -> Result<Renderer, CliError> {
            self.order.borrow_mut().push("create");
            if self.fail {
                return Err(CliError::Template(crate::TemplateError::Configuration(
                    "factory test failure",
                )));
            }
            let mut methods = MethodRegistry::new();
            methods
                .register("decorate", |value| match value {
                    Value::Text(text) => Ok(Value::Text(format!("[{text}]"))),
                    _ => Err(crate::TemplateError::Method {
                        name: "decorate".into(),
                        reason: "text required".into(),
                    }),
                })
                .unwrap();
            let group = register_renderer("test.renderer", config, methods).unwrap();
            let ((), factory) = Registry::new()
                .register(group)
                .unwrap()
                .freeze()
                .unwrap()
                .into_factories();
            factory.create().map_err(CliError::Template)
        }
    }
    fn request(name: &str) -> RenderRequest {
        RenderRequest {
            source: Source::File("fake.forge".into()),
            bindings: vec![super::super::input::Binding {
                name: name.into(),
                value: Scalar::Text("Forge".into()),
            }],
        }
    }
    #[test]
    fn factory_is_injected_called_once_after_read_and_preserves_custom_methods() {
        let order = Rc::new(RefCell::new(Vec::new()));
        let task = RenderTask {
            source: Rc::new(SourceProbe {
                order: order.clone(),
                fail: false,
            }),
            factory: FactoryProbe {
                order: order.clone(),
                fail: false,
            },
        };
        assert_eq!(task.run(request("name")).unwrap(), "[Forge]");
        assert_eq!(*order.borrow(), ["read", "create"]);
    }
    #[test]
    fn invalid_context_and_source_failure_do_not_construct_renderer() {
        for invalid in [true, false] {
            let order = Rc::new(RefCell::new(Vec::new()));
            let task = RenderTask {
                source: Rc::new(SourceProbe {
                    order: order.clone(),
                    fail: !invalid,
                }),
                factory: FactoryProbe {
                    order: order.clone(),
                    fail: false,
                },
            };
            assert!(task
                .run(request(if invalid { "bad.name" } else { "name" }))
                .is_err());
            assert_eq!(*order.borrow(), if invalid { vec![] } else { vec!["read"] });
        }
    }
    #[test]
    fn construction_failure_is_typed_and_not_retried() {
        let order = Rc::new(RefCell::new(Vec::new()));
        let task = RenderTask {
            source: Rc::new(SourceProbe {
                order: order.clone(),
                fail: false,
            }),
            factory: FactoryProbe {
                order: order.clone(),
                fail: true,
            },
        };
        assert!(matches!(
            task.run(request("name")),
            Err(CliError::Template(crate::TemplateError::Configuration(
                "factory test failure"
            )))
        ));
        assert_eq!(*order.borrow(), ["read", "create"]);
    }
}
