//! Raw worker-only ABI. All code is memory-safe Rust: exported pointer integers
//! refer only to owned input/output Vec buffers. We never read a caller-provided
//! pointer or construct a slice from one. The lint allowance is required only
//! for exported linker names, which Rust classifies as unsafe attributes.
//!
//! Host order: reserve_input(len), copy exactly len bytes through memory.buffer,
//! dispatch_input(), copy output_ptr/output_len bytes, release_output(). Refresh
//! JavaScript memory views after every call; reentry returns 0, never panics.
//! ABI 2 requires report-scoped local actions and the workbench view bindings.
//! Deploy the generated script/HTML/CSS and WASM as one verified asset bundle;
//! this local ABI version is separate from the unchanged HTTP report protocol.
use forge_ide_ui::{protocol::MAX_MESSAGE_BYTES, UiModel};
use std::{
    cell::RefCell,
    io::{self, Write},
};

struct State {
    model: UiModel,
    input: Vec<u8>,
    output: Vec<u8>,
}
thread_local! {
    static STATE: RefCell<State> = RefCell::new(State {
        model: UiModel::default(), input: Vec::new(), output: Vec::new(),
    });
}
struct BoundedBytes(Vec<u8>);
impl Write for BoundedBytes {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_MESSAGE_BYTES.saturating_sub(self.0.len()) {
            return Err(io::Error::other("output limit"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn abi_version() -> u32 {
    2
}

/// Returns zero on invalid size/reentry. Reserving invalidates previous input.
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn reserve_input(length: u32) -> u32 {
    STATE.with(|cell| {
        let Ok(mut s) = cell.try_borrow_mut() else {
            return 0;
        };
        s.input.fill(0);
        s.input.clear();
        if length == 0 || length as usize > MAX_MESSAGE_BYTES {
            return 0;
        }
        s.input.resize(length as usize, 0);
        s.input.as_mut_ptr() as usize as u32
    })
}

/// Processes one owned bounded message. Returns one if a JSON output is ready.
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn dispatch_input() -> u32 {
    STATE.with(|cell| {
        let Ok(mut s) = cell.try_borrow_mut() else {
            return 0;
        };
        s.output.fill(0);
        s.output.clear();
        let input = serde_json::from_slice::<forge_ide_ui::protocol::Input>(&s.input);
        s.input.fill(0);
        s.input.clear();
        let output = match input {
            Ok(input) => match s.model.dispatch(input) {
                Ok(output) => output,
                Err(error) => s.model.output(Some(error.to_string())),
            },
            Err(_) => s.model.output(Some("invalid or oversized UI input".into())),
        };
        let mut bytes = BoundedBytes(Vec::new());
        if serde_json::to_writer(&mut bytes, &output).is_err() {
            // The typed model remains bounded. No partially serialized effect is
            // exposed; callers must stop this instance after ABI failure.
            return 0;
        }
        s.output = bytes.0;
        1
    })
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn output_ptr() -> u32 {
    STATE.with(|cell| {
        cell.try_borrow()
            .map(|s| s.output.as_ptr() as usize as u32)
            .unwrap_or(0)
    })
}
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn output_len() -> u32 {
    STATE.with(|cell| {
        cell.try_borrow()
            .map(|s| s.output.len() as u32)
            .unwrap_or(0)
    })
}
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn release_output() {
    STATE.with(|cell| {
        if let Ok(mut s) = cell.try_borrow_mut() {
            s.output.fill(0);
            s.output.clear();
        }
    });
}
