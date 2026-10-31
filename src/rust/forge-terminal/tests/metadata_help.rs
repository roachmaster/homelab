use forge_local::{Application, CommandInfo, CommandKind, DataType, LocalConfig, Presence};
use forge_terminal::{
    console::StreamConsole,
    help::{command_help, usage, HELP_OPTION},
    parser::decode,
    run,
};
use std::{ffi::OsString, io::Cursor};

fn args(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}
fn invoke(values: &[&str], input: &[u8]) -> (u8, String, String) {
    let console = StreamConsole::new(Cursor::new(input.to_vec()), Vec::new(), Vec::new());
    let status = run(args(values), &console);
    let (_, output, errors) = console.into_parts();
    (
        status,
        String::from_utf8(output).unwrap(),
        String::from_utf8(errors).unwrap(),
    )
}
fn node_catalog() -> Vec<CommandInfo> {
    Application::bootstrap(LocalConfig::default())
        .unwrap()
        .commands()
        .unwrap()
}

#[test]
fn reusable_help_option_is_typed_and_not_a_business_argument() {
    assert_eq!(HELP_OPTION.argument.name, "help");
    assert_eq!(HELP_OPTION.argument.data_type, DataType::Bool);
    assert_eq!(
        HELP_OPTION.argument.presence,
        Presence::Optional {
            default: Some("false")
        }
    );
    assert!(!HELP_OPTION.argument.help.is_empty());
    assert!(HELP_OPTION.matches("-h".as_ref()));
    assert!(HELP_OPTION.matches("--help".as_ref()));
    assert!(!HELP_OPTION.matches("--help=true".as_ref()));
    for info in node_catalog() {
        assert_eq!(info.descriptor.name, info.route.destination());
        assert!(info.descriptor.arguments.is_empty());
        assert!(matches!(
            info.kind,
            CommandKind::Commands | CommandKind::NodeInfo | CommandKind::NodeStatus
        ));
        assert!(decode(&info, &[]).is_ok());
        assert!(decode(&info, &args(&["unexpected"])).is_err());
    }
}

#[test]
fn exact_help_forms_return_the_authoritative_description() {
    let catalog = node_catalog();
    let info = catalog
        .iter()
        .find(|info| info.kind == CommandKind::NodeInfo)
        .unwrap();
    for tail in [
        vec!["node.info", "--help"],
        vec!["node.info", "-h"],
        vec!["help", "node.info"],
    ] {
        let mut arguments = vec!["--no-disk"];
        arguments.extend(tail);
        let (code, out, err) = invoke(&arguments, b"");
        assert_eq!(code, 0);
        assert!(err.is_empty());
        assert!(out.contains(info.descriptor.help));
        assert!(out.contains("Command: node.info\n"));
        assert!(out.contains("Arguments:\n  (none)"));
        assert!(out.contains("help: Bool (optional, default=false)"));
        assert!(!out.contains("target_os="));
    }
    for tail in ["--help", "-h", "help"] {
        let (code, out, err) = invoke(&["--no-disk", tail], b"");
        assert_eq!(code, 0);
        assert!(err.is_empty());
        assert!(out.contains("Registered commands:"));
    }
}

#[test]
fn malformed_extra_arguments_never_silently_trigger_help() {
    for tail in [
        vec!["node.info", "--help", "extra"],
        vec!["node.info", "extra", "--help"],
        vec!["help", "node.info", "extra"],
        vec!["--help", "node.info"],
        vec!["help", "missing"],
        vec!["missing", "--help"],
    ] {
        let mut arguments = vec!["--no-disk"];
        arguments.extend(tail);
        let (code, out, err) = invoke(&arguments, b"");
        assert_eq!(code, 2);
        assert!(out.is_empty());
        assert!(!err.is_empty());
    }
}

#[test]
fn session_supports_command_help_without_execution_and_rejects_extra_help_arguments() {
    let (code, out, err) = invoke(
        &["--no-disk", "--session"],
        b"help node.info\nnode.status --help\nnode.info -h\nexit\n",
    );
    assert_eq!(code, 0);
    assert!(err.is_empty());
    assert!(out.contains("Command: node.info"));
    assert!(out.contains("Command: node.status"));
    assert!(!out.contains("target_os="));
    assert!(!out.contains("instance_uptime_ms="));
    let (code, out, err) = invoke(
        &["--no-disk", "--session"],
        b"node.info --help extra\nhelp node.info extra\nexit\n",
    );
    assert_eq!(code, 2);
    assert!(!out.contains("Command:"));
    assert!(!err.is_empty());
}

