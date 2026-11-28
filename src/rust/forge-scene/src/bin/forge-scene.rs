//! `forge-scene render <scene.yaml> <out.rs>` writes generated Rust.
//! `forge-scene check <scene.yaml> <out.rs>` fails if the file is stale.
//!
//! A build-time tool: run it by hand (or from CI) and commit the output.

use std::{fs, process::ExitCode};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (mode, scene, out) = match args.as_slice() {
        [mode, scene, out] if mode == "render" || mode == "check" => (mode, scene, out),
        _ => {
            eprintln!("usage: forge-scene <render|check> <scene.yaml> <out.rs>");
            return ExitCode::from(2);
        }
    };
    let yaml = match fs::read_to_string(scene) {
        Ok(yaml) => yaml,
        Err(error) => {
            eprintln!("cannot read {scene}: {error}");
            return ExitCode::FAILURE;
        }
    };
    let rendered = match forge_scene::render::render(&yaml) {
        Ok(rendered) => rendered,
        Err(error) => {
            eprintln!("{scene}: {error}");
            return ExitCode::FAILURE;
        }
    };
    if mode == "check" {
        return match fs::read_to_string(out) {
            Ok(current) if current == rendered => ExitCode::SUCCESS,
            _ => {
                eprintln!("{out} is stale; run: forge-scene render {scene} {out}");
                ExitCode::FAILURE
            }
        };
    }
    match fs::write(out, rendered) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("cannot write {out}: {error}");
            ExitCode::FAILURE
        }
    }
}
