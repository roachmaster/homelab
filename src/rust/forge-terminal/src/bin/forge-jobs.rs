#[cfg(unix)]
fn main() -> std::process::ExitCode {
    native::run()
}

#[cfg(not(unix))]
fn main() -> std::process::ExitCode {
    eprintln!("forge-jobs requires a Unix operator account");
    std::process::ExitCode::FAILURE
}

#[cfg(unix)]
mod native {
    use forge_core::access::{AuthenticatedBoundary, Grant, IdentityFacts, RoutePolicy, Verifier};
    use forge_core::bootstrap::{PreparedGroup, Registry};
    use forge_core::ingress::{Envelope, Header, RouteKey};
    use forge_core::io_client::IoClient;
    use forge_core::provider::{Factory, InstanceProvider, Singleton};
    use forge_jobs::{
        authenticated_bridge, register_private_store, register_worker, Command, JobId, Reply,
        StoreConfig, Work, Worker, WorkerConfig,
    };
    use forge_terminal::console::{register_stdio, ConsoleRequest, StdioConsole};
    use std::{
        env,
        error::Error,
        ffi::OsString,
        io,
        path::PathBuf,
        process::ExitCode,
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
        thread,
        time::{Duration, Instant},
    };

    const USAGE: &str = "forge-jobs DIRECTORY submit KEY THROUGH | status ID | cancel ID | worker";
    const CALL_BUDGET: Duration = Duration::from_secs(5);
    const ROUTES: [&str; 3] = ["jobs.submit", "jobs.status", "jobs.cancel"];

    struct OperatorFactory(u32);
    struct UnixOperator(u32);

    impl Factory<UnixOperator> for OperatorFactory {
        type Error = io::Error;

        fn create(&self) -> io::Result<UnixOperator> {
            Ok(UnixOperator(self.0))
        }
    }

    impl Verifier<()> for UnixOperator {
        type Error = io::Error;

        fn verify(&self, (): ()) -> io::Result<IdentityFacts> {
            if self.0 != nix::unistd::geteuid().as_raw() {
                return Err(io::Error::from(io::ErrorKind::PermissionDenied));
            }
            Ok(IdentityFacts {
                subject: format!("uid-{}", self.0),
                valid_until: Instant::now() + CALL_BUDGET,
            })
        }
    }

    enum Action {
        Worker,
        Dispatch(&'static str, Command),
    }

    pub fn run() -> ExitCode {
        match console() {
            Ok(provider) => {
                let console = provider.acquire().expect("owned console");
                match invoke(env::args_os().skip(1).collect(), console) {
                    Ok(code) => ExitCode::from(code),
                    Err(error) => {
                        let _ = console.execute(ConsoleRequest::WriteErr(format!("{error}\n")));
                        ExitCode::FAILURE
                    }
                }
            }
            Err(error) => {
                eprintln!("console setup failed: {error}");
                ExitCode::FAILURE
            }
        }
    }

    fn console() -> Result<Singleton<StdioConsole>, Box<dyn Error>> {
        let ((), factory) = Registry::new()
            .register(register_stdio()?)?
            .freeze()?
            .into_factories();
        Ok(Singleton::new(factory.create()?))
    }

    fn parse(args: &[OsString]) -> Option<(PathBuf, Action)> {
        let (directory, rest) = args.split_first()?;
        let action = match rest {
            [verb] if verb == "worker" => Action::Worker,
            [verb, key, through] if verb == "submit" => Action::Dispatch(
                ROUTES[0],
                Command::Submit {
                    key: key.to_str()?.into(),
                    work: Work::Sum {
                        through: through.to_str()?.parse().ok()?,
                    },
                },
            ),
            [verb, id] if verb == "status" || verb == "cancel" => {
                let id = JobId(id.to_str()?.parse().ok()?);
                if verb == "status" {
                    Action::Dispatch(ROUTES[1], Command::Status { id })
                } else {
                    Action::Dispatch(ROUTES[2], Command::Cancel { id })
                }
            }
            _ => return None,
        };
        Some((PathBuf::from(directory), action))
    }

    fn invoke(args: Vec<OsString>, console: &StdioConsole) -> Result<u8, Box<dyn Error>> {
        if matches!(args.as_slice(), [flag] if flag == "--help") {
            console.execute(ConsoleRequest::WriteOut(format!("{USAGE}\n")))?;
            return Ok(0);
        }
        let Some((directory, action)) = parse(&args) else {
            console.execute(ConsoleRequest::WriteErr(format!("Usage: {USAGE}\n")))?;
            return Ok(2);
        };
        let uid = nix::unistd::geteuid().as_raw();
        let identity = PreparedGroup::new(
            "terminal.jobs-operator",
            &["terminal.jobs-identity"],
            &[],
            OperatorFactory(uid),
        )?;
        let store = register_private_store(
            StoreConfig {
                directory,
                max_jobs: 4096,
            },
            uid,
        )?;
        let worker = register_worker(WorkerConfig {
            poll_interval: Duration::from_millis(100),
            lease: Duration::from_secs(10),
            retries: 2,
        })?;
        let ((((), identity), store), worker) = Registry::new()
            .register(identity)?
            .register(store)?
            .register(worker)?
            .freeze()?
            .into_factories();

        let boundary = Singleton::new(AuthenticatedBoundary::new(identity.create()?, CALL_BUDGET));
        let context = boundary
            .acquire()?
            .authenticate((), CALL_BUDGET)
            .map_err(|_| io::Error::from(io::ErrorKind::PermissionDenied))?;
        let store = Singleton::new(Arc::new(store.create()?));
        let store = Arc::clone(store.acquire()?);
        match action {
            Action::Worker => {
                let stopping = Arc::new(AtomicBool::new(false));
                let signal = Arc::clone(&stopping);
                ctrlc::set_handler(move || signal.store(true, Ordering::Release))?;
                supervise(worker.bind(store).create()?, &stopping)?;
            }
            Action::Dispatch(destination, command) => {
                let grants = ROUTES.map(|route| Grant {
                    subject: context.subject().into(),
                    route: RouteKey::new(route, 1).expect("fixed durable job route"),
                    discoverable: true,
                });
                let policy = RoutePolicy::new(grants)
                    .map_err(|_| io::Error::other("invalid local job admission policy"))?;
                let dispatch = authenticated_bridge(store, policy);
                let header = Header::new(RouteKey::new(destination, 1)?, "terminal-job")?;
                let reply = dispatch.dispatch(Envelope::new(header, command), &context)?;
                let text = match reply.payload {
                    Reply::Submitted(receipt) => format!("{}\n", receipt.id.0),
                    Reply::Status(status) | Reply::Cancelled(status) => {
                        format!("{} {:?}\n", status.id.0, status.state)
                    }
                };
                console.execute(ConsoleRequest::WriteOut(text))?;
            }
        }
        Ok(0)
    }

    fn supervise(worker: Worker, stopping: &AtomicBool) -> Result<(), Box<dyn Error>> {
        while !stopping.load(Ordering::Acquire) && !worker.is_finished() {
            thread::sleep(Duration::from_millis(100));
        }
        worker.shutdown()?;
        Ok(())
    }
}
