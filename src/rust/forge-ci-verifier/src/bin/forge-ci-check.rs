fn main() {
    match forge_ci_verifier::public_main() {
        Ok(code) => std::process::exit(code),
        Err(_) => {
            eprintln!("Trusted native CI supervision failed.");
            std::process::exit(1)
        }
    }
}
