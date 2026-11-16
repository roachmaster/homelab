#[path = "src/design/mod.rs"]
mod design;

use design::DesignPage;
use std::{env, fs, io, path::PathBuf};

fn main() -> io::Result<()> {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/design");
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo build output directory"));
    for (name, page) in [
        ("design.html", DesignPage::Overview),
        ("chat-history.html", DesignPage::ChatHistory),
        ("chat-dispatch.html", DesignPage::ChatDispatch),
    ] {
        fs::write(output.join(name), page.render())?;
    }
    Ok(())
}
