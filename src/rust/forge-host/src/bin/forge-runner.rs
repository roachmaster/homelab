//! Authenticated Mission Control runner for work, Jenkins, usage, workspace and
//! asynchronous model runs. Dispatch mode reconciles distributed GPU results.

use std::collections::HashMap;
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use forge_host::runner_http::{configured_listener, load_token, load_work_config, serve, Services};
use forge_postgres::runner_composition::{
    create_usage_service, create_work_service, runner_pg_config,
};

fn fail(message: &str) -> ! {
    eprintln!("forge-runner: {message}");
    std::process::exit(1);
}

fn main() {
    let environ: HashMap<String, String> = std::env::vars().collect();
    let (port, api_only) = match configured_listener(&environ) {
        Ok(pair) => pair,
        Err(error) => fail(&error),
    };
    let token = match load_token(&environ) {
        Ok(token) => token,
        Err(error) => fail(&error),
    };

    // Work service (its own config load) + workflow thread start.
    let mut work_service = None;
    let mut workflow_thread = None;
    let mut work_config = None;
    if let Some(path) = environ.get(forge_work::work_config::CONFIG_ENV_VAR) {
        let config = match load_work_config(path) {
            Ok(config) => config,
            Err(error) => fail(&error),
        };
        let pg = match runner_pg_config(&config.owner) {
            Ok(pg) => pg,
            Err(error) => fail(&error),
        };
        match create_work_service(&config, &pg) {
            Ok(runner_work) => {
                work_service = Some(runner_work.service);
                workflow_thread = Some(runner_work.workflow_thread);
                work_config = Some(config);
            }
            Err(error) => fail(&format!("work service failed: {error}")),
        }
    }

    // Usage service: Python loads the usage config separately;
    // without one it falls back to the work config's shape, and
    // with neither there is no usage service.
    let usage_config =
        if let Some(path) = environ.get(forge_work::work_config::USAGE_CONFIG_ENV_VAR) {
            match load_work_config(path) {
                Ok(config) => Some(config),
                Err(error) => fail(&error),
            }
        } else {
            work_config.clone()
        };
    let usage_service = match &usage_config {
        Some(config) => {
            let pg = match runner_pg_config(&config.owner) {
                Ok(pg) => pg,
                Err(error) => fail(&error),
            };
            match create_usage_service(config, &pg) {
                Ok(service) => service.map(Arc::new),
                Err(error) => fail(&format!("usage service failed: {error}")),
            }
        }
        None => None,
    };

    // Jenkins service + its event consumer.
    let jenkins_config = match forge_jenkins::config::load_jenkins_config(
        environ
            .get(forge_jenkins::config::CONFIG_ENV_VAR)
            .map(String::as_str),
    ) {
        Ok(config) => config,
        Err(error) => fail(&format!("jenkins config failed: {error}")),
    };
    let mut jenkins_consumer = None;
    let jenkins_service = match jenkins_config {
        Some(config) => {
            let store_config = config.clone();
            let store_factory: forge_jenkins::consumer::StoreFactory = Arc::new(move || {
                let pg =
                    runner_pg_config(&store_config.owner).expect("runner database configuration");
                Box::new(forge_postgres::jenkins_store_adapter::PgJenkinsStore::new(
                    store_config.clone(),
                    pg,
                )) as Box<dyn forge_jenkins::store::JenkinsStorePort>
            });
            match forge_jenkins::api::create_jenkins_service(
                Some(config),
                store_factory,
                None,
                "jenkins-consumer",
                !api_only,
            ) {
                Ok(Some(mut service)) => {
                    jenkins_consumer = service.consumer_worker.take();
                    Some(Arc::new(Mutex::new(service)))
                }
                Ok(None) => None,
                Err(error) => fail(&format!("jenkins service failed: {error}")),
            }
        }
        None => None,
    };

    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = Arc::clone(&stop);
        if ctrlc::set_handler(move || stop.store(true, Ordering::SeqCst)).is_err() {
            fail("failed to install the stop handler");
        }
    }

    // Acquire the listener before any background claimant can start.
    let listener = match TcpListener::bind(("127.0.0.1", port)) {
        Ok(listener) => listener,
        Err(error) => fail(&format!("unable to bind 127.0.0.1:{port}: {error}")),
    };

    // Background services (runner.py's background_services):
    // start the workflow coordinator thread and the Jenkins
    // consumer, then report ready.
    let workspace = runner_pg_config("mission-control").unwrap_or_else(|e| fail(&e));
    let inference_model =
        (environ.get("FORGE_RUNNER_MODE").map(String::as_str) == Some("dispatch")).then(|| {
            let default = environ
                .get("FORGE_RUNNER_MODEL")
                .map(String::as_str)
                .unwrap_or(forge_postgres::inference_runs::DEFAULT_MODEL);
            if !matches!(
                default,
                forge_postgres::inference_runs::DEFAULT_MODEL
                    | forge_postgres::inference_runs::SECOND_MODEL
            ) {
                fail("Invalid configured runner model")
            }
            default.to_owned()
        });
    let mut background: Vec<std::thread::JoinHandle<()>> = Vec::new();
    let mut consumer_thread = None;
    if !api_only {
        if inference_model.is_some() {
            let pg = workspace.clone();
            let stop = stop.clone();
            background.push(std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    if forge_postgres::inference_runs::reconcile(&pg).is_err() {
                        eprintln!("GPU completion scan unavailable");
                    }
                    for _ in 0..20 {
                        if stop.load(Ordering::SeqCst) {
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(100));
                    }
                }
            }));
        }
        if let (Some(start), Some(service)) = (workflow_thread, &work_service) {
            background.push(start.spawn(Arc::clone(service), Arc::clone(&stop)));
        }
        if let Some(mut consumer) = jenkins_consumer {
            let stop = Arc::clone(&consumer.stop);
            consumer_thread = Some((
                stop,
                std::thread::spawn(move || {
                    consumer.run();
                    consumer
                }),
            ));
        }
    }
    let ready = Arc::new(AtomicBool::new(true));

    let services = Arc::new(Services {
        workspace,
        inference_model,
        work: work_service,
        jenkins: jenkins_service,
        usage: usage_service,
    });
    serve(
        listener,
        services,
        Arc::new(token),
        ready,
        Arc::clone(&stop),
    );
    stop.store(true, Ordering::SeqCst);
    for handle in background {
        let _ = handle.join();
    }
    if let Some((consumer_stop, handle)) = consumer_thread {
        consumer_stop.store(true, Ordering::SeqCst);
        if let Ok(mut consumer) = handle.join() {
            let _ = consumer.close(6.0);
        }
    }
}
