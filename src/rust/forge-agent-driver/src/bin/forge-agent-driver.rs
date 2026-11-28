//! forge-agent-driver: compare independent Forge chat responses on
//! fixed command fixtures (benchmark.py's main).

use std::ffi::CString;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use forge_agent_driver::benchmark::{
    DRIVER, LOCK_PATH, MODELS, RESULTS_PATH, TOKEN_PATH, TokenFileConnector, check_fixtures,
    load_cases, run_benchmark,
};
use nix::fcntl::{Flock, FlockArg};

fn parser_error(message: &str) -> ! {
    eprintln!("forge-agent-driver: error: {message}");
    std::process::exit(2);
}

fn main() -> ExitCode {
    let mut check_only = false;
    let mut models: Vec<String> = Vec::new();
    let mut results = PathBuf::from(RESULTS_PATH);
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--check" => check_only = true,
            "--model" => match args.next() {
                Some(model) => models.push(model),
                None => parser_error("--model requires a value"),
            },
            "--results" => match args.next() {
                Some(path) => results = PathBuf::from(path),
                None => parser_error("--results requires a value"),
            },
            _ => parser_error(&format!("unrecognized argument {arg}")),
        }
    }
    let hostname = nix::unistd::gethostname()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    if hostname.split('.').next() != Some("mission-control") || nix::unistd::geteuid().as_raw() != 0
    {
        parser_error("Run on Mission Control with sudo");
    }
    let account = match nix::unistd::User::from_name(DRIVER) {
        Ok(Some(account)) => account,
        _ => parser_error("Create the driver account using setup-user.sh"),
    };
    let groups = CString::new(DRIVER)
        .ok()
        .and_then(|name| nix::unistd::getgrouplist(&name, account.gid).ok())
        .unwrap_or_default();
    if account.uid.as_raw() == 0
        || account.gid.as_raw() == 0
        || account.dir != Path::new("/nonexistent")
        || account.shell != Path::new("/usr/sbin/nologin")
        || groups != [account.gid]
    {
        parser_error("The driver account must have its own group, no home and no login shell");
    }
    let cases_path = std::env::current_exe()
        .map(|exe| exe.with_file_name("cases.yaml"))
        .unwrap_or_else(|_| PathBuf::from("cases.yaml"));
    let cases = match load_cases(&cases_path) {
        Ok(cases) => cases,
        Err(error) => {
            eprintln!("forge-agent-driver: {error}");
            return ExitCode::from(1);
        }
    };
    let identity = (account.uid.as_raw(), account.gid.as_raw());
    let fixtures = std::env::temp_dir().join(format!(
        "forge-agent-fixtures-{}",
        std::process::id()
    ));
    let run = (|| -> Result<i32, String> {
        std::fs::create_dir(&fixtures).map_err(|error| error.to_string())?;
        std::fs::set_permissions(&fixtures, std::fs::Permissions::from_mode(0o711))
            .map_err(|error| error.to_string())?;
        check_fixtures(&cases, &fixtures, identity)?;
        if check_only {
            println!("{} fixture references passed", cases.len());
            return Ok(0);
        }
        let lock_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(LOCK_PATH)
            .map_err(|error| error.to_string())?;
        let _lock = match Flock::lock(lock_file, FlockArg::LockExclusiveNonblock) {
            Ok(lock) => lock,
            Err(_) => {
                return Err("A Forge command comparison is already running".to_string());
            }
        };
        let mut selected: Vec<String> = Vec::new();
        for model in if models.is_empty() {
            MODELS.iter().map(|model| model.to_string()).collect()
        } else {
            models.clone()
        } {
            if !selected.contains(&model) {
                selected.push(model);
            }
        }
        run_benchmark(
            &TokenFileConnector::new(TOKEN_PATH),
            &selected,
            &results,
            &cases,
            &fixtures,
            identity,
        )
            .map_err(|error| error.0)
    })();
    let _ = std::fs::remove_dir_all(&fixtures);
    match run {
        Ok(code) => ExitCode::from(code as u8),
        Err(error) => {
            eprintln!("forge-agent-driver: {error}");
            ExitCode::from(2)
        }
    }
}
