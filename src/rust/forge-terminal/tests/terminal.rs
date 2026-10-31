use forge_io_client::IoClient;
use forge_terminal::{
    console::{ConsoleRequest, ConsoleResponse, InputLine, StreamConsole, MAX_LINE_BYTES},
    parser::{parse_options, quote_os, session_arguments, Mode},
    run,
};
use std::{
    ffi::{OsStr, OsString},
    io::Cursor,
};

fn arguments(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}
fn execute(args: &[&str], input: &[u8]) -> (u8, String, String) {
    let console = StreamConsole::new(Cursor::new(input.to_vec()), Vec::new(), Vec::new());
    let status = run(arguments(args), &console);
    let (_, output, errors) = console.into_parts();
    (
        status,
        String::from_utf8(output).unwrap(),
        String::from_utf8(errors).unwrap(),
    )
}

#[test]
fn console_executes_prepared_writes_and_reports_eof() {
    let console = StreamConsole::new(Cursor::new(Vec::<u8>::new()), Vec::new(), Vec::new());
    assert_eq!(
        console
            .execute(ConsoleRequest::WriteOut("prepared text\n".into()))
            .unwrap(),
        ConsoleResponse::Written
    );
    console
        .execute(ConsoleRequest::WriteErr("prepared error\n".into()))
        .unwrap();
    assert_eq!(
        console.execute(ConsoleRequest::ReadLine).unwrap(),
        ConsoleResponse::Input(InputLine::End)
    );
    let (_, out, err) = console.into_parts();
    assert_eq!(out, b"prepared text\n");
    assert_eq!(err, b"prepared error\n");
}

#[test]
fn bounded_console_rejects_then_resynchronizes() {
    let mut input = vec![b'x'; MAX_LINE_BYTES + 100_000];
    input.extend_from_slice(b"\nnode.info\n");
    let console = StreamConsole::new(Cursor::new(input), Vec::new(), Vec::new());
    assert!(matches!(
        console.execute(ConsoleRequest::ReadLine).unwrap(),
        ConsoleResponse::Input(InputLine::Rejected(_))
    ));
    assert_eq!(
        console.execute(ConsoleRequest::ReadLine).unwrap(),
        ConsoleResponse::Input(InputLine::Line("node.info".into()))
    );
}

#[test]
fn exact_limit_crlf_and_invalid_utf8_are_handled() {
    let mut input = vec![b'x'; MAX_LINE_BYTES];
    input.extend_from_slice(b"\r\n\xff\nlast");
    let console = StreamConsole::new(Cursor::new(input), Vec::new(), Vec::new());
    assert_eq!(
        console.execute(ConsoleRequest::ReadLine).unwrap(),
        ConsoleResponse::Input(InputLine::Line("x".repeat(MAX_LINE_BYTES)))
    );
    assert!(matches!(
        console.execute(ConsoleRequest::ReadLine).unwrap(),
        ConsoleResponse::Input(InputLine::Rejected(_))
    ));
    assert_eq!(
        console.execute(ConsoleRequest::ReadLine).unwrap(),
        ConsoleResponse::Input(InputLine::Line("last".into()))
    );
}

#[test]
fn malformed_startup_options_do_not_build_the_app() {
    for args in [
        vec!["--bad"],
        vec!["--node-id"],
        vec!["--node-id", "x", "--node-id", "y"],
        vec!["--session", "node.info"],
    ] {
        let (code, out, err) = execute(&args, b"");
        assert_eq!(code, 2);
        assert!(out.is_empty());
        assert!(err.starts_with("error:"));
    }
}

#[test]
fn invalid_typed_config_is_usage_error() {
    let (code, out, err) = execute(&["--no-disk", "--node-id", "bad id", "node.info"], b"");
    assert_eq!(code, 2);
    assert!(out.is_empty());
    assert!(!err.is_empty());
}

#[test]
fn discovery_tracks_selected_capabilities() {
    let (code, out, err) = execute(&["--no-disk", "--help"], b"");
    assert_eq!(code, 0);
    assert!(err.is_empty());
    assert!(out.contains("node.info"));
    assert!(out.contains("node.status"));
    assert!(out.contains("system.commands"));
    assert!(!out.contains("disk.pwd"));
    let (code, out, err) = execute(&["--no-disk", "disk.pwd"], b"");
    assert_eq!(code, 2);
    assert!(out.is_empty());
    assert!(err.contains("unavailable"));
}

