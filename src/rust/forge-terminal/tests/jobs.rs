#![cfg(feature = "jobs")]
use forge_io_client::IoClient;
use forge_terminal::{
    console::{ConsoleRequest, ConsoleResponse, InputLine, StreamConsole},
    run,
};
use std::{
    cell::RefCell,
    collections::VecDeque,
    ffi::OsString,
    io::{self, Cursor},
};

struct Script {
    lines: RefCell<VecDeque<String>>,
    out: RefCell<String>,
    err: RefCell<String>,
    id: RefCell<Option<String>>,
}
impl Script {
    fn new(lines: &[&str]) -> Self {
        Self {
            lines: RefCell::new(lines.iter().map(|s| (*s).into()).collect()),
            out: RefCell::new(String::new()),
            err: RefCell::new(String::new()),
            id: RefCell::new(None),
        }
    }
}
impl IoClient for Script {
    type Request = ConsoleRequest;
    type Response = ConsoleResponse;
    type Error = io::Error;
    fn execute(&self, request: ConsoleRequest) -> io::Result<ConsoleResponse> {
        Ok(match request {
            ConsoleRequest::ReadLine => {
                ConsoleResponse::Input(match self.lines.borrow_mut().pop_front() {
                    Some(line) => InputLine::Line(
                        line.replace("{id}", self.id.borrow().as_deref().unwrap_or("missing")),
                    ),
                    None => InputLine::End,
                })
            }
            ConsoleRequest::WriteOut(text) => {
                for line in text.lines() {
                    if let Some(id) = line.strip_prefix("job_id=") {
                        *self.id.borrow_mut() = Some(id.into());
                    }
                }
                self.out.borrow_mut().push_str(&text);
                ConsoleResponse::Written
            }
            ConsoleRequest::WriteErr(text) => {
                self.err.borrow_mut().push_str(&text);
                ConsoleResponse::Written
            }
        })
    }
}
fn args(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

#[test]
fn session_admission_observation_and_confirmed_cancel() {
    let console = Script::new(&[
        "jobs.submit first 100",
        "jobs.list",
        "jobs.inspect {id}",
        "jobs.cancel {id}",
        "jobs.inspect {id}",
        "exit",
    ]);
    assert_eq!(run(args(&["--no-disk", "--session"]), &console), 0);
    let out = console.out.borrow();
    assert!(out.contains("acceptance=volatile"));
    assert!(out.contains("state=Queued"));
    assert!(out.contains("count=1"));
    assert!(out.contains("cancellation_ack=cancelled_before_start"));
    assert!(out.contains("state=Cancelled"));
    assert!(console.err.borrow().is_empty());
}
#[test]
fn session_step_and_duplicate_conflict_are_explicit() {
    let console = Script::new(&[
        "jobs.submit same 1",
        "jobs.submit same 1",
        "jobs.submit same 2",
        "jobs.step {id}",
        "jobs.inspect {id}",
        "jobs.list",
        "exit",
    ]);
    assert_eq!(run(args(&["--no-disk", "--session"]), &console), 1);
    let out = console.out.borrow();
    assert_eq!(out.matches("acceptance=volatile").count(), 2);
    assert!(out.contains("count=1"));
    assert!(out.contains("state=Succeeded"));
    assert!(!console.err.borrow().is_empty());
}
#[test]
fn malformed_inputs_and_help_create_no_jobs() {
    let console = Script::new(&[
        "jobs.submit --bad 1",
        "jobs.submit key -1",
        "jobs.submit key +1",
        "jobs.submit key 18446744073709551616",
        "jobs.submit key 1 extra",
        "jobs.list extra",
        "jobs.inspect wrong",
        "jobs.submit --help",
        "help jobs.step",
        "jobs.list",
        "exit",
    ]);
    assert_eq!(run(args(&["--no-disk", "--session"]), &console), 2);
    let out = console.out.borrow();
    assert!(out.contains("count=0"));
    assert!(!out.contains("acceptance=volatile"));
    assert!(out.contains("Command: jobs.submit"));
}
#[test]
fn disabled_jobs_are_not_advertised_and_cannot_execute() {
    let console = StreamConsole::new(Cursor::new(Vec::<u8>::new()), Vec::new(), Vec::new());
    assert_eq!(
        run(args(&["--no-disk", "--no-jobs", "--help"]), &console),
        0
    );
    let (_, out, _) = console.into_parts();
    assert!(!String::from_utf8(out).unwrap().contains("jobs.submit"));
    let console = StreamConsole::new(Cursor::new(Vec::<u8>::new()), Vec::new(), Vec::new());
    assert_eq!(run(args(&["--no-jobs", "jobs.list"]), &console), 2);
}
#[cfg(unix)]
#[test]
fn non_utf8_job_arguments_are_rejected() {
    use std::os::unix::ffi::OsStringExt;
    let console = StreamConsole::new(Cursor::new(Vec::<u8>::new()), Vec::new(), Vec::new());
    assert_eq!(
        run(
            vec![
                "jobs.submit".into(),
                OsString::from_vec(vec![255]),
                "1".into()
            ],
            &console
        ),
        2
    );
}

#[test]
fn running_cancellation_is_requested_then_confirmed_on_step() {
    let console = Script::new(&[
        "jobs.submit running 100",
        "jobs.step {id}",
        "jobs.cancel {id}",
        "jobs.inspect {id}",
        "jobs.step {id}",
        "jobs.inspect {id}",
        "exit",
    ]);
    assert_eq!(run(args(&["--no-disk", "--session"]), &console), 0);
    let out = console.out.borrow();
    assert!(out.contains("state=Running"));
    assert!(out.contains("cancellation_ack=requested_not_confirmed"));
    assert!(out.contains("state=Cancelled"));
    assert!(!out.contains("state=Succeeded"));
}
