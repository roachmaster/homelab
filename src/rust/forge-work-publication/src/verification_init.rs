//! Pure launcher recipe for a verifier's private PID namespace.
//! Port of work_verification_init.py.
//!
//! PID 1 must reap orphaned descendants while its owned verifier is
//! still alive. The fixed script runs after the supervisor's
//! UID/capability drop and forwards termination to its owned child
//! session. It never selects a command from logs.

/// The fixed PID-1 script, byte-identical to the Python constant
/// (leading and trailing newlines included).
pub const VERIFICATION_INIT: &str = r"
import os,signal,subprocess,sys,time
child = None
received = 0

def forward(signum, _frame):
 global received
 received = received or signum
 if child is not None:
  try: os.killpg(child.pid, signum)
  except ProcessLookupError: pass

for signum in (signal.SIGTERM,signal.SIGINT,signal.SIGHUP):
 signal.signal(signum,forward)
try:
 child = subprocess.Popen(sys.argv[1:],start_new_session=True)
except OSError:
 sys.exit(127)
if received:
 forward(received,None)
main_status = None
cleanup_at = None
while True:
 any_children = True
 while True:
  try: pid,status = os.waitpid(-1,os.WNOHANG)
  except ChildProcessError:
   any_children = False
   break
  if pid == 0:
   break
  if pid == child.pid:
   main_status = os.waitstatus_to_exitcode(status)
   child.returncode = main_status
 if main_status is not None:
  if not any_children:
   break
  if cleanup_at is None:
   cleanup_at = time.monotonic()
   try: os.killpg(child.pid,signal.SIGTERM)
   except ProcessLookupError: pass
  age = time.monotonic()-cleanup_at
  if age >= .2:
   try: os.killpg(child.pid,signal.SIGKILL)
   except ProcessLookupError: pass
  if age >= 2:
   sys.exit(125)
 time.sleep(.005)
if received:
 sys.exit(128+received)
sys.exit(128-main_status if main_status < 0 else main_status)
";

/// Wrap only an operator-selected argv, preserving its exit status.
/// The command must be a non-empty sequence of non-empty strings
/// without NUL bytes.
pub fn verification_argv(command: &[String]) -> Result<Vec<String>, String> {
    if command.is_empty()
        || command
            .iter()
            .any(|part| part.is_empty() || part.contains('\0'))
    {
        return Err("Verification needs a fixed command argv".to_string());
    }
    let mut argv = vec![
        "/usr/bin/python3".to_string(),
        "-B".to_string(),
        "-c".to_string(),
        VERIFICATION_INIT.to_string(),
    ];
    argv.extend(command.iter().cloned());
    Ok(argv)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv_wraps_only_fixed_commands() {
        let argv = verification_argv(&["/usr/bin/make".to_string(), "check".to_string()]).unwrap();
        assert_eq!(argv[0], "/usr/bin/python3");
        assert_eq!(argv[3], VERIFICATION_INIT);
        assert_eq!(argv[4], "/usr/bin/make");
        assert!(verification_argv(&[]).is_err());
        assert!(verification_argv(&["".to_string()]).is_err());
        assert!(verification_argv(&["a\0b".to_string()]).is_err());
        assert!(VERIFICATION_INIT.starts_with('\n') && VERIFICATION_INIT.ends_with('\n'));
    }
}