#[test]
fn node_info_status_and_unknown_command_have_explicit_outcomes() {
    let (code, out, err) = execute(&["--no-disk", "--node-id", "node-test", "node.info"], b"");
    assert_eq!(code, 0);
    assert!(err.is_empty());
    assert!(out.contains("node_id=node-test\n"));
    let (code, out, err) = execute(&["--no-disk", "node.status"], b"");
    assert_eq!(code, 0);
    assert!(err.is_empty());
    assert!(out.contains("observation=responding\n"));
    assert!(out.contains("not OS uptime or overall health"));
    assert_eq!(execute(&["--no-disk", "node.info", "extra"], b"").0, 2);
    assert_eq!(execute(&["--no-disk", "not-a-command"], b"").0, 2);
}

#[test]
fn session_is_literal_not_a_shell_and_continues_after_rejection() {
    assert_eq!(
        session_arguments(" disk.cd path with spaces "),
        arguments(&["disk.cd", "path with spaces"])
    );
    assert_eq!(
        session_arguments("disk.cd '$HOME'"),
        arguments(&["disk.cd", "'$HOME'"])
    );
    assert_eq!(
        session_arguments("disk.permissions --no-follow link with spaces"),
        arguments(&["disk.permissions", "--no-follow link with spaces"])
    );
    let (code, out, err) = execute(
        &["--no-disk", "--session"],
        b"bad-command\nnode.info\nexit\n",
    );
    assert_eq!(code, 2);
    assert!(out.contains("node_id=local\n"));
    assert!(err.contains("unavailable"));
}

#[test]
fn control_characters_are_escaped() {
    let text = quote_os(OsStr::new("a\x1b[31m\n\t\"\\"));
    assert_eq!(text, "\"a\\x1b[31m\\x0a\\x09\\\"\\\\\"");
    let (code, out, err) = execute(&["--no-disk", "\u{1b}[2J"], b"");
    assert_eq!(code, 2);
    assert!(out.is_empty());
    assert!(!err.contains('\u{1b}'));
}

#[test]
fn default_mode_is_help() {
    assert_eq!(parse_options(Vec::new()).unwrap().mode, Mode::Help);
}

#[test]
fn input_and_output_failures_propagate_without_retries() {
    use std::{cell::Cell, io};
    struct FailedConsole(Cell<usize>);
    impl IoClient for FailedConsole {
        type Request = ConsoleRequest;
        type Response = ConsoleResponse;
        type Error = io::Error;
        fn execute(&self, _: ConsoleRequest) -> io::Result<ConsoleResponse> {
            self.0.set(self.0.get() + 1);
            Err(io::Error::other("console unavailable"))
        }
    }
    let console = FailedConsole(Cell::new(0));
    assert_eq!(run(arguments(&["--no-disk", "node.info"]), &console), 1);
    // One failed response write, then one diagnostic attempt; no operation retry.
    assert_eq!(console.0.get(), 2);
}

#[cfg(unix)]
#[test]
fn native_invalid_utf8_bytes_survive_argument_parsing_and_escape() {
    use std::os::unix::ffi::OsStringExt;
    let path = OsString::from_vec(vec![b'a', 0xff, 0x1b]);
    assert_eq!(quote_os(&path), "\"a\\xff\\x1b\"");
    let options = parse_options(vec![OsString::from("disk.cd"), path.clone()]).unwrap();
    let Mode::Command(args) = options.mode else {
        panic!("expected command")
    };
    assert_eq!(args[1], path);
}

