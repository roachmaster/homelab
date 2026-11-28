fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 || args[1] != "--config" {
        eprintln!("usage: forge-pg-proxy --config /absolute/config.json");
        std::process::exit(2)
    }
    if let Err(error) = forge_postgres::proxy::service::run(std::path::Path::new(&args[2])) {
        eprintln!("forge-pg-proxy: {error}");
        std::process::exit(1)
    }
}
