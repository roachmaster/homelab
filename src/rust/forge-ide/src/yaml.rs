//! YAML 1.1 output boundary. Internal reports are typed, not generic YAML values.
use crate::model::ProjectReport;
use std::{
    error::Error,
    fmt,
    io::{self, Write},
};
#[derive(Debug)]
pub enum YamlError {
    InvalidLimit,
    Encode(serde_yaml_ng::Error),
}
impl fmt::Display for YamlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLimit => f.write_str("output limit must be between 1 byte and 64 MiB"),
            Self::Encode(e) => e.fmt(f),
        }
    }
}
impl Error for YamlError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Encode(e) => Some(e),
            _ => None,
        }
    }
}
struct Bounded {
    bytes: Vec<u8>,
    limit: usize,
}
impl Write for Bounded {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.limit - self.bytes.len() {
            return Err(io::Error::other("YAML output byte limit exceeded"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
/// Serialize privately before exposing any bytes. This caps emitted bytes, not
/// all serializer-internal allocation. The report itself must also be bounded.
pub fn to_yaml(report: &ProjectReport, max_bytes: usize) -> Result<Vec<u8>, YamlError> {
    if max_bytes == 0 || max_bytes > 64 * 1024 * 1024 {
        return Err(YamlError::InvalidLimit);
    }
    let mut output = Bounded {
        bytes: Vec::new(),
        limit: max_bytes,
    };
    serde_yaml_ng::to_writer(&mut output, report).map_err(YamlError::Encode)?;
    Ok(output.bytes)
}
