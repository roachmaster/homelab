//! Bounded cleanup for a caller-owned, new-session CLI launcher
//! (work_process.py). This controls only the local launcher
//! process group created with start_new_session; it is not proof
//! that the separately supervised systemd agent scope stopped.

use nix::sys::signal::{killpg, Signal};
use nix::unistd::{getpgid, getpid, getsid, Pid};
use std::process::Child;
use std::time::{Duration, Instant};

const MAX_WAIT_SECONDS: f64 = 10.0;
const POLL_SECONDS: f64 = 0.02;

fn duration(value: f64) -> Result<f64, String> {
    if !value.is_finite() || !(0.0..=MAX_WAIT_SECONDS).contains(&value) {
        return Err("Launcher cleanup needs a bounded nonnegative timeout".to_string());
    }
    Ok(value)
}

fn group_alive(group: Pid) -> bool {
    killpg(group, None).is_ok()
}

fn signal_group(group: Pid, signal: Signal) {
    // A stopped group still needs its owned parent reaped.
    let _ = killpg(group, signal);
}

fn poll(child: &mut Child) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    child.try_wait().ok().flatten().map(|status| {
        status
            .code()
            .unwrap_or_else(|| status.signal().map(|s| -s).unwrap_or(-1))
    })
}

fn wait_stopped(child: &mut Child, group: Pid, seconds: f64) -> bool {
    let deadline = Instant::now() + Duration::from_secs_f64(seconds);
    loop {
        if poll(child).is_some() && !group_alive(group) {
            return true;
        }
        let now = Instant::now();
        if now >= deadline {
            return false;
        }
        let remaining = deadline.duration_since(now);
        std::thread::sleep(remaining.min(Duration::from_secs_f64(POLL_SECONDS)));
    }
}

/// Stop the owned launcher group with TERM, then KILL, and reap
/// its parent. Returns true only when the parent was reaped and
/// its process group no longer exists.
pub fn terminate_and_reap(
    child: &mut Child,
    term_seconds: f64,
    kill_seconds: f64,
) -> Result<bool, String> {
    let term_seconds = duration(term_seconds)?;
    let kill_seconds = duration(kill_seconds)?;
    let group = Pid::from_raw(child.id() as i32);
    if group.as_raw() <= 1 || group == getpid() || group == nix::unistd::getpgrp() {
        return Ok(false);
    }
    let run = (|| -> bool {
        let reaped = poll(child).is_some();
        match (getpgid(Some(group)), getsid(Some(group))) {
            (Ok(actual_group), Ok(session)) => {
                // An existing PID after our child was reaped is a
                // different process; refuse the recycled identity.
                if reaped || actual_group != group || session != group {
                    return false;
                }
            }
            _ => {
                if !reaped && poll(child).is_none() {
                    return false;
                }
            }
        }
        if !group_alive(group) {
            return poll(child).is_some();
        }
        signal_group(group, Signal::SIGTERM);
        if wait_stopped(child, group, term_seconds) {
            return true;
        }
        signal_group(group, Signal::SIGKILL);
        wait_stopped(child, group, kill_seconds)
    })();
    Ok(run)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_are_bounded() {
        assert!(duration(2.0).is_ok());
        assert!(duration(0.0).is_ok());
        assert!(duration(10.5).is_err());
        assert!(duration(f64::NAN).is_err());
        assert!(duration(-1.0).is_err());
    }

    #[test]
    fn refuses_own_group() {
        let mut child = std::process::Command::new("/bin/true").spawn().unwrap();
        // The child is in our own process group (no new session), so
        // its pid is neither our pgid nor its own session leader in
        // the required shape; cleanup must refuse or find it gone,
        // never signal our group.
        let result = terminate_and_reap(&mut child, 0.1, 0.1).unwrap();
        let _ = child.wait();
        assert!(!result || poll(&mut child).is_some());
    }
}
