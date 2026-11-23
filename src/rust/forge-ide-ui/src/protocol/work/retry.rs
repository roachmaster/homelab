//! A retry allowance belongs to one unresolved event, never the whole task.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkRetryKind {
    Compilation,
    Tests,
    Browser,
    Lint,
    Formatting,
    Planning,
    Review,
    Merge,
    Infrastructure,
    Verification,
    FeedbackDelivery,
    ReviewerCorrection,
}
impl WorkRetryKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Compilation => "Compilation",
            Self::Tests => "Tests",
            Self::Browser => "Browser checks",
            Self::Lint => "Lint",
            Self::Formatting => "Formatting",
            Self::Planning => "Plan review",
            Self::Review => "Code review",
            Self::Merge => "Merge",
            Self::Infrastructure => "Infrastructure",
            Self::Verification => "Verification",
            Self::FeedbackDelivery => "Feedback delivery",
            Self::ReviewerCorrection => "Reviewer response",
        }
    }
    pub fn action(self) -> &'static str {
        match self {
            Self::Compilation => "Fixing compilation",
            Self::Tests => "Fixing tests",
            Self::Browser => "Fixing browser checks",
            Self::Lint => "Fixing lint",
            Self::Formatting => "Fixing formatting",
            Self::Planning => "Revising plan",
            Self::Review => "Addressing code review",
            Self::Merge => "Repairing merge",
            Self::Infrastructure => "Retrying infrastructure",
            Self::Verification => "Retrying verification",
            Self::FeedbackDelivery => "Recovering feedback delivery",
            Self::ReviewerCorrection => "Correcting reviewer response",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkRetryStatus {
    Open,
    Resolved,
    Exhausted,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkRetryEvent {
    pub event_id: String,
    pub kind: WorkRetryKind,
    pub used: u32,
    pub limit: u32,
    pub status: WorkRetryStatus,
}
impl WorkRetryEvent {
    pub fn is_valid(&self) -> bool {
        valid_chat_id(&self.event_id)
            && self.limit == MAX_AGENT_RETRIES
            && self.used <= self.limit
            && (self.status != WorkRetryStatus::Exhausted || self.used == self.limit)
    }
    pub fn label(&self) -> String {
        match self.status {
            WorkRetryStatus::Open => format!(
                "{} · Retry {}/{}",
                self.kind.action(),
                self.used,
                self.limit
            ),
            WorkRetryStatus::Exhausted => format!(
                "{} needs attention · Retry {}/{}",
                self.kind.label(),
                self.used,
                self.limit
            ),
            WorkRetryStatus::Resolved => {
                format!("{} resolved · {} retries", self.kind.label(), self.used)
            }
            WorkRetryStatus::Cancelled => {
                format!("{} cancelled · {} retries", self.kind.label(), self.used)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn event_allowance_is_strict_and_independent_of_task_totals() {
        let raw = serde_json::json!({"event_id":"11111111-1111-4111-8111-111111111111","kind":"tests","used":4,"limit":20,"status":"open"});
        let event: WorkRetryEvent = serde_json::from_value(raw.clone()).unwrap();
        assert!(event.is_valid());
        assert_eq!(event.label(), "Fixing tests · Retry 4/20");
        for (field, value) in [("used", 21), ("limit", 19), ("limit", 21)] {
            let mut changed = raw.clone();
            changed[field] = value.into();
            assert!(!serde_json::from_value::<WorkRetryEvent>(changed)
                .unwrap()
                .is_valid());
        }
        let mut exhausted = event;
        exhausted.status = WorkRetryStatus::Exhausted;
        assert!(!exhausted.is_valid());
        exhausted.used = 20;
        assert!(exhausted.is_valid());
    }
}