#[test]
fn descriptor_display_preserves_semantic_type_and_default_not_executable_schema() {
    use forge_local::{ArgumentDescriptor, CommandDescriptor, RouteKey};
    static DESCRIPTOR: CommandDescriptor = CommandDescriptor {
        name: "example.inspect",
        help: "A display-only fixture.",
        arguments: &[ArgumentDescriptor {
            name: "resource",
            help: "Capability-owned input.",
            data_type: DataType::Custom("ResourceId"),
            presence: Presence::Required,
        }],
    };
    let info = CommandInfo {
        route: RouteKey::new(DESCRIPTOR.name, 1).unwrap(),
        kind: CommandKind::NodeInfo,
        descriptor: &DESCRIPTOR,
    };
    assert_eq!(
        usage(&info),
        "example.inspect <resource:Custom(ResourceId)>"
    );
    assert!(command_help(&info).contains("resource: Custom(ResourceId) (required)"));
    // Metadata does not invent a converter or override the explicit NodeInfo codec.
    assert!(decode(&info, &args(&["resource-id"])).is_err());
}

#[cfg(feature = "disk")]
mod disk {
    use super::*;
    use forge_local::{Command, DiskCommand};
    use std::path::PathBuf;
    fn catalog() -> Vec<CommandInfo> {
        let config = LocalConfig {
            disk_directory: Some(std::env::current_dir().unwrap()),
            ..LocalConfig::default()
        };
        Application::bootstrap(config).unwrap().commands().unwrap()
    }
    #[test]
    fn path_and_boolean_descriptors_agree_with_explicit_codecs() {
        let catalog = catalog();
        let list = catalog
            .iter()
            .find(|info| info.kind == CommandKind::DiskList)
            .unwrap();
        assert_eq!(list.descriptor.arguments.len(), 1);
        let path = &list.descriptor.arguments[0];
        assert_eq!(
            (path.name, path.data_type, path.presence),
            (
                "path",
                DataType::Path,
                Presence::Optional { default: Some(".") }
            )
        );
        assert_eq!(usage(list), "disk.list [path:Path=.]");
        assert_eq!(
            decode(list, &[]).unwrap(),
            Command::Disk(DiskCommand::List {
                path: PathBuf::from(".")
            })
        );
        let cd = catalog
            .iter()
            .find(|info| info.kind == CommandKind::DiskCd)
            .unwrap();
        assert_eq!(
            (
                cd.descriptor.arguments[0].name,
                cd.descriptor.arguments[0].data_type,
                cd.descriptor.arguments[0].presence
            ),
            ("path", DataType::Path, Presence::Required)
        );
        assert!(decode(cd, &[]).is_err());
        let permissions = catalog
            .iter()
            .find(|info| info.kind == CommandKind::DiskPermissions)
            .unwrap();
        assert_eq!(permissions.descriptor.arguments.len(), 2);
        let follow = &permissions.descriptor.arguments[1];
        assert_eq!(
            (follow.name, follow.data_type, follow.presence),
            (
                "follow_symlinks",
                DataType::Bool,
                Presence::Optional {
                    default: Some("true")
                }
            )
        );
        assert_eq!(
            usage(permissions),
            "disk.permissions [--no-follow] <path:Path>"
        );
        assert!(
            command_help(permissions).contains("follow_symlinks: Bool (optional, default=true)")
        );
        for (tokens, expected) in [(vec!["."], true), (vec!["--no-follow", "."], false)] {
            assert_eq!(
                decode(permissions, &args(&tokens)).unwrap(),
                Command::Disk(DiskCommand::Permissions {
                    path: PathBuf::from("."),
                    follow_symlinks: expected
                })
            );
        }
        for tokens in [
            vec!["--unknown", "."],
            vec!["--no-follow=false", "."],
            vec!["--follow-symlinks", "maybe", "."],
            vec![""],
        ] {
            assert!(decode(permissions, &args(&tokens)).is_err());
        }
    }
    #[test]
    fn unknown_flags_reject_but_explicit_native_paths_survive() {
        let catalog = catalog();
        let list = catalog
            .iter()
            .find(|info| info.kind == CommandKind::DiskList)
            .unwrap();
        assert!(decode(list, &args(&["--bad"])).is_err());
        assert!(decode(list, &args(&["--help", "extra"])).is_err());
        assert_eq!(
            decode(list, &args(&["./--help"])).unwrap(),
            Command::Disk(DiskCommand::List {
                path: PathBuf::from("./--help")
            })
        );
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            let raw = OsString::from_vec(vec![b'.', b'/', 0xff]);
            assert_eq!(
                decode(list, std::slice::from_ref(&raw)).unwrap(),
                Command::Disk(DiskCommand::List {
                    path: PathBuf::from(raw)
                })
            );
        }
    }
    #[test]
    fn disk_help_never_needs_required_path_and_absent_disk_remains_absent() {
        for arguments in [
            vec!["disk.cd", "--help"],
            vec!["help", "disk.permissions"],
            vec!["disk.list", "-h"],
        ] {
            let (code, out, err) = invoke(&arguments, b"");
            assert_eq!(code, 0);
            assert!(err.is_empty());
            assert!(out.contains("path: Path"));
            assert!(!out.contains("cwd="));
            assert!(!out.contains("directory="));
        }
        assert_eq!(invoke(&["--no-disk", "disk.cd", "--help"], b"").0, 2);
        assert_eq!(invoke(&["disk.cd", "--help", "extra"], b"").0, 2);
    }
}
