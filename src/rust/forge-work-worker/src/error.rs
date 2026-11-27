/// The worker crate's error taxonomy, mirroring the Python exceptions
/// the loop branches on: WorkerStoreError carries an HTTP-style status
/// and a stable code (the SQLSTATE translation in work_worker_store:
/// P0008 lease expired, P0009 outcome conflict, P0007 result mismatch,
/// 42501 access denied); execution/session/cleanup failures are the
/// executor's own families.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerError {
    Store {
        status: u16,
        code: String,
        message: String,
    },
    Execution(String),
    WorkspaceBusy(String),
    Session(String),
    AuthCleanup(String),
    HeartbeatCleanup(String),
    Protocol(String),
}

impl WorkerError {
    pub fn store(status: u16, code: &str, message: impl Into<String>) -> WorkerError {
        WorkerError::Store {
            status,
            code: code.to_string(),
            message: message.into(),
        }
    }

    pub fn lease_expired() -> WorkerError {
        WorkerError::store(409, "lease_expired", "Attempt lease is not active")
    }

    pub fn outcome_conflict() -> WorkerError {
        WorkerError::store(409, "outcome_conflict", "Outcome or chunk conflict")
    }

    pub fn result_mismatch() -> WorkerError {
        WorkerError::store(
            409,
            "result_mismatch",
            "Result manifest does not match staged chunks",
        )
    }

    pub fn access_denied() -> WorkerError {
        WorkerError::store(
            403,
            "denied",
            "Worker role lacks required grant or attempt ownership",
        )
    }

    pub fn execution(message: impl Into<String>) -> WorkerError {
        WorkerError::Execution(message.into())
    }

    pub fn protocol(message: impl Into<String>) -> WorkerError {
        WorkerError::Protocol(message.into())
    }
}

impl std::fmt::Display for WorkerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WorkerError::Store {
                status,
                code,
                message,
            } => write!(f, "store error {status} {code}: {message}"),
            WorkerError::Execution(m) => write!(f, "execution error: {m}"),
            WorkerError::WorkspaceBusy(m) => write!(f, "workspace busy: {m}"),
            WorkerError::Session(m) => write!(f, "session error: {m}"),
            WorkerError::AuthCleanup(m) => write!(f, "auth cleanup failed: {m}"),
            WorkerError::HeartbeatCleanup(m) => {
                write!(f, "heartbeat cleanup failed: {m}")
            }
            WorkerError::Protocol(m) => write!(f, "protocol error: {m}"),
        }
    }
}

impl std::error::Error for WorkerError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_families_match_python() {
        assert_eq!(
            WorkerError::lease_expired(),
            WorkerError::store(409, "lease_expired", "Attempt lease is not active")
        );
        assert_eq!(
            WorkerError::access_denied(),
            WorkerError::store(
                403,
                "denied",
                "Worker role lacks required grant or attempt ownership"
            )
        );
    }
}
