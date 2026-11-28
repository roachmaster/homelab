//! forge-runner composition: typed settings, one registrar per
//! service, and a single registry the process freezes before it
//! creates anything.
//!
//! - **Settings** are loaded once at the composition boundary from
//!   an environment map the binary collects. Library code below
//!   never reads process state.
//! - **Registrars** stage effect-free factory recipes. An
//!   unconfigured service is still bound: its factory yields the
//!   null-object port, so the HTTP surface needs no `Option`.
//! - **Freeze** validates the plan: the HTTP surface requires the
//!   work, usage and Jenkins bindings, so leaving one out fails
//!   before any database, thread or socket is touched.
//! - **Assembly** creates the services in the old order (work,
//!   usage, Jenkins), hands each port to a `Singleton` provider,
//!   and only binds the listener after the background loops start.
//!
//! Tests and the client example use the same `assemble` with fake
//! factories in place of the production ones.

use std::collections::HashMap;
use std::net::TcpListener;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use forge_core::bootstrap::{PreparedGroup, RegistrationError, Registry};
use forge_core::provider::{Factory, InstanceProvider, Singleton};
use forge_jenkins::config::JenkinsConfig;
use forge_postgres::runner_composition::{
    create_usage_service, create_work_service, runner_pg_config, RunnerPgEnv,
};
use forge_work::work_config::WorkApiConfig;

use crate::runner_http::{configured_listener, load_token, load_work_config, serve, Services};
use crate::runner_ports::{
    Background, ConsumerLoop, Idle, JenkinsPort, LiveJenkins, LiveUsage, LiveWork,
    UnavailableJenkins, UnavailableUsage, UnavailableWork, UsagePort, WorkPort, WorkflowLoop,
};

/// Binding provided by the work registrar.
pub const WORK: &str = "runner.work";
/// Binding provided by the usage registrar.
pub const USAGE: &str = "runner.usage";
/// Binding provided by the Jenkins registrar.
pub const JENKINS: &str = "runner.jenkins";
/// Binding provided by the HTTP surface registrar.
pub const HTTP: &str = "runner.http";

/// A created service: the port the HTTP surface calls and the
/// background loop the process owns for it.
pub struct ServiceParts<P: ?Sized> {
    pub port: Arc<P>,
    pub background: Box<dyn Background>,
}

/// The work service as created.
pub type WorkParts = ServiceParts<dyn WorkPort>;
/// The Jenkins service as created.
pub type JenkinsParts = ServiceParts<dyn JenkinsPort>;

// ---------------------------------------------------------------
// Settings
// ---------------------------------------------------------------

/// Everything the runner selects at startup, validated before
/// registration.
pub struct RunnerSettings {
    pub port: u16,
    pub api_only: bool,
    pub token: String,
    pub work: Option<WorkApiConfig>,
    pub usage: Option<WorkApiConfig>,
    pub jenkins: Option<JenkinsConfig>,
    pub pg_env: RunnerPgEnv,
}

impl RunnerSettings {
    /// Load the runner's settings from a collected environment map.
    /// Error text is the runner's existing startup failure text.
    pub fn load(environ: &HashMap<String, String>) -> Result<Self, String> {
        let (port, api_only) = configured_listener(environ)?;
        let token = load_token(environ)?;
        let work = match environ.get(forge_work::work_config::CONFIG_ENV_VAR) {
            Some(path) => Some(load_work_config(path)?),
            None => None,
        };
        // Python loads the usage config separately; without one it
        // falls back to the work config's shape, and with neither
        // there is no usage service.
        let usage = match environ.get(forge_work::work_config::USAGE_CONFIG_ENV_VAR) {
            Some(path) => Some(load_work_config(path)?),
            None => work.clone(),
        };
        let jenkins = forge_jenkins::config::load_jenkins_config(
            environ
                .get(forge_jenkins::config::CONFIG_ENV_VAR)
                .map(String::as_str),
        )
        .map_err(|error| format!("jenkins config failed: {error}"))?;
        Ok(RunnerSettings {
            port,
            api_only,
            token,
            work,
            usage,
            jenkins,
            pg_env: RunnerPgEnv::from_environ(environ),
        })
    }
}

// ---------------------------------------------------------------
// Production factories
// ---------------------------------------------------------------

/// Creates the work service, or the unavailable default when no
/// work configuration was selected.
pub enum WorkFactory {
    Configured {
        config: Box<WorkApiConfig>,
        pg_env: RunnerPgEnv,
        api_only: bool,
    },
    Unconfigured,
}

