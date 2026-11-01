//! Native boundary operations only: no schema or business payload assembly.
use forge_io_client::IoClient;
use std::{
    error::Error,
    fmt,
    fs::File,
    io::{self, Read, Write},
    path::PathBuf,
};

#[derive(Debug)]
pub(super) struct ReadSource {
    pub path: PathBuf,
    pub max_bytes: usize,
}
#[derive(Debug)]
pub(super) enum SourceError {
    Io(io::Error),
    TooLarge,
    InvalidUtf8,
    NotRegularFile,
}
impl fmt::Display for SourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "template file: {e}"),
            Self::TooLarge => f.write_str("template file exceeds the source-byte limit"),
            Self::InvalidUtf8 => f.write_str("template file must contain UTF-8 text"),
            Self::NotRegularFile => f.write_str("template source must be a regular file"),
        }
    }
}
impl Error for SourceError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}
impl From<io::Error> for SourceError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

pub(super) struct NativeSourceClient;
impl IoClient for NativeSourceClient {
    type Request = ReadSource;
    type Response = String;
    type Error = SourceError;
    fn execute(&self, request: ReadSource) -> Result<String, SourceError> {
        // Reject ordinary special-file paths before opening (e.g. an idle FIFO).
        // This follows symlinks and is NOT a race-safe filesystem sandbox.
        if !std::fs::metadata(&request.path)?.is_file() {
            return Err(SourceError::NotRegularFile);
        }
        let mut file = File::open(request.path)?;
        if !file.metadata()?.is_file() {
            return Err(SourceError::NotRegularFile);
        }
        read_bounded(&mut file, request.max_bytes)
    }
}
fn read_bounded(reader: &mut impl Read, maximum: usize) -> Result<String, SourceError> {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        // At most one sentinel byte beyond the limit, never file-size allocation.
        let amount = buffer
            .len()
            .min(maximum.saturating_sub(bytes.len()).saturating_add(1));
        let count = match reader.read(&mut buffer[..amount]) {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if count == 0 {
            break;
        }
        if count > maximum.saturating_sub(bytes.len()) {
            return Err(SourceError::TooLarge);
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    String::from_utf8(bytes).map_err(|_| SourceError::InvalidUtf8)
}

pub(super) enum ConsoleRequest {
    Output(String),
    Diagnostic(String),
}
pub(super) struct NativeConsoleClient;
impl IoClient for NativeConsoleClient {
    type Request = ConsoleRequest;
    type Response = ();
    type Error = io::Error;
    fn execute(&self, request: ConsoleRequest) -> io::Result<()> {
        match request {
            ConsoleRequest::Output(text) => {
                let mut out = io::stdout().lock();
                out.write_all(text.as_bytes())?;
                out.flush()
            }
            ConsoleRequest::Diagnostic(text) => {
                let mut out = io::stderr().lock();
                out.write_all(text.as_bytes())?;
                out.flush()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_reads_reject_before_storing_extra_bytes_and_preserve_utf8() {
        assert_eq!(read_bounded(&mut "é".as_bytes(), 2).unwrap(), "é");
        assert!(matches!(
            read_bounded(&mut "é".as_bytes(), 1),
            Err(SourceError::TooLarge)
        ));
        assert!(matches!(
            read_bounded(&mut &[0xff][..], 10),
            Err(SourceError::InvalidUtf8)
        ));
        assert_eq!(read_bounded(&mut &b""[..], 0).unwrap(), "");
        assert!(matches!(
            read_bounded(&mut &b"x"[..], 0),
            Err(SourceError::TooLarge)
        ));
        struct Infinite {
            bytes: usize,
        }
        impl Read for Infinite {
            fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
                out.fill(b'x');
                self.bytes += out.len();
                Ok(out.len())
            }
        }
        let mut stream = Infinite { bytes: 0 };
        assert!(matches!(
            read_bounded(&mut stream, 10000),
            Err(SourceError::TooLarge)
        ));
        assert_eq!(stream.bytes, 10001);
    }
}
