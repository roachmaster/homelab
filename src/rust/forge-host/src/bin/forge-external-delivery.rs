//! Run the separately leased external Git/Jenkins delivery service.
use forge_work_publication::durable_files::{effective_uid, read_private};
use forge_work_publication::jenkins_observer::{
    configured_read_observer, ImmutableReader, ProcessObserverExecute, StdMonotonicClock,
    CONFIG_ENV,
};
use forge_work_publication::trusted_delivery::{DeliveryConfig, JenkinsObserverPort};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

fn run() -> Result<(), String> {
    let mut args = std::env::args_os().skip(1);
    let mut config_path = None;
    let mut observer_path = None;
    while let Some(flag) = args.next() {
        let slot = match flag.to_str() {
            Some("--config") => &mut config_path,
            Some("--read-observer-config") => &mut observer_path,
            _ => return Err("Expected --config PATH [--read-observer-config PATH]".into()),
        };
        if slot.is_some() {
            return Err("Repeated configuration argument".into());
        }
        *slot = Some(PathBuf::from(
            args.next().ok_or("Missing configuration path")?,
        ));
    }
    let bytes = read_private(
        &config_path.ok_or("--config is required")?,
        65536,
        Some(effective_uid()),
    )?;
    let doc =
        forge_jenkins::strict_json::strict_parse(&bytes).map_err(|_| "Invalid delivery JSON")?;
    let cfg = DeliveryConfig::parse(&doc).map_err(|e| e.to_string())?;
    let observer_path = observer_path.or_else(|| std::env::var_os(CONFIG_ENV).map(PathBuf::from));
    let home = cfg
        .github_config_dir
        .parent()
        .and_then(|p| p.parent())
        .ok_or("Missing operator home")?
        .display()
        .to_string();
    let observer: Option<Box<dyn JenkinsObserverPort>> = configured_read_observer(
        observer_path,
        ProcessObserverExecute,
        ImmutableReader,
        StdMonotonicClock::new(),
        home,
    )
    .map_err(|_| "Invalid Jenkins observer configuration")?
    .map(|factory| {
        factory
            .create()
            .map(|port| Box::new(port) as Box<dyn JenkinsObserverPort>)
    })
    .transpose()
    .map_err(|_| "Unavailable Jenkins read observer")?;
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    ctrlc::set_handler(move || flag.store(true, Ordering::SeqCst))
        .map_err(|_| "Cannot install stop handler")?;
    let mut worker = forge_postgres::publication_composition::external_delivery_worker(
        cfg,
        observer,
        stop.clone(),
    )?;
    while !stop.load(Ordering::SeqCst) {
        match worker.run_once() {
            Ok(true) => continue,
            Ok(false) => (),
            Err(_) => eprintln!("External delivery action unavailable"),
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    Ok(())
}
fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("External delivery: {e}");
            ExitCode::FAILURE
        }
    }
}
