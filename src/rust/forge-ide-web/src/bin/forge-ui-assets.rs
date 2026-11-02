//! Explicit build-boundary I/O; no runtime host or application domain decisions.
use forge_bootstrap::Registry;
use forge_ide_web::generate::{
    application::{ide_document, ide_transport, IDE_THEME},
    register_assets, AssetConfig,
};
use forge_provider::{Factory, InstanceProvider, Singleton};
use std::{
    error::Error,
    fs::{self, File},
    io::Read,
    path::PathBuf,
};
fn run() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() == 1 && (args[0] == "--help" || args[0] == "-h") {
        println!("forge-ui-assets --wasm BUILT_APP.wasm --output NEW_DIRECTORY\nCreates deterministic assets; refuses an existing output directory.");
        return Ok(());
    }
    if args.len() != 4 || args[0] != "--wasm" || args[2] != "--output" {
        return Err("usage: forge-ui-assets --wasm BUILT_APP.wasm --output NEW_DIRECTORY".into());
    }
    let mut wasm = Vec::new();
    File::open(&args[1])?
        .take(8 * 1024 * 1024 + 1)
        .read_to_end(&mut wasm)?;
    let group = register_assets(
        "ui.assets",
        AssetConfig {
            document: ide_document(),
            theme_yaml: IDE_THEME.into(),
            transport: ide_transport(),
            wasm,
        },
    )?;
    let frozen = Registry::new().register(group)?.freeze()?;
    let ((), factory) = frozen.into_factories();
    let provider = Singleton::new(factory.create()?);
    let assets = provider.acquire()?;
    let output = PathBuf::from(&args[3]);
    fs::create_dir(&output)?;
    let result = (|| -> Result<(), std::io::Error> {
        for (path, bytes) in &assets.files {
            fs::write(output.join(path), bytes)?;
        }
        fs::write(output.join("manifest.json"), &assets.manifest)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&output);
    }
    result?;
    println!(
        "Generated {} assets at {}",
        assets.files.len(),
        output.display()
    );
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("forge-ui-assets: {e}");
        std::process::exit(1);
    }
}
