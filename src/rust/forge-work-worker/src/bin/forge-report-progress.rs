use std::path::Path;
use std::process::ExitCode;
fn main() -> ExitCode {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if let [kind, summary] = args.as_slice() {
        if forge_work_worker::progress::write_report(
            Path::new("/run/forge-coding-attempt/progress"),
            kind,
            summary,
        )
        .is_ok()
        {
            return ExitCode::SUCCESS;
        }
    }
    eprintln!("Expected a bounded public plan, followed by progress summaries.");
    ExitCode::from(2)
}
