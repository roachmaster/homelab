use std::fmt;

/// The adapter's error categories, mirroring the Python WorkerDB's translated exceptions so migrated callers make the same decisions.
#[derive(Debug)]
pub enum AdapterError {
    /// Database is unavailable.
    Unavailable(String),
    /// Access is denied.
    AccessDenied(String),
    /// Lease has expired.
    LeaseExpired(String),
    /// Outcome conflict.
    OutcomeConflict(String),
    /// Result mismatch.
    ResultMismatch(String),
    /// Adapter protocol error.
    Protocol(String),
    /// Database error.
    Sql {
        state: Option<String>,
        message: String,
    },
}

impl fmt::Display for AdapterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AdapterError::Unavailable(m) => write!(f, "database unavailable: {}", m),
            AdapterError::AccessDenied(m) => write!(f, "access denied: {}", m),
            AdapterError::LeaseExpired(m) => write!(f, "lease expired: {}", m),
            AdapterError::OutcomeConflict(m) => write!(f, "outcome conflict: {}", m),
            AdapterError::ResultMismatch(m) => write!(f, "result mismatch: {}", m),
            AdapterError::Protocol(m) => write!(f, "adapter protocol error: {}", m),
            AdapterError::Sql { state, message } => write!(
                f,
                "database error [{}]: {}",
                state.as_deref().unwrap_or("-----"),
                message
            ),
        }
    }
}

impl std::error::Error for AdapterError {}

/// Categorizes a database error based on its SQLSTATE and message.
///
/// # Arguments
///
/// * `state` - The SQLSTATE code of the error.
/// * `message` - The error message.
///
/// # Returns
///
/// An `AdapterError` variant corresponding to the error category.
pub fn categorize(state: Option<&str>, message: &str) -> AdapterError {
    if message.to_lowercase().contains("result_mismatch") {
        return AdapterError::ResultMismatch(message.to_string());
    }
    if message.to_lowercase().contains("lease")
        && (message.to_lowercase().contains("expired") || message.to_lowercase().contains("lost"))
    {
        return AdapterError::LeaseExpired(message.to_string());
    }
    match state {
        Some("42501") => AdapterError::AccessDenied(message.to_string()),
        Some("40001") | Some("40P01") | Some("23505") => {
            AdapterError::OutcomeConflict(message.to_string())
        }
        _ => AdapterError::Sql {
            state: state.map(|s| s.to_string()),
            message: message.to_string(),
        },
    }
}

impl From<tokio_postgres::Error> for AdapterError {
    fn from(err: tokio_postgres::Error) -> Self {
        if let Some(db_error) = err.as_db_error() {
            return categorize(Some(db_error.code().code()), db_error.message());
        }
        if err.is_closed() {
            return AdapterError::Unavailable(err.to_string());
        }
        AdapterError::Sql {
            state: None,
            message: err.to_string(),
        }
    }
}

impl From<forge_core::bootstrap::RegistrationError> for AdapterError {
    fn from(err: forge_core::bootstrap::RegistrationError) -> Self {
        AdapterError::Protocol(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_categorize() {
        assert!(matches!(
            categorize(Some("42501"), "Access denied"),
            AdapterError::AccessDenied(_)
        ));
        assert!(matches!(
            categorize(
                Some("40001"),
                "Duplicate key value violates unique constraint"
            ),
            AdapterError::OutcomeConflict(_)
        ));
        assert!(matches!(
            categorize(
                Some("23505"),
                "could not serialize access due to concurrent update"
            ),
            AdapterError::OutcomeConflict(_)
        ));
        assert!(matches!(
            categorize(Some("42501"), "result_mismatch"),
            AdapterError::ResultMismatch(_)
        ));
        assert!(matches!(
            categorize(Some("42501"), "lease expired"),
            AdapterError::LeaseExpired(_)
        ));
        assert!(matches!(
            categorize(Some("42501"), "lease lost"),
            AdapterError::LeaseExpired(_)
        ));
        assert!(matches!(
            categorize(None, "unknown error"),
            AdapterError::Sql {
                state: None,
                message: _
            }
        ));
    }
}
