//! forge-publication-worker: the publication host process
//! (work_publication.py's main) and its composition boundary.
//!
//! It reads the environment once, loads the publication config,
//! stages the process exec client, the publication runtime and the
//! publication worker groups, and freezes the registry: a plan missing
//! a required group is rejected before any session, thread or socket
//! exists. It then creates the observer from the frozen observer exec
//! factory, binds it into the worker recipe, installs the stop
//! handler, creates the worker and runs it until SIGINT/SIGTERM.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use forge_core::bootstrap::Registry;
use forge_core::provider::Factory;
use forge_postgres::publication_composition::{
    load_publication_config, register_publication_worker, PublicationWorkerRecipe,
};
use forge_work_publication::jenkins_observer::{
    configured_read_observer, ImmutableReader, StdMonotonicClock, CONFIG_ENV,
};
use forge_work_publication::publication_config::PublicationConfig;
use forge_work_publication::registrar::register_publication;
use forge_work_publication::trusted_delivery::JenkinsObserverPort;
use forge_work_worker::process_exec::register_process_exec;

type Factories = (
    (
        ((), forge_work_worker::process_exec::ProcessExecFactory),
        forge_work_publication::registrar::PublicationFactories,
    ),
    PublicationWorkerRecipe,
);

/// The production plan: process exec client, publication runtime,
/// publication worker. Registration is effect-free; freeze rejects a
/// plan missing a required binding.
fn freeze_plan(config: PublicationConfig) -> Result<Factories, String> {
    let frozen = Registry::new()
        .register(register_process_exec().map_err(|error| error.to_string())?)
        .map_err(|rejected| rejected.reason().to_string())?
        .register(register_publication().map_err(|error| error.to_string())?)
        .map_err(|rejected| rejected.reason().to_string())?
        .register(register_publication_worker(config))
        .map_err(|rejected| rejected.reason().to_string())?
        .freeze()
        .map_err(|rejected| rejected.reason().to_string())?;
    Ok(frozen.into_factories())
}

const CONFIG_UNAVAILABLE: &str = "Unable to load publication configuration";

fn main() -> ExitCode {
    let Some(config_path) =
        std::env::var_os(forge_work_publication::publication_config::CONFIG_ENV)
    else {
        eprintln!("{CONFIG_UNAVAILABLE}");
        return ExitCode::FAILURE;
    };
    let observer_path = std::env::var_os(CONFIG_ENV).map(PathBuf::from);
    let config = match load_publication_config(&PathBuf::from(config_path)) {
        Ok(config) => config,
        Err(_) => {
            eprintln!("{CONFIG_UNAVAILABLE}");
            return ExitCode::FAILURE;
        }
    };
    let home = config.worker.account.home.to_string_lossy().into_owned();

    let ((((), _process_exec), runtime), worker) = match freeze_plan(config) {
        Ok(factories) => factories,
        Err(reason) => {
            eprintln!("{reason}");
            return ExitCode::FAILURE;
        }
    };

    let observer_exec = match runtime.observer_exec.create() {
        Ok(observer_exec) => observer_exec,
        Err(_) => {
            eprintln!("{CONFIG_UNAVAILABLE}");
            return ExitCode::FAILURE;
        }
    };
    let observer: Option<Box<dyn JenkinsObserverPort>> = match configured_read_observer(
        observer_path,
        observer_exec,
        ImmutableReader,
        StdMonotonicClock::new(),
        home,
    ) {
        Ok(Some(factory)) => match factory.create() {
            Ok(observer) => Some(Box::new(observer)),
            Err(_) => {
                eprintln!("{CONFIG_UNAVAILABLE}");
                return ExitCode::FAILURE;
            }
        },
        Ok(None) => None,
        Err(_) => {
            eprintln!("{CONFIG_UNAVAILABLE}");
            return ExitCode::FAILURE;
        }
    };

    let stop = Arc::new(AtomicBool::new(false));
    let worker = worker.bind(observer, Arc::clone(&stop));
    let handler_stop = Arc::clone(&stop);
    if ctrlc::set_handler(move || handler_stop.store(true, Ordering::SeqCst)).is_err() {
        eprintln!("Unable to install the stop handler");
        return ExitCode::FAILURE;
    }
    let mut worker = match worker.create() {
        Ok(worker) => worker,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::FAILURE;
        }
    };
    worker.run_forever();
    ExitCode::SUCCESS
}
