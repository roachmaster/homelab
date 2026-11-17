#[path = "src/design/mod.rs"]
mod design;

use design::DesignPage;
use std::{env, fs, io, path::PathBuf};

fn main() -> io::Result<()> {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/design");
    println!(
        "cargo:rerun-if-changed={}",
        design::game_engine::SOURCE_PATH
    );
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo build output directory"));
    for (name, page) in [
        ("design.html", DesignPage::Overview),
        ("chat-history.html", DesignPage::ChatHistory),
        ("chat-dispatch.html", DesignPage::ChatDispatch),
        ("mail.html", DesignPage::Mail),
        ("game-studio.html", DesignPage::GameStudio),
        ("playtest.html", DesignPage::Playtest),
    ] {
        fs::write(output.join(name), page.render())?;
    }
    fs::write(
        output.join("game-studio.css"),
        design::game_engine::stylesheet(),
    )?;
    fs::write(output.join("playtest.css"), design::playtest::stylesheet())?;
    fs::write(
        output.join("playtest.js"),
        design::playtest_browser::javascript(),
    )
}
