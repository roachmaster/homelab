//! Run the separately deployed Forge-local inference worker.
use forge_host::gpu_proxy::GpuProxyStore;
use forge_postgres::gpu_adapter::DirectGpuStore;
use forge_work_publication::durable_files::read_private;
use forge_work_worker::gpu::{ForgeGpuWorker, GpuStorePort, HttpModelAdapter};
use forge_work_worker::runner::StopFlag;
use std::{
    env,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::Arc,
};

fn port(key: &str, default: u16) -> Result<u16, String> {
    match env::var(key) {
        Err(env::VarError::NotPresent) => Ok(default),
        Ok(s) => s
            .parse::<u16>()
            .ok()
            .filter(|p| *p > 0)
            .ok_or_else(|| format!("Invalid {key}")),
        _ => Err(format!("Invalid {key}")),
    }
}
fn secret(path: &Path) -> Result<String, String> {
    if !path.is_absolute() {
        return Err("Expected absolute credential path".into());
    }
    let bytes = read_private(path, 4096, None)?;
    let value = std::str::from_utf8(&bytes)
        .map_err(|_| "Invalid credential file")?
        .trim();
    if value.is_empty() {
        return Err("Empty credential file".into());
    }
    Ok(value.into())
}
fn run() -> Result<(), String> {
    let path =
        env::var_os("FORGE_WORKER_MODEL_TOKEN_FILE").ok_or("Missing model credential path")?;
    let token = secret(Path::new(&path))?;
    if !(32..=128).contains(&token.len()) || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("Invalid model credential".into());
    }
    let model_port = port("FORGE_WORKER_MODEL_PORT", 18080)?;
    let store: Box<dyn GpuStorePort> = match (
        env::var_os("FORGE_PG_PROXY_URL"),
        env::var_os("FORGE_PG_PROXY_TOKEN_FILE"),
    ) {
        (Some(url), Some(file)) => Box::new(
            GpuProxyStore::new(
                url.to_str().ok_or("Invalid proxy URL")?,
                &secret(Path::new(&file))?,
            )
            .map_err(|e| e.to_string())?,
        ),
        (None, None) => {
            let path = PathBuf::from(env::var_os("PGPASSFILE").ok_or("Missing GPU passfile")?);
            Box::new(
                DirectGpuStore::from_passfile(&path, port("FORGE_WORKER_POSTGRES_PORT", 15432)?)
                    .map_err(|e| e.to_string())?,
            )
        }
        _ => return Err("GPU proxy requires URL and token file together".into()),
    };
    let stop = StopFlag::new();
    let signal = stop.clone();
    ctrlc::set_handler(move || signal.set())
        .map_err(|_| "Cannot install GPU worker stop handler")?;
    ForgeGpuWorker::new(store, Arc::new(HttpModelAdapter::new(token, model_port)))
        .run_forever(&stop);
    Ok(())
}
fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("GPU worker: {e}");
            ExitCode::FAILURE
        }
    }
}
