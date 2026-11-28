fn main() {
    if forge_ci_verifier::private_child_main().is_err() {
        eprintln!("Private CI child setup failed.");
        std::process::exit(1)
    }
}