impl Factory<WorkParts> for WorkFactory {
    type Error = String;
    fn create(&self) -> Result<WorkParts, String> {
        let WorkFactory::Configured {
            config,
            pg_env,
            api_only,
        } = self
        else {
            return Ok(ServiceParts {
                port: Arc::new(UnavailableWork),
                background: Box::new(Idle),
            });
        };
        let pg = runner_pg_config(&config.owner, pg_env)?;
        let runner_work = create_work_service(config, &pg)
            .map_err(|error| format!("work service failed: {error}"))?;
        let background: Box<dyn Background> = if *api_only {
            Box::new(Idle)
        } else {
            Box::new(WorkflowLoop {
                start: runner_work.workflow_thread,
                service: Arc::clone(&runner_work.service),
            })
        };
        Ok(ServiceParts {
            port: Arc::new(LiveWork(runner_work.service)),
            background,
        })
    }
}

/// Creates the usage service, or the unavailable default when no
/// usage configuration applies or the store declines to serve.
pub enum UsageFactory {
    Configured {
        config: Box<WorkApiConfig>,
        pg_env: RunnerPgEnv,
    },
    Unconfigured,
}

impl Factory<Arc<dyn UsagePort>> for UsageFactory {
    type Error = String;
    fn create(&self) -> Result<Arc<dyn UsagePort>, String> {
        let UsageFactory::Configured { config, pg_env } = self else {
            return Ok(Arc::new(UnavailableUsage));
        };
        let pg = runner_pg_config(&config.owner, pg_env)?;
        match create_usage_service(config, &pg)
            .map_err(|error| format!("usage service failed: {error}"))?
        {
            Some(service) => Ok(Arc::new(LiveUsage(Arc::new(service)))),
            None => Ok(Arc::new(UnavailableUsage)),
        }
    }
}

/// Creates the Jenkins service and its event consumer, or the
/// unavailable default when Jenkins integration is not configured.
pub enum JenkinsFactory {
    Configured {
        config: Box<JenkinsConfig>,
        pg_env: RunnerPgEnv,
        api_only: bool,
    },
    Unconfigured,
}

impl Factory<JenkinsParts> for JenkinsFactory {
    type Error = String;
    fn create(&self) -> Result<JenkinsParts, String> {
        let unavailable = || ServiceParts {
            port: Arc::new(UnavailableJenkins) as Arc<dyn JenkinsPort>,
            background: Box::new(Idle) as Box<dyn Background>,
        };
        let JenkinsFactory::Configured {
            config,
            pg_env,
            api_only,
        } = self
        else {
            return Ok(unavailable());
        };
        let store_config = (**config).clone();
        let store_env = pg_env.clone();
        let store_factory: forge_jenkins::consumer::StoreFactory = Arc::new(move || {
            let pg = runner_pg_config(&store_config.owner, &store_env)
                .expect("runner database configuration");
            Box::new(forge_postgres::jenkins_store_adapter::PgJenkinsStore::new(
                store_config.clone(),
                pg,
            )) as Box<dyn forge_jenkins::store::JenkinsStorePort>
        });
        match forge_jenkins::api::create_jenkins_service(
            Some((**config).clone()),
            store_factory,
            None,
            "jenkins-consumer",
            !api_only,
        ) {
            Ok(Some(mut service)) => {
                let consumer = service.consumer_worker.take();
                let background: Box<dyn Background> = match consumer {
                    Some(consumer) if !api_only => Box::new(ConsumerLoop(consumer)),
                    _ => Box::new(Idle),
                };
                Ok(ServiceParts {
                    port: Arc::new(LiveJenkins(Arc::new(Mutex::new(service)))),
                    background,
                })
            }
            Ok(None) => Ok(unavailable()),
            Err(error) => Err(format!("jenkins service failed: {error}")),
        }
    }
}

/// Binds the loopback listener the HTTP surface serves on.
pub struct ListenerFactory {
    pub port: u16,
}

impl Factory<TcpListener> for ListenerFactory {
    type Error = String;
    fn create(&self) -> Result<TcpListener, String> {
        let port = self.port;
        TcpListener::bind(("127.0.0.1", port))
            .map_err(|error| format!("unable to bind 127.0.0.1:{port}: {error}"))
    }
}

// ---------------------------------------------------------------
// Registrars
// ---------------------------------------------------------------

/// Stage the work service.
pub fn register_work<F>(factory: F) -> Result<PreparedGroup<F>, RegistrationError> {
    PreparedGroup::new(WORK, &[WORK], &[], factory)
}

