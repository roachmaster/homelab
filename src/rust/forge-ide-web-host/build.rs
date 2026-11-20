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
    println!("cargo:rerun-if-changed=../../../docs/design/implementation-status.md");
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo build output directory"));
    for (name, page) in [
        ("home.html", DesignPage::Home),
        ("design.html", DesignPage::Overview),
        ("implementation.html", DesignPage::Implementation),
        ("characters.html", DesignPage::Characters),
        ("previous-chats.html", DesignPage::PreviousChats),
        ("wireframes.html", DesignPage::Wireframes),
        ("workspace.html", DesignPage::Workspace),
        ("forge-providers.html", DesignPage::ForgeProviders),
        ("chat-history.html", DesignPage::ChatHistory),
        ("chat-dispatch.html", DesignPage::ChatDispatch),
        ("mail.html", DesignPage::Mail),
        ("game-studio.html", DesignPage::GameStudio),
        ("game-physics.html", DesignPage::GamePhysics),
        ("game-asset-pipeline.html", DesignPage::GameAssetPipeline),
        ("forge-workers.html", DesignPage::ForgeWorkers),
        (
            "forge-workers-playtest.html",
            DesignPage::ForgeWorkersPlaytest,
        ),
        ("forge-workers-studio.html", DesignPage::ForgeWorkersStudio),
        ("coding-agents.html", DesignPage::CodingAgents),
        ("agent-usage.html", DesignPage::AgentUsage),
        ("agent-feedback-loop.html", DesignPage::AgentFeedback),
        ("autonomous-agents.html", DesignPage::AgentAutonomy),
        ("playtest.html", DesignPage::Playtest),
        ("studio.html", DesignPage::Studio),
    ] {
        fs::write(output.join(name), page.render())?;
    }
    fs::write(output.join("home.css"), design::home_stylesheet())?;
    fs::write(output.join("docs.css"), design::docs_stylesheet())?;
    fs::write(
        output.join("characters.css"),
        design::characters_stylesheet(),
    )?;
    fs::write(output.join("design-index.css"), design::index_stylesheet())?;
    fs::write(
        output.join("game-studio.css"),
        design::game_engine::stylesheet(),
    )?;
    fs::write(output.join("playtest.css"), design::playtest::stylesheet())?;
    fs::write(output.join("studio.css"), design::studio::stylesheet())?;
    fs::write(output.join("studio.js"), design::studio::MAIN)?;
    fs::write(output.join("studio-worker.js"), design::studio::WORKER)?;
    fs::write(
        output.join("playtest-assets-worker.js"),
        design::playtest_asset_worker::render(),
    )?;
    fs::write(
        output.join("playtest.js"),
        design::playtest_browser::javascript(),
    )
}
