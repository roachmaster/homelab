//! Synchronous terminal I/O boundary. Requests contain already prepared text.
use forge_io_client::IoClient;
use std::{
    cell::RefCell,
    io::{self, BufRead, Write},
};

/// Maximum session input bytes, excluding a trailing LF or CRLF.
pub const MAX_LINE_BYTES: usize = 4096;

#[derive(Debug, PartialEq, Eq)]
pub enum ConsoleRequest {
    ReadLine,
    WriteOut(String),
    WriteErr(String),
}
#[derive(Debug, PartialEq, Eq)]
pub enum InputLine {
    Line(String),
    End,
    Rejected(&'static str),
}
#[derive(Debug, PartialEq, Eq)]
pub enum ConsoleResponse {
    Input(InputLine),
    Written,
}

struct Streams<R, W, E> {
    input: R,
    output: W,
    errors: E,
}

/// Owns injected streams. No global input/output, retries, threads or shell.
/// Writes complete and flush before success; an I/O error may follow a partial
/// write. A rejected long/invalid UTF-8 line is drained through its newline so
/// the next command starts at a defined boundary. Reading may block until then.
pub struct StreamConsole<R, W, E> {
    streams: RefCell<Streams<R, W, E>>,
}
impl<R, W, E> StreamConsole<R, W, E> {
    pub fn new(input: R, output: W, errors: E) -> Self {
        Self {
            streams: RefCell::new(Streams {
                input,
                output,
                errors,
            }),
        }
    }
    pub fn into_parts(self) -> (R, W, E) {
        let Streams {
            input,
            output,
            errors,
        } = self.streams.into_inner();
        (input, output, errors)
    }
}
impl<R: BufRead, W: Write, E: Write> IoClient for StreamConsole<R, W, E> {
    type Request = ConsoleRequest;
    type Response = ConsoleResponse;
    type Error = io::Error;

    fn execute(&self, request: ConsoleRequest) -> io::Result<ConsoleResponse> {
        let mut streams = self
            .streams
            .try_borrow_mut()
            .map_err(|_| io::Error::other("console already borrowed"))?;
        match request {
            ConsoleRequest::ReadLine => {
                bounded_line(&mut streams.input).map(ConsoleResponse::Input)
            }
            ConsoleRequest::WriteOut(text) => {
                streams.output.write_all(text.as_bytes())?;
                streams.output.flush()?;
                Ok(ConsoleResponse::Written)
            }
            ConsoleRequest::WriteErr(text) => {
                streams.errors.write_all(text.as_bytes())?;
                streams.errors.flush()?;
                Ok(ConsoleResponse::Written)
            }
        }
    }
}

fn bounded_line(input: &mut impl BufRead) -> io::Result<InputLine> {
    // Keep at most limit + CR + LF. Oversized content is consumed, not allocated.
    let mut line = Vec::with_capacity(128);
    let mut overflow = false;
    let mut read_any = false;
    loop {
        let chunk = input.fill_buf()?;
        if chunk.is_empty() {
            break;
        }
        read_any = true;
        let newline = chunk.iter().position(|byte| *byte == b'\n');
        let count = newline.map_or(chunk.len(), |position| position + 1);
        let retain = (MAX_LINE_BYTES + 2 - line.len()).min(count);
        line.extend_from_slice(&chunk[..retain]);
        overflow |= retain < count;
        input.consume(count);
        if newline.is_some() {
            break;
        }
    }
    if !read_any {
        return Ok(InputLine::End);
    }
    if line.last() == Some(&b'\n') {
        line.pop();
    }
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    if overflow || line.len() > MAX_LINE_BYTES {
        return Ok(InputLine::Rejected("session line exceeds 4096 bytes"));
    }
    Ok(match String::from_utf8(line) {
        Ok(text) => InputLine::Line(text),
        Err(_) => {
            InputLine::Rejected("session input must be UTF-8; use argv for native non-UTF-8 paths")
        }
    })
}