/// Stage the usage service.
pub fn register_usage<F>(factory: F) -> Result<PreparedGroup<F>, RegistrationError> {
    PreparedGroup::new(USAGE, &[USAGE], &[], factory)
}

/// Stage the Jenkins service.
pub fn register_jenkins<F>(factory: F) -> Result<PreparedGroup<F>, RegistrationError> {
    PreparedGroup::new(JENKINS, &[JENKINS], &[], factory)
}

/// Stage the HTTP surface. It requires every service binding, so a
/// plan without one of them is rejected at freeze.
pub fn register_http<F>(factory: F) -> Result<PreparedGroup<F>, RegistrationError> {
    PreparedGroup::new(HTTP, &[HTTP], &[WORK, USAGE, JENKINS], factory)
}

/// The production selection: each service's factory chosen from
/// the settings, plus the loopback listener.
pub struct ProductionGroups {
    pub work: PreparedGroup<WorkFactory>,
    pub usage: PreparedGroup<UsageFactory>,
    pub jenkins: PreparedGroup<JenkinsFactory>,
    pub http: PreparedGroup<ListenerFactory>,
    pub token: String,
}

impl ProductionGroups {
    pub fn select(settings: RunnerSettings) -> Result<Self, String> {
        let RunnerSettings {
            port,
            api_only,
            token,
            work,
            usage,
            jenkins,
            pg_env,
        } = settings;
        let work = match work {
            Some(config) => WorkFactory::Configured {
                config: Box::new(config),
                pg_env: pg_env.clone(),
                api_only,
            },
            None => WorkFactory::Unconfigured,
        };
        let usage = match usage {
            Some(config) => UsageFactory::Configured {
                config: Box::new(config),
                pg_env: pg_env.clone(),
            },
            None => UsageFactory::Unconfigured,
        };
        let jenkins = match jenkins {
            Some(config) => JenkinsFactory::Configured {
                config: Box::new(config),
                pg_env,
                api_only,
            },
            None => JenkinsFactory::Unconfigured,
        };
        Ok(ProductionGroups {
            work: register_work(work).map_err(wiring)?,
            usage: register_usage(usage).map_err(wiring)?,
            jenkins: register_jenkins(jenkins).map_err(wiring)?,
            http: register_http(ListenerFactory { port }).map_err(wiring)?,
            token,
        })
    }

    pub fn assemble(self) -> Result<Runner<ListenerFactory>, String> {
        assemble(self.work, self.usage, self.jenkins, self.http, self.token)
    }
}

fn wiring(error: impl std::fmt::Display) -> String {
    format!("runner wiring rejected: {error}")
}

// ---------------------------------------------------------------
// Assembly
// ---------------------------------------------------------------

/// The assembled runner: services created and held by their
/// providers, listener not yet bound, background loops not yet
/// started.
pub struct Runner<L> {
    work: Singleton<Arc<dyn WorkPort>>,
    usage: Singleton<Arc<dyn UsagePort>>,
    jenkins: Singleton<Arc<dyn JenkinsPort>>,
    backgrounds: Vec<Box<dyn Background>>,
    listener: L,
    token: String,
}

/// Register, freeze, then create the services in order. Nothing
/// is created unless the whole plan validates.
pub fn assemble<W, U, J, L>(
    work: PreparedGroup<W>,
    usage: PreparedGroup<U>,
    jenkins: PreparedGroup<J>,
    http: PreparedGroup<L>,
    token: String,
) -> Result<Runner<L>, String>
where
    W: Factory<WorkParts, Error = String>,
    U: Factory<Arc<dyn UsagePort>, Error = String>,
    J: Factory<JenkinsParts, Error = String>,
    L: Factory<TcpListener, Error = String>,
{
    let frozen = Registry::new()
        .register(work)
        .map_err(|rejected| wiring(rejected.reason()))?
        .register(usage)
        .map_err(|rejected| wiring(rejected.reason()))?
        .register(jenkins)
        .map_err(|rejected| wiring(rejected.reason()))?
        .register(http)
        .map_err(|rejected| wiring(rejected.reason()))?
        .freeze()
        .map_err(|rejected| wiring(rejected.reason()))?;
    let (((((), work), usage), jenkins), listener) = frozen.into_factories();
    let work = work.create()?;
    let usage = usage.create()?;
    let jenkins = jenkins.create()?;
    Ok(Runner {
        work: Singleton::new(work.port),
        usage: Singleton::new(usage),
        jenkins: Singleton::new(jenkins.port),
        backgrounds: vec![work.background, jenkins.background],
        listener,
        token,
    })
}

