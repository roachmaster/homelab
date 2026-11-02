//! Explicit agent/client application composition. Remote capability contracts
//! live in their optional libraries, not in this application package.
mod cli;
mod config;
mod dispatch;
mod host;
pub use cli::run;
use std::{error::Error, fmt, io};

#[derive(Debug)]
pub enum AppError {
    Usage(&'static str),
    Configuration(&'static str),
    Input(io::ErrorKind),
    Assembly(Box<dyn Error>),
    Remote(forge_remote::RemoteError),
    Output(io::Error),
}
impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(s) => write!(f, "usage: {s}"),
            Self::Configuration(s) => write!(f, "configuration: {s}"),
            Self::Input(k) => write!(
                f,
                "cannot read bounded configuration/credential input: {k:?}"
            ),
            Self::Assembly(e) => write!(f, "assembly failed: {e}"),
            Self::Remote(e) => e.fmt(f),
            Self::Output(_) => f.write_str("console output failed"),
        }
    }
}
impl Error for AppError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Assembly(e) => Some(e.as_ref()),
            Self::Remote(e) => Some(e),
            Self::Output(e) => Some(e),
            _ => None,
        }
    }
}
impl From<forge_remote::RemoteError> for AppError {
    fn from(e: forge_remote::RemoteError) -> Self {
        Self::Remote(e)
    }
}
pub(crate) fn assembly(e: impl Error + 'static) -> AppError {
    AppError::Assembly(Box::new(e))
}
