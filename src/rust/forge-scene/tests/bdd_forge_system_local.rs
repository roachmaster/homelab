//! Step bindings for features/forge_system_local.feature, driving the real
//! forge-system::disk registrar and forge-local::Application command bridge.

use forge_core::bootstrap::Registry;
use forge_core::bridge::DispatchError;
use forge_core::provider::Factory;
use forge_local::{
    AppError, Application, Command, DiskCommand, DiskResponse, LocalConfig, Output, RouteKey,
};
use forge_scene::bdd::{ensure, verify, Steps};
use forge_system::disk::{register as register_disk, ConfigError, DiskConfig};
use std::fs;
use std::path::PathBuf;
use uuid::Uuid;

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("forge-scene-disk-{}", Uuid::new_v4().simple()));
        fs::create_dir(&path).expect("create temp dir");
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[derive(Default)]
struct World {
    _dir: Option<TempDir>,
    reg_error: Option<ConfigError>,
    staged_ok: bool,
    create_failed: bool,
    app: Option<Application>,
    discovered: Vec<String>,
    listed_names: Vec<String>,
    invoke_error: Option<AppError>,
}

fn steps() -> Steps<World> {
    Steps::new()
        .given(
            "the disk registrar needs an explicit absolute starting directory",
            |_, _| Ok(()),
        )
        .given(
            "the disk registrar can stage configuration without filesystem access",
            |_, _| Ok(()),
        )
        .given(
            "the local host may start without the disk capability",
            |_, _| Ok(()),
        )
        .given(
            "the local host can enable the disk capability from a directory",
            |_, _| Ok(()),
        )
        .given(
            "the local host can list files in its configured directory",
            |_, _| Ok(()),
        )
        .given(
            "the local host can verify disk command variants against route headers",
            |_, _| Ok(()),
        )
        .when("registration receives a relative path", |w, _| {
            w.reg_error = register_disk(DiskConfig::Native {
                initial_directory: PathBuf::from("relative/path"),
            })
            .err();
            Ok(())
        })
        .when(
            "a nonexistent absolute directory is registered and frozen",
            |w, _| {
                let missing = std::env::temp_dir()
                    .join(format!("forge-scene-missing-{}", Uuid::new_v4().simple()));
                let group = register_disk(DiskConfig::Native {
                    initial_directory: missing,
                })
                .map_err(|e| e.to_string())?;
                let frozen = Registry::new()
                    .register(group)
                    .map_err(|e| format!("{:?}", e.reason()))?
                    .freeze()
                    .map_err(|e| format!("{:?}", e.reason()))?;
                w.staged_ok = true;
                let ((), factories) = frozen.into_factories();
                w.create_failed = factories.client.create().is_err();
                Ok(())
            },
        )
        .when("the application bootstraps with no disk directory", |w, _| {
            let app = Application::bootstrap(LocalConfig {
                node_id: "bdd-local".into(),
                disk_directory: None,
            })
            .map_err(|e| format!("{e:?}"))?;
            let cmds = app.commands().map_err(|e| format!("{e:?}"))?;
            w.discovered = cmds.into_iter().map(|c| c.name.to_string()).collect();
            w.app = Some(app);
            Ok(())
        })
        .when("the application bootstraps with a temporary directory", |w, _| {
            let dir = TempDir::new();
            let app = Application::bootstrap(LocalConfig {
                node_id: "bdd-local".into(),
                disk_directory: Some(dir.0.clone()),
            })
            .map_err(|e| format!("{e:?}"))?;
            let cmds = app.commands().map_err(|e| format!("{e:?}"))?;
            w.discovered = cmds.into_iter().map(|c| c.name.to_string()).collect();
            w._dir = Some(dir);
            w.app = Some(app);
            Ok(())
        })
        .when(
            "disk.list is invoked on a directory containing beta.txt and alpha.txt",
            |w, _| {
                let dir = TempDir::new();
                fs::write(dir.0.join("beta.txt"), b"b").map_err(|e| e.to_string())?;
                fs::write(dir.0.join("alpha.txt"), b"a").map_err(|e| e.to_string())?;
                let app = Application::bootstrap(LocalConfig {
                    node_id: "bdd-local".into(),
                    disk_directory: Some(dir.0.clone()),
                })
                .map_err(|e| format!("{e:?}"))?;
                let route = RouteKey::new("disk.list", 1).map_err(|e| e.to_string())?;
                let out = app
                    .invoke(
                        route,
                        Command::Disk(DiskCommand::List {
                            path: PathBuf::from("."),
                        }),
                    )
                    .map_err(|e| format!("{e:?}"))?;
                if let Output::Disk(DiskResponse::Listed(listing)) = out {
                    w.listed_names = listing
                        .entries
                        .into_iter()
                        .map(|e| e.name.to_string_lossy().into_owned())
                        .collect();
                }
                w._dir = Some(dir);
                w.app = Some(app);
                Ok(())
            },
        )
        .when("route disk.list receives a pwd command payload", |w, _| {
            let dir = TempDir::new();
            let app = Application::bootstrap(LocalConfig {
                node_id: "bdd-local".into(),
                disk_directory: Some(dir.0.clone()),
            })
            .map_err(|e| format!("{e:?}"))?;
            let route = RouteKey::new("disk.list", 1).map_err(|e| e.to_string())?;
            w.invoke_error = app.invoke(route, Command::Disk(DiskCommand::Pwd)).err();
            w._dir = Some(dir);
            w.app = Some(app);
            Ok(())
        })
        .then(
            "registration fails with an absolute-path configuration error",
            |w, _| {
                ensure(
                    w.reg_error == Some(ConfigError::InitialDirectoryMustBeAbsolute),
                    format!("unexpected reg_error: {:?}", w.reg_error),
                )
            },
        )
        .then(
            "staging succeeds and factory creation fails on the missing directory",
            |w, _| {
                ensure(w.staged_ok, "staging did not succeed")?;
                ensure(w.create_failed, "factory creation unexpectedly succeeded")
            },
        )
        .then("command discovery omits disk.list", |w, _| {
            ensure(
                !w.discovered.iter().any(|n| n == "disk.list"),
                format!("disk.list was present in {:?}", w.discovered),
            )
        })
        .then(
            "command discovery includes disk.pwd, disk.list, disk.cd, and disk.permissions",
            |w, _| {
                for expected in ["disk.pwd", "disk.list", "disk.cd", "disk.permissions"] {
                    ensure(
                        w.discovered.iter().any(|n| n == expected),
                        format!("{expected} missing from {:?}", w.discovered),
                    )?;
                }
                Ok(())
            },
        )
        .then("the listing returns alpha.txt before beta.txt", |w, _| {
            ensure(
                w.listed_names == vec!["alpha.txt".to_string(), "beta.txt".to_string()],
                format!("listed_names were {:?}", w.listed_names),
            )
        })
        .then("invocation fails with a payload mismatch error", |w, _| {
            ensure(
                matches!(
                    w.invoke_error,
                    Some(AppError::Dispatch(DispatchError::PayloadMismatch))
                ),
                format!("unexpected invoke_error: {:?}", w.invoke_error),
            )
        })
}

#[test]
fn forge_system_local_feature() {
    let report = verify(
        include_str!("../features/forge_system_local.feature"),
        &steps(),
    )
    .unwrap_or_else(|errors| panic!("forge_system_local.feature failed:\n{errors}"));
    assert_eq!(report.scenarios, 6);
}