impl<L: Factory<TcpListener, Error = String>> Runner<L> {
    /// The ports the HTTP surface dispatches to.
    pub fn services(&self) -> Services {
        let Ok(work) = self.work.acquire();
        let Ok(usage) = self.usage.acquire();
        let Ok(jenkins) = self.jenkins.acquire();
        Services {
            work: Arc::clone(work),
            jenkins: Arc::clone(jenkins),
            usage: Arc::clone(usage),
        }
    }

    /// Start the background loops, bind the listener, serve until
    /// `stop` is raised, then finish the loops in start order.
    pub fn run(self, stop: Arc<AtomicBool>) -> Result<(), String> {
        let services = Arc::new(self.services());
        let running: Vec<_> = self
            .backgrounds
            .into_iter()
            .map(|background| background.start(&stop))
            .collect();
        let ready = Arc::new(AtomicBool::new(true));
        let listener = self.listener.create()?;
        serve(listener, services, Arc::new(self.token), ready, stop);
        for loop_ in running {
            loop_.finish();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner_ports::Running;
    use forge_core::bootstrap::ValidationError;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Shared record of what the test env's fakes saw.
    #[derive(Default)]
    struct Journal {
        created: AtomicUsize,
        events: Mutex<Vec<String>>,
    }

    impl Journal {
        fn note(&self, event: &str) {
            self.events.lock().unwrap().push(event.to_string());
        }
        fn events(&self) -> Vec<String> {
            self.events.lock().unwrap().clone()
        }
    }

    struct FakeLoop {
        name: &'static str,
        journal: Arc<Journal>,
    }

    impl Background for FakeLoop {
        fn start(self: Box<Self>, _stop: &Arc<AtomicBool>) -> Box<dyn Running> {
            self.journal.note(&format!("start {}", self.name));
            self
        }
    }

    impl Running for FakeLoop {
        fn finish(self: Box<Self>) {
            self.journal.note(&format!("finish {}", self.name));
        }
    }

    struct FakeWorkFactory(Arc<Journal>);
    impl Factory<WorkParts> for FakeWorkFactory {
        type Error = String;
        fn create(&self) -> Result<WorkParts, String> {
            self.0.created.fetch_add(1, Ordering::SeqCst);
            self.0.note("create work");
            Ok(ServiceParts {
                port: Arc::new(UnavailableWork),
                background: Box::new(FakeLoop {
                    name: "workflow",
                    journal: Arc::clone(&self.0),
                }),
            })
        }
    }

    struct FakeUsageFactory(Arc<Journal>);
    impl Factory<Arc<dyn UsagePort>> for FakeUsageFactory {
        type Error = String;
        fn create(&self) -> Result<Arc<dyn UsagePort>, String> {
            self.0.created.fetch_add(1, Ordering::SeqCst);
            self.0.note("create usage");
            Ok(Arc::new(UnavailableUsage))
        }
    }

    struct FakeJenkinsFactory(Arc<Journal>);
    impl Factory<JenkinsParts> for FakeJenkinsFactory {
        type Error = String;
        fn create(&self) -> Result<JenkinsParts, String> {
            self.0.created.fetch_add(1, Ordering::SeqCst);
            self.0.note("create jenkins");
            Ok(ServiceParts {
                port: Arc::new(UnavailableJenkins),
                background: Box::new(FakeLoop {
                    name: "consumer",
                    journal: Arc::clone(&self.0),
                }),
            })
        }
    }

    /// Binds an ephemeral loopback port instead of the configured one.
    struct EphemeralListener(Arc<Journal>);
    impl Factory<TcpListener> for EphemeralListener {
        type Error = String;
        fn create(&self) -> Result<TcpListener, String> {
            self.0.note("bind");
            TcpListener::bind(("127.0.0.1", 0)).map_err(|error| error.to_string())
        }
    }

    fn token() -> String {
        "a".repeat(32)
    }

    fn test_env(journal: &Arc<Journal>) -> Result<Runner<EphemeralListener>, String> {
        assemble(
            register_work(FakeWorkFactory(Arc::clone(journal))).unwrap(),
            register_usage(FakeUsageFactory(Arc::clone(journal))).unwrap(),
            register_jenkins(FakeJenkinsFactory(Arc::clone(journal))).unwrap(),
            register_http(EphemeralListener(Arc::clone(journal))).unwrap(),
            token(),
        )
    }

    #[test]
    fn a_plan_without_the_jenkins_service_is_rejected_at_freeze() {
        let journal = Arc::new(Journal::default());
        let rejected = Registry::new()
            .register(register_work(FakeWorkFactory(Arc::clone(&journal))).unwrap())
            .unwrap()
            .register(register_usage(FakeUsageFactory(Arc::clone(&journal))).unwrap())
            .unwrap()
            .register(register_http(EphemeralListener(Arc::clone(&journal))).unwrap())
            .unwrap()
            .freeze();
        let Err(rejected) = rejected else {
            panic!("the HTTP surface requires the Jenkins binding");
        };
        assert_eq!(
            rejected.reason(),
            &ValidationError::MissingRequirement {
                group: HTTP.to_string(),
                binding: JENKINS.to_string(),
            }
        );
        assert_eq!(journal.created.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_misnamed_service_stops_assembly_before_anything_is_created() {
        let journal = Arc::new(Journal::default());
        let stray = PreparedGroup::new(
            "runner.jenkins-old",
            &["runner.jenkins-old"],
            &[],
            FakeJenkinsFactory(Arc::clone(&journal)),
        )
        .unwrap();
        let assembled = assemble(
            register_work(FakeWorkFactory(Arc::clone(&journal))).unwrap(),
            register_usage(FakeUsageFactory(Arc::clone(&journal))).unwrap(),
            stray,
            register_http(EphemeralListener(Arc::clone(&journal))).unwrap(),
            token(),
        );
        let Err(error) = assembled else {
            panic!("assembly must be rejected");
        };
        assert!(error.starts_with("runner wiring rejected:"), "{error}");
        assert_eq!(journal.created.load(Ordering::SeqCst), 0);
        assert!(journal.events().is_empty(), "no bind, no loop, no service");
    }

    #[test]
    fn the_test_env_binds_its_fakes_through_the_same_assembly() {
        let journal = Arc::new(Journal::default());
        let runner = test_env(&journal).expect("fakes assemble");
        assert_eq!(
            journal.events(),
            ["create work", "create usage", "create jenkins"],
            "services are created in the runner's historical order"
        );
        let first = runner.services();
        let second = runner.services();
        assert!(
            Arc::ptr_eq(&first.work, &second.work),
            "one work port per runner"
        );
        assert!(
            Arc::ptr_eq(&first.usage, &second.usage),
            "one usage port per runner"
        );
        assert!(
            Arc::ptr_eq(&first.jenkins, &second.jenkins),
            "one Jenkins port per runner"
        );
    }

    #[test]
    fn loops_start_before_the_listener_binds_and_finish_after_serving() {
        let journal = Arc::new(Journal::default());
        let runner = test_env(&journal).expect("fakes assemble");
        let stop = Arc::new(AtomicBool::new(true));
        runner.run(stop).expect("runner serves until stopped");
        assert_eq!(
            journal.events(),
            [
                "create work",
                "create usage",
                "create jenkins",
                "start workflow",
                "start consumer",
                "bind",
                "finish workflow",
                "finish consumer",
            ]
        );
    }

    #[test]
    fn an_unconfigured_runner_assembles_without_touching_a_database() {
        let settings = RunnerSettings {
            port: 18083,
            api_only: false,
            token: token(),
            work: None,
            usage: None,
            jenkins: None,
            pg_env: RunnerPgEnv::default(),
        };
        let runner = ProductionGroups::select(settings)
            .and_then(ProductionGroups::assemble)
            .expect("null-object services need no database");
        assert_eq!(runner.backgrounds.len(), 2);
    }

    #[test]
    fn settings_load_the_listener_and_token_and_select_no_services_by_default() {
        let directory = std::env::temp_dir().join(format!(
            "forge-runner-settings-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("runner.token"), format!("{}\n", token())).unwrap();
        let environ: HashMap<String, String> = [
            (
                "CREDENTIALS_DIRECTORY".to_string(),
                directory.display().to_string(),
            ),
            ("FORGE_RUNNER_PORT".to_string(), "18099".to_string()),
            ("PGUSER".to_string(), "forge_runner".to_string()),
        ]
        .into_iter()
        .collect();
        let settings = RunnerSettings::load(&environ).expect("settings load");
        std::fs::remove_dir_all(&directory).unwrap();
        assert_eq!(settings.port, 18099);
        assert!(!settings.api_only);
        assert_eq!(settings.token, token());
        assert!(settings.work.is_none() && settings.usage.is_none() && settings.jenkins.is_none());
        assert_eq!(settings.pg_env.pguser.as_deref(), Some("forge_runner"));
    }

    #[test]
    fn settings_report_a_missing_credentials_directory_as_before() {
        let error = RunnerSettings::load(&HashMap::new())
            .err()
            .expect("no credentials");
        assert_eq!(error, "CREDENTIALS_DIRECTORY is not set");
    }
}
