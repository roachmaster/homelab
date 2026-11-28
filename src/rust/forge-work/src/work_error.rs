use std::fmt;

/// The crate's single error type: domain rejections (status + stable code, mirroring Python's Rejected), missing objects, and adapter failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkError {
    Rejected { status: u16, code: String },
    NotFound { what: String },
    Store { message: String },
}

impl WorkError {
    /// Creates a new `WorkError` with a rejected status and code.
    pub fn rejected(status: u16, code: impl Into<String>) -> WorkError {
        WorkError::Rejected {
            status,
            code: code.into(),
        }
    }

    /// Creates a new `WorkError` indicating a missing object.
    pub fn not_found(what: impl Into<String>) -> WorkError {
        WorkError::NotFound { what: what.into() }
    }

    /// Creates a new `WorkError` for adapter failures.
    pub fn store(message: impl Into<String>) -> WorkError {
        WorkError::Store {
            message: message.into(),
        }
    }
}

impl fmt::Display for WorkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WorkError::Rejected { status, code } => write!(f, "rejected {}: {}", status, code),
            WorkError::NotFound { what } => write!(f, "not found: {}", what),
            WorkError::Store { message } => write!(f, "store failure: {}", message),
        }
    }
}

impl std::error::Error for WorkError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rejected() {
        let error = WorkError::rejected(400, "invalid_request");
        match &error {
            WorkError::Rejected { status, code } => {
                assert_eq!(*status, 400);
                assert_eq!(code, "invalid_request");
            }
            other => panic!("wrong variant: {other:?}"),
        }
        assert_eq!(error.to_string(), "rejected 400: invalid_request");
    }

    #[test]
    fn test_not_found() {
        let error = WorkError::not_found("task");
        match &error {
            WorkError::NotFound { what } => assert_eq!(what, "task"),
            other => panic!("wrong variant: {other:?}"),
        }
        assert_eq!(error.to_string(), "not found: task");
    }

    #[test]
    fn test_store() {
        let error = WorkError::store("database connection failed");
        match &error {
            WorkError::Store { message } => {
                assert_eq!(message, "database connection failed")
            }
            other => panic!("wrong variant: {other:?}"),
        }
        assert_eq!(
            error.to_string(),
            "store failure: database connection failed"
        );
    }
}
