//! Visible, typed CLI composition. Source loading is admitted work; configured
//! renderer construction is per invocation, after loading, not global lookup.
use super::{
    catalog,
    input::{Command, Invocation, Kind},
    io::{ReadSource, SourceError},
    task::{RenderTask, RendererFactory},
    CliError,
};
use crate::{
    assembly::{register_renderer, RenderConfig, Renderer},
    MethodRegistry,
};
use forge_bootstrap::{PreparedGroup, Registry};
use forge_bridge::{Bridge, RegistryBuilder};
use forge_command::validate_descriptors;
use forge_ingress_contract::{AdmissionDenied, Envelope, Header, RouteKey};
use forge_io_client::IoClient;
use forge_provider::{Factory, InstanceProvider, Singleton};
use forge_task::Task;
use std::{convert::Infallible, rc::Rc};

/// Explicit command-scoped construction, not an opaque callback or mutable
/// container. Each invocation stages/freezes the public capability factory.
pub(super) struct RegisteredRendererFactory;
impl RendererFactory for RegisteredRendererFactory {
    fn create(&self, config: RenderConfig) -> Result<Renderer, CliError> {
        let group = register_renderer("template.cli.renderer", config, MethodRegistry::new())
            .map_err(CliError::Assembly)?;
        let pending = Registry::new()
            .register(group)
            .map_err(|e| CliError::Registration(e.reason().clone()))?;
        let frozen = pending
            .freeze()
            .map_err(|e| CliError::Validation(e.reason().clone()))?;
        let ((), factory) = frozen.into_factories();
        // Exactly one construction; the task owns this invocation-local value.
        factory.create().map_err(CliError::Template)
    }
}
struct WorkflowFactory<C> {
    source: Rc<C>,
}
impl<C> Factory<RenderTask<C, RegisteredRendererFactory>> for WorkflowFactory<C> {
    type Error = Infallible;
    fn create(&self) -> Result<RenderTask<C, RegisteredRendererFactory>, Infallible> {
        Ok(RenderTask {
            source: Rc::clone(&self.source),
            factory: RegisteredRendererFactory,
        })
    }
}
fn admit(_: &Header, allowed: &bool) -> Result<(), AdmissionDenied> {
    if *allowed {
        Ok(())
    } else {
        Err(AdmissionDenied)
    }
}
type Policy = fn(&Header, &bool) -> Result<(), AdmissionDenied>;
pub(super) struct Application {
    pub bridge: Bridge<'static, Command, bool, String, CliError, Policy>,
}
impl Application {
    pub fn bootstrap<C>(source: C) -> Result<Self, CliError>
    where
        C: IoClient<Request = ReadSource, Response = String, Error = SourceError> + 'static,
    {
        validate_descriptors(catalog::descriptors()).map_err(CliError::Descriptor)?;
        let group = PreparedGroup::new(
            "template.cli",
            &["template.cli.task"],
            &[],
            WorkflowFactory {
                source: Rc::new(source),
            },
        )
        .map_err(CliError::Registration)?;
        let pending = Registry::new()
            .register(group)
            .map_err(|e| CliError::Registration(e.reason().clone()))?;
        let frozen = pending
            .freeze()
            .map_err(|e| CliError::Validation(e.reason().clone()))?;
        let ((), factory) = frozen.into_factories();
        let task = match factory.create() {
            Ok(task) => task,
            Err(never) => match never {},
        };
        let provider = Singleton::new(task);
        let mut routes = RegistryBuilder::new();
        let descriptor = catalog::descriptors()
            .iter()
            .find(|d| d.name == catalog::RENDER_ROUTE)
            .ok_or(CliError::Invariant("missing render descriptor"))?;
        let route = RouteKey::new(descriptor.name, 1).map_err(CliError::Header)?;
        routes
            .register(route, Kind::Render, move |command, _: &bool| {
                let Command::Render(request) = command else {
                    return Err(CliError::Invariant("render binding mismatch"));
                };
                let task = match provider.acquire() {
                    Ok(task) => task,
                    Err(never) => match never {},
                };
                task.run(request)
            })
            .map_err(CliError::Duplicate)?;
        Self::finish(routes)
    }
    fn finish(
        mut routes: RegistryBuilder<'static, Command, bool, String, CliError>,
    ) -> Result<Self, CliError> {
        routes
            .register(
                RouteKey::new(catalog::METHODS_ROUTE, 1).map_err(CliError::Header)?,
                Kind::Methods,
                |command, _: &bool| match command {
                    Command::Methods => Ok(MethodRegistry::new()
                        .names()
                        .map(|name| format!("{name}\n"))
                        .collect()),
                    _ => Err(CliError::Invariant("methods binding mismatch")),
                },
            )
            .map_err(CliError::Duplicate)?;
        routes
            .register(
                RouteKey::new(catalog::HELP_ROUTE, 1).map_err(CliError::Header)?,
                Kind::Help,
                |command, _: &bool| match command {
                    Command::Help(target) => Ok(catalog::help(target)),
                    _ => Err(CliError::Invariant("help binding mismatch")),
                },
            )
            .map_err(CliError::Duplicate)?;
        Ok(Self {
            bridge: Bridge::new(routes.freeze(), admit as Policy),
        })
    }
    pub fn invoke(&self, invocation: Invocation) -> Result<String, CliError> {
        let header = Header::new(
            RouteKey::new(invocation.route, 1).map_err(CliError::Header)?,
            "template-cli",
        )
        .map_err(CliError::Header)?;
        // Trusted local execution only. This constant is not authentication or
        // authorization for an untrusted remote caller.
        self.bridge
            .dispatch(Envelope::new(header, invocation.command), &true)
            .map(|response| response.payload)
            .map_err(|error| match error {
                forge_bridge::DispatchError::Handler(error) => error,
                _ => CliError::Invariant("CLI route/admission invariant failed"),
            })
    }
}
