//! Finite adapter codecs and presentation, not a job engine or scheduler.
use forge_local::jobs::{CancelAck, IntentKey, JobCommand, JobId, JobOutput, JobSnapshot};
use forge_local::{Command, CommandKind};
use std::ffi::OsString;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) fn is_job(kind: CommandKind) -> bool {
    matches!(
        kind,
        CommandKind::JobSubmit
            | CommandKind::JobInspect
            | CommandKind::JobList
            | CommandKind::JobStep
            | CommandKind::JobCancel
    )
}
pub(crate) fn decode(kind: CommandKind, args: &[OsString]) -> Result<Command, String> {
    let text: Vec<&str> = args
        .iter()
        .map(|a| {
            a.to_str()
                .ok_or_else(|| "job arguments must be UTF-8".to_owned())
        })
        .collect::<Result<_, _>>()?;
    let command = match kind {
        CommandKind::JobSubmit => {
            if text.len() != 2 {
                return Err("jobs.submit needs exactly KEY TOTAL".into());
            }
            if text[0].starts_with('-') {
                return Err("job key must not look like an option".into());
            }
            let key = IntentKey::new(text[0].to_owned()).map_err(|_| "invalid intent key")?;
            if text[1].is_empty() || !text[1].bytes().all(|b| b.is_ascii_digit()) {
                return Err("TOTAL must be unsigned decimal u64".into());
            }
            let total = text[1].parse::<u64>().map_err(|_| "TOTAL exceeds u64")?;
            JobCommand::Submit { key, total }
        }
        CommandKind::JobList => {
            if !text.is_empty() {
                return Err("jobs.list takes no arguments".into());
            }
            JobCommand::List
        }
        CommandKind::JobInspect | CommandKind::JobStep | CommandKind::JobCancel => {
            if text.len() != 1 {
                return Err("job operation needs exactly one ID".into());
            }
            let id = text[0].parse::<JobId>().map_err(|_| "invalid job ID")?;
            match kind {
                CommandKind::JobInspect => JobCommand::Inspect { id },
                CommandKind::JobStep => JobCommand::Step { id },
                _ => JobCommand::Cancel { id },
            }
        }
        _ => return Err("not a job operation".into()),
    };
    Ok(Command::Jobs(command))
}
fn snapshot(value: JobSnapshot) -> String {
    format!("job_id={}\nrevision={}\nstate={:?}\ncompleted={}\ntotal={}\ncancellation={:?}\nresult={:?}\nfailure={:?}\n", value.id, value.revision, value.state, value.completed, value.total, value.cancellation, value.result, value.failure)
}
pub(crate) fn format_output(value: JobOutput) -> String {
    match value {
        JobOutput::Submitted(receipt) => format!("acceptance=volatile\njob_id={}\nacceptance_revision={}\nterminal_retention_ms={}\nscope=this application lifetime\n", receipt.id, receipt.revision, receipt.terminal_retention.as_millis()),
        JobOutput::Snapshot(value) => snapshot(value),
        JobOutput::Listed(values) => {
            let mut out = format!("count={}\n", values.len());
            for value in values { out.push_str(&snapshot(value)); }
            out
        }
        JobOutput::Cancelled(ack) => {
            let (kind, value) = match ack {
                CancelAck::CancelledBeforeStart(value) => ("cancelled_before_start", value),
                CancelAck::Requested(value) => ("requested_not_confirmed", value),
                CancelAck::AlreadyTerminal(value) => ("already_terminal", value),
            };
            format!("cancellation_ack={kind}\n{}", snapshot(value))
        }
    }
}
static SEQUENCE: AtomicU64 = AtomicU64::new(0);
fn base_parts(pid: u32, nanos: u128, sequence: u64) -> Result<String, String> {
    let nanos = u64::try_from(nanos).map_err(|_| "startup timestamp exceeds u64 nanoseconds")?;
    Ok(format!("{pid:x}.{nanos:x}.{sequence:x}"))
}
pub(crate) fn fresh_base() -> Result<String, String> {
    let sequence = SEQUENCE
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
        .map_err(|_| "startup sequence exhausted")?;
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "startup clock predates Unix epoch")?;
    base_parts(std::process::id(), time.as_nanos(), sequence)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bases_are_bounded_and_distinct() {
        assert_ne!(fresh_base().unwrap(), fresh_base().unwrap());
        let max = base_parts(u32::MAX, u128::from(u64::MAX), u64::MAX).unwrap();
        assert_eq!(max.len(), 42);
        assert!(max.is_ascii());
        assert!(base_parts(1, u128::from(u64::MAX) + 1, 1).is_err());
    }
}
