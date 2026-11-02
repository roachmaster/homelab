use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
fn main() -> std::process::ExitCode {
    let stop = Arc::new(AtomicBool::new(false));
    let signal = Arc::clone(&stop);
    // Exactly one process-owned signal listener, not a thread per request.
    if ctrlc::set_handler(move || {
        signal.store(true, Ordering::SeqCst);
        // Best-effort acknowledgement from ctrlc's dedicated thread, not a raw
        // signal handler. Admission is already stopped even if stderr fails.
        use std::io::Write;
        let _ = std::io::stderr().write_all(b"STOP REQUESTED\n");
    })
    .is_err()
    {
        eprintln!("cannot install process shutdown handler");
        return std::process::ExitCode::FAILURE;
    }
    match forge_agent::run(std::env::args_os().skip(1), stop) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}
