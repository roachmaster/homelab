//! Runs only against a uniquely created temporary fixture, then removes that
//! owned fixture. The native client itself exposes no create/delete operation.
use forge_bootstrap_demo::{
    disk::{
        bootstrap_disk, change_directory_route, list_current_route, permissions_route, pwd_route,
        DiskApplication, DiskCommand,
    },
    DemoContext,
};
use forge_ingress_contract::{Envelope, Header, RouteKey};
use forge_kernel::disk::DiskConfig;
use std::{
    error::Error,
    fs, io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

struct Fixture(Option<PathBuf>);
impl Fixture {
    fn create() -> io::Result<Self> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        // The temporary directory is only an allocation base, never listed or
        // used as the client's cwd. No browsing path is read from CLI/config/env.
        for attempt in 0..128 {
            let path = std::env::temp_dir().join(format!(
                "forge-disk-example-{}-{nonce}-{attempt}",
                std::process::id()
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self(Some(path))),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a unique demo fixture",
        ))
    }
    fn path(&self) -> &Path {
        self.0.as_deref().expect("fixture is still owned")
    }
    fn cleanup(mut self) -> io::Result<()> {
        fs::remove_dir_all(self.path())?;
        self.0 = None;
        Ok(())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            let _ = fs::remove_dir_all(path); // Best-effort fallback on early error.
        }
    }
}

fn dispatch(
    app: &DiskApplication,
    route: RouteKey,
    command: DiskCommand,
) -> Result<(), Box<dyn Error>> {
    let request = Envelope::new(Header::new(route, "disk-example")?, command);
    // This flag demonstrates bridge admission, not production authentication.
    println!(
        "{:?}",
        app.dispatch(request, &DemoContext { allow: true })?.payload
    );
    Ok(())
}
fn main() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::create()?;
    fs::create_dir(fixture.path().join("notes"))?;
    fs::write(
        fixture.path().join("notes/hello.txt"),
        b"Native Rust filesystem access.\n",
    )?;
    let app = bootstrap_disk(DiskConfig::Native {
        initial_directory: fs::canonicalize(fixture.path())?,
    })?;
    dispatch(&app, pwd_route(), DiskCommand::Pwd)?;
    dispatch(&app, list_current_route(), DiskCommand::ListCurrent)?;
    dispatch(
        &app,
        change_directory_route(),
        DiskCommand::ChangeDirectory {
            path: "notes".into(),
        },
    )?;
    dispatch(&app, list_current_route(), DiskCommand::ListCurrent)?;
    dispatch(
        &app,
        permissions_route(),
        DiskCommand::Permissions {
            path: "hello.txt".into(),
            follow_symlinks: false,
        },
    )?;
    drop(app);
    fixture.cleanup()?;
    Ok(())
}