#[cfg(feature = "disk")]
mod disk {
    use super::*;
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::atomic::{AtomicUsize, Ordering},
    };
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let base = std::env::temp_dir();
            for _ in 0..100 {
                let path = base.join(format!(
                    "forge-terminal-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
                match fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("owned fixture creation failed: {error}"),
                }
            }
            panic!("could not allocate owned fixture")
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }
    fn execute_at(root: &Path, command: &[&str], input: &[u8]) -> (u8, String, String) {
        let console = StreamConsole::new(Cursor::new(input.to_vec()), Vec::new(), Vec::new());
        let mut args = vec![OsString::from("--cwd"), root.as_os_str().to_owned()];
        args.extend(arguments(command));
        let code = run(args, &console);
        let (_, out, err) = console.into_parts();
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }
    #[test]
    fn session_cwd_persists_and_failed_cd_preserves_it() {
        let fixture = Fixture::new();
        fs::create_dir(fixture.path().join("child with spaces")).unwrap();
        let process_cwd = std::env::current_dir().unwrap();
        let (code, out, err) = execute_at(
            fixture.path(),
            &["--session"],
            b"disk.cd child with spaces\ndisk.pwd\ndisk.cd missing\ndisk.pwd\nexit\n",
        );
        assert_eq!(code, 1);
        assert!(err.contains("handler failed"));
        let expected = format!(
            "cwd={}",
            quote_os(
                fs::canonicalize(fixture.path().join("child with spaces"))
                    .unwrap()
                    .as_os_str()
            )
        );
        assert_eq!(out.matches(&expected).count(), 3);
        assert_eq!(std::env::current_dir().unwrap(), process_cwd);
        let (code, out, _) = execute_at(fixture.path(), &["disk.pwd"], b"");
        assert_eq!(code, 0);
        assert!(out.contains(&quote_os(
            fs::canonicalize(fixture.path()).unwrap().as_os_str()
        )));
    }
    #[test]
    fn list_order_permissions_and_escaped_names_are_native() {
        let fixture = Fixture::new();
        fs::write(fixture.path().join("z-file"), b"z").unwrap();
        fs::write(fixture.path().join("a-file"), b"a").unwrap();
        fs::write(fixture.path().join("escape\u{1b}[2J"), b"e").unwrap();
        let (code, out, err) = execute_at(fixture.path(), &["disk.list"], b"");
        assert_eq!(code, 0);
        assert!(err.is_empty());
        assert!(out.find("a-file").unwrap() < out.find("z-file").unwrap());
        assert!(out.contains("escape\\x1b[2J"));
        assert!(!out.contains('\u{1b}'));
        let (code, out, _) = execute_at(fixture.path(), &["disk.permissions", "a-file"], b"");
        assert_eq!(code, 0);
        assert!(out.contains("not effective access authorization"));
    }
    #[cfg(unix)]
    #[test]
    fn permission_session_flag_uses_selected_kind_and_argv_paths_stay_literal() {
        let fixture = Fixture::new();
        fs::write(fixture.path().join("target"), b"t").unwrap();
        std::os::unix::fs::symlink("target", fixture.path().join("link with spaces")).unwrap();
        let (code, out, err) = execute_at(
            fixture.path(),
            &["--session"],
            b"disk.permissions --no-follow link with spaces\nexit\n",
        );
        assert_eq!(code, 0);
        assert!(err.is_empty());
        assert!(out.contains("kind=Symlink"));
        fs::write(fixture.path().join("--no-follow file"), b"f").unwrap();
        let (code, out, err) = execute_at(
            fixture.path(),
            &["disk.permissions", "./--no-follow file"],
            b"",
        );
        assert_eq!(code, 0);
        assert!(err.is_empty());
        assert!(out.contains("kind=File"));
    }

    #[test]
    fn startup_conflict_and_missing_directory_are_controlled() {
        assert_eq!(execute(&["--no-disk", "--cwd", "x", "node.info"], b"").0, 2);
        let fixture = Fixture::new();
        let (code, out, err) =
            execute_at(&fixture.path().join("does-not-exist"), &["node.info"], b"");
        assert_eq!(code, 1);
        assert!(out.is_empty());
        assert!(!err.is_empty());
    }
}

#[test]
fn session_stops_after_console_diagnostic_or_response_failure() {
    use std::{cell::Cell, io};
    struct BrokenConsole {
        reads: Cell<usize>,
        fail_diagnostic: bool,
    }
    impl IoClient for BrokenConsole {
        type Request = ConsoleRequest;
        type Response = ConsoleResponse;
        type Error = io::Error;
        fn execute(&self, request: ConsoleRequest) -> io::Result<ConsoleResponse> {
            match request {
                ConsoleRequest::ReadLine => {
                    let previous = self.reads.get();
                    self.reads.set(previous + 1);
                    if previous != 0 {
                        panic!("must stop before reading a second command");
                    }
                    let command = if self.fail_diagnostic {
                        "unknown"
                    } else {
                        "node.info"
                    };
                    Ok(ConsoleResponse::Input(InputLine::Line(command.into())))
                }
                ConsoleRequest::WriteErr(_) if self.fail_diagnostic => {
                    Err(io::Error::other("stderr failed"))
                }
                ConsoleRequest::WriteOut(text) if text.starts_with("node_id=") => {
                    Err(io::Error::other("stdout failed"))
                }
                _ => Ok(ConsoleResponse::Written),
            }
        }
    }
    for fail_diagnostic in [true, false] {
        let console = BrokenConsole {
            reads: Cell::new(0),
            fail_diagnostic,
        };
        assert_eq!(run(arguments(&["--no-disk", "--session"]), &console), 1);
        assert_eq!(console.reads.get(), 1);
    }
}
