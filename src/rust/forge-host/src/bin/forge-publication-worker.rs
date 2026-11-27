//! forge-publication-worker: the publication host process
//! (work_publication.py's main): load the publication config,
//! compose the worker through forge-postgres, and run it until
//! SIGINT/SIGTERM.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use forge_postgres::publication_composition::{create_publication_worker, load_publication_config};
use forge_work_publication::jenkins_observer::{
    configured_read_observer, ImmutableReader, ProcessObserverExecute, StdMonotonicClock,
    CONFIG_ENV,
};
use forge_work_publication::trusted_delivery::JenkinsObserverPort;

fn main() -> ExitCode {
    let Some(config_path) = std::env::var_os(
        forge_work_publication::publication_config::CONFIG_ENV,
    ) else {
        eprintln!("Unable to load publication configuration");
        return ExitCode::FAILURE;
    };
    let config = match load_publication_config(&PathBuf::from(config_path)) {
        Ok(config) => config,
        Err(_) => {
            eprintln!("Unable to load publication configuration");
            return ExitCode::FAILURE;
        }
    };

    let observer: Option<Box<dyn JenkinsObserverPort>> = {
        let path = std::env::var_os(CONFIG_ENV).map(PathBuf::from);
        let home = config.worker.account.home.to_string_lossy().into_owned();
        match configured_read_observer(
            path,
            ProcessObserverExecute,
            ImmutableReader,
            StdMonotonicClock::new(),
            home,
        ) {
            Ok(Some(factory)) => match factory.create() {
                Ok(observer) => Some(Box::new(observer)),
                Err(_) => {
                    eprintln!("Unable to load publication configuration");
                    return ExitCode::FAILURE;
                }
            },
            Ok(None) => None,
            Err(_) => {
                eprintln!("Unable to load publication configuration");
                return ExitCode::FAILURE;
            }
        }
    };

    let stop = Arc::new(AtomicBool::new(false));
    let handler_stop = Arc::clone(&stop);
    if ctrlc::set_handler(move || handler_stop.store(true, Ordering::SeqCst)).is_err() {
        eprintln!("Unable to install the stop handler");
        return ExitCode::FAILURE;
    }
    let mut worker = match create_publication_worker(&config, Arc::clone(&stop), observer) {
        Ok(worker) => worker,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::FAILURE;
        }
    };
    worker.run_forever();
    ExitCode::SUCCESS
}
