//! Native process construction and scoped GPU acquisition for a configured driver.

use crate::{backend_address, Config, Error, MAIL_MODEL_ID};
use forge_core::provider::{Factory, InstanceProvider, Transient};
use std::{
    fs::{File, OpenOptions, TryLockError},
    io::{self, Read},
    net::TcpListener,
    path::Path,
    process::{Child, Command, Stdio},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

pub(crate) trait ModelRuntimeProvider: Send + Sync {
    fn validate(&self, path: &Path) -> Result<(), Error>;
    fn acquire(&self) -> Result<Arc<dyn ModelRuntimeLease>, Error>;
}

/// An owned GPU reservation can be retained while the driver reaps and replaces a child.
pub(crate) trait ModelRuntimeLease: Send + Sync {
    fn spawn(&self, model: &str, path: &Path) -> Result<Box<dyn ModelProcess>, Error>;
}

/// The process handle must reap its owned child on drop, including failed startup.
pub(crate) trait ModelProcess: Send + Sync {
    fn exited(&mut self) -> io::Result<bool>;
    fn terminate(&mut self) -> io::Result<()>;
}

pub(crate) struct NativeModelProvider {
    config: Arc<Config>,
    leases: Transient<GpuLeaseFactory>,
}

impl NativeModelProvider {
    pub(crate) fn new(config: Arc<Config>) -> Self {
        Self {
            leases: Transient::new(GpuLeaseFactory(config.clone())),
            config,
        }
    }
}

impl ModelRuntimeProvider for NativeModelProvider {
    fn validate(&self, path: &Path) -> Result<(), Error> {
        let mut magic = [0; 4];
        if !path.is_file()
            || File::open(path)
                .and_then(|mut file| file.read_exact(&mut magic))
                .is_err()
            || &magic != b"GGUF"
        {
            return Err(Error::ModelUnavailable);
        }
        if let Some(device) = &self.config.gpu_device {
            let render_node_present =
                std::fs::read_dir(device.join("drm"))
                    .ok()
                    .is_some_and(|entries| {
                        entries
                            .filter_map(Result::ok)
                            .any(|entry| entry.file_name().to_string_lossy().starts_with("renderD"))
                    });
            if !device.join("driver").is_dir() || !render_node_present {
                return Err(Error::GpuUnavailable);
            }
        }
        Ok(())
    }

    fn acquire(&self) -> Result<Arc<dyn ModelRuntimeLease>, Error> {
        Ok(Arc::new(self.leases.acquire()?.into_inner()))
    }
}

struct GpuLeaseFactory(Arc<Config>);

impl Factory<LockedRuntime> for GpuLeaseFactory {
    type Error = Error;

    fn create(&self) -> Result<LockedRuntime, Error> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&self.0.gpu_lock)
            .map_err(|_| Error::StartFailed)?;
        file.try_lock().map_err(|err| match err {
            TryLockError::WouldBlock => Error::Busy,
            TryLockError::Error(_) => Error::StartFailed,
        })?;
        Ok(LockedRuntime {
            config: self.0.clone(),
            _gpu_lock: file,
        })
    }
}

struct LockedRuntime {
    config: Arc<Config>,
    _gpu_lock: File,
}

impl ModelRuntimeLease for LockedRuntime {
    fn spawn(&self, model: &str, path: &Path) -> Result<Box<dyn ModelProcess>, Error> {
        let factory = ModelProcessFactory {
            config: &self.config,
            model,
            path,
        };
        Ok(Box::new(factory.create()?))
    }
}

struct ModelProcessFactory<'a> {
    config: &'a Config,
    model: &'a str,
    path: &'a Path,
}

impl Factory<NativeModelProcess> for ModelProcessFactory<'_> {
    type Error = Error;

    fn create(&self) -> Result<NativeModelProcess, Error> {
        drop(
            TcpListener::bind(backend_address(self.config.backend_port))
                .map_err(|_| Error::Busy)?,
        );
        let mut command = Command::new(&self.config.server);
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LC_ALL", "C");
        if let Some(paths) = &self.config.library_path {
            command.env("LD_LIBRARY_PATH", paths);
        }
        if self.config.device.starts_with("SYCL") {
            command
                .env("ONEAPI_DEVICE_SELECTOR", "level_zero:0")
                .env("GGML_SYCL_ENABLE_GRAPH", "0")
                .env("GGML_SYCL_FA_ONEDNN_MAX_KV", "32768");
        }
        let mail = self
            .config
            .mail_expert_paging
            .as_ref()
            .filter(|_| self.model == MAIL_MODEL_ID);
        let context_size = mail.map_or(self.config.context_size, |profile| profile.context_size);
        command
            .arg("--model")
            .arg(self.path)
            .args(["--alias", self.model, "--host", "127.0.0.1", "--port"])
            .arg(self.config.backend_port.to_string())
            .args([
                "--device",
                &self.config.device,
                "--gpu-layers",
                "all",
                "--fit",
                "off",
                "--ctx-size",
            ])
            .arg(context_size.to_string())
            .args([
                "--parallel",
                "1",
                "--threads",
                "8",
                "--offline",
                "--jinja",
                "--no-ui",
                "--no-warmup",
                "--reasoning-format",
                "deepseek",
                "--api-key-file",
            ])
            .arg(&self.config.token_file);
        if mail.is_some() {
            command
                .env("ZES_ENABLE_SYSMAN", "1")
                .env_remove("GGML_SYCL_FA_ONEDNN_MAX_KV");
            command.args([
                "--cpu-moe",
                "--moe-cache-mib",
                "4096",
                "--load-mode",
                "mmap",
                "--no-repack",
                "--batch-size",
                "8",
                "--ubatch-size",
                "1",
                "--reasoning-effort",
                "low",
                "--no-context-shift",
                "--cache-ram",
                "0",
                "--no-cache-idle-slots",
            ]);
        } else {
            command.args([
                "--batch-size",
                "128",
                "--ubatch-size",
                "128",
                "--flash-attn",
                "on",
                "--verbosity",
                "4",
                "--cache-type-k",
                "f16",
                "--cache-type-v",
                "f16",
            ]);
        }
        let child = command.stdin(Stdio::null()).spawn().map_err(|err| {
            eprintln!("llama-server spawn failed: {err}");
            Error::StartFailed
        })?;
        Ok(NativeModelProcess { child })
    }
}

struct NativeModelProcess {
    child: Child,
}

impl ModelProcess for NativeModelProcess {
    fn exited(&mut self) -> io::Result<bool> {
        self.child.try_wait().map(|status| status.is_some())
    }

    fn terminate(&mut self) -> std::io::Result<()> {
        if self.child.try_wait()?.is_some() {
            return Ok(());
        }
        let pid = i32::try_from(self.child.id()).map_err(std::io::Error::other)?;
        use nix::{
            sys::signal::{kill, Signal},
            unistd::Pid,
        };
        if let Err(error) = kill(Pid::from_raw(pid), Signal::SIGTERM) {
            if self.child.try_wait()?.is_some() {
                return Ok(());
            }
            return Err(std::io::Error::from_raw_os_error(error as i32));
        }
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            if self.child.try_wait()?.is_some() {
                return Ok(());
            }
            if Instant::now() >= deadline {
                self.child.kill()?;
                self.child.wait()?;
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "llama-server forced to exit",
                ));
            }
            thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for NativeModelProcess {
    fn drop(&mut self) {
        if let Err(error) = self.terminate() {
            eprintln!("llama-server cleanup: {error}");
        }
    }
}
