#![cfg(feature = "build-assets")]
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "forge-ui-generator-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
fn actual_generator_refuses_overwrite_and_invalid_wasm_without_output() {
    let f = Fixture::new();
    let wasm = f.0.join("app.wasm");
    let out = f.0.join("assets");
    fs::write(&wasm, [0, 97, 115, 109, 1, 0, 0, 0]).unwrap();
    let invoke = || {
        Command::new(env!("CARGO_BIN_EXE_forge-ui-assets"))
            .arg("--wasm")
            .arg(&wasm)
            .arg("--output")
            .arg(&out)
            .output()
            .unwrap()
    };
    assert!(invoke().status.success());
    let before = fs::read(out.join("manifest.json")).unwrap();
    assert!(!invoke().status.success());
    assert_eq!(before, fs::read(out.join("manifest.json")).unwrap());
    fs::remove_dir_all(&out).unwrap();
    fs::write(&wasm, b"not wasm").unwrap();
    assert!(!invoke().status.success());
    assert!(!out.exists());
}
