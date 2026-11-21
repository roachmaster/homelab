//! Bounded task-event timeline. Database history remains authoritative.
use super::*;
const PAGE: usize = 50;
const SUMMARY_BYTES: usize = 256 * 1024;
const BOOKMARKS: usize = 64;
#[derive(Debug)]
pub(super) struct HistoryRequest {
    cursor: u64,
    direction: Direction,
}
impl Agents {
    pub(super) fn clear_history(&mut self) {
        self.history.clear();
        self.history_task = None;
        self.history_correlation = None;
        self.history_cursor = 0;
        self.history_bookmarks.clear();
        self.history_next = None;
        self.history_loaded = false;
        self.history_request = None;
    }
    pub(super) fn history_action_valid(&self, task: &str, correlation: &str, cursor: u64) -> bool {
        self.pane == Pane::Reader
            && self.pending.is_none()
            && !self.locked()
            && self.history_cursor == cursor
            && self
                .task
                .as_ref()
                .is_some_and(|t| t.task_id == task && t.correlation_id == correlation)
    }
    pub(super) fn history_page(
        &mut self,
        id: String,
        direction: Direction,
        cursor: u64,
        effects: &mut Vec<UiEffect>,
    ) {
        let Some(task) = self.task.as_ref() else {
            return;
        };
        self.history_request = Some(HistoryRequest { cursor, direction });
        self.request(
            WorkRequest::WorkEvents {
                request_id: id,
                project_alias: self.project.clone(),
                task_id: task.task_id.clone(),
                after_sequence: cursor,
                limit: PAGE,
            },
            effects,
        );
    }
    pub(super) fn accept_history(
        &mut self,
        page: HistoryRequest,
        events: Vec<WorkLogEvent>,
        next: u64,
    ) {
        self.history_loaded = true;
        if events.is_empty() && matches!(page.direction, Direction::Next) {
            self.history_next = None;
            self.status = "End of event history.".into();
            return;
        }
        let mut size = 0usize;
        let valid = events.iter().all(|e| {
            size = size
                .saturating_add(e.progress.as_ref().map_or(0, |p| p.summary.len()))
                .saturating_add(e.external_progress.as_ref().map_or(0, |p| p.summary.len()))
                .saturating_add(e.workflow_progress.as_ref().map_or(0, |p| p.summary.len()));
            size <= SUMMARY_BYTES
        });
        if !valid || events.len() > PAGE {
            self.warning = true;
            self.status = "The event page exceeds its display bound. Refresh to retry.".into();
            return;
        }
        match page.direction {
            Direction::Current => self.history_bookmarks.clear(),
            Direction::Next => {
                if self.history_bookmarks.len() == BOOKMARKS {
                    self.history_bookmarks.remove(0);
                }
                self.history_bookmarks.push(self.history_cursor);
            }
            Direction::Previous => {
                self.history_bookmarks.pop();
            }
        }
        self.history_cursor = page.cursor;
        self.history_next = (events.len() == PAGE).then_some(next);
        self.history = events.clone();
        if !self.events_loaded && page.cursor == 0 {
            self.event_sequence = next;
            self.events_loaded = true;
            self.events_more = events.len() == PAGE;
            self.keep_progress(events);
        }
    }
    pub(super) fn update_history(&mut self, events: &[WorkLogEvent]) {
        if !self.history_loaded || self.history_next.is_some() {
            return;
        }
        let last = self
            .history
            .last()
            .map_or(self.history_cursor, |e| e.sequence);
        for event in events.iter().filter(|e| e.sequence > last) {
            if self.history.len() == PAGE {
                self.history_next = self.history.last().map(|e| e.sequence);
                break;
            }
            self.history.push(event.clone());
        }
    }
    fn history_summary(&self, event: &WorkLogEvent) -> String {
        if let Some(p) = event.workflow_progress.as_ref() {
            if self
                .task
                .as_ref()
                .and_then(|t| t.workflow.as_ref())
                .is_some_and(|w| w.workflow_id == p.workflow_id)
            {
                return p.summary.clone();
            }
        }
        if let Some(p) = event.external_progress.as_ref() {
            if self
                .task
                .as_ref()
                .and_then(|t| t.external_result.as_ref())
                .is_none_or(|r| r.assignment_id == p.assignment_id)
            {
                return p.summary.clone();
            }
        }
        event
            .progress
            .as_ref()
            .filter(|p| {
                event.progress_verified
                    || self
                        .task
                        .as_ref()
                        .and_then(|t| t.attempt.as_ref())
                        .is_some_and(|a| a.attempt_id == p.attempt_id)
            })
            .map_or_else(String::new, |p| p.summary.clone())
    }
    pub(super) fn project_history(&self, view: &mut ViewModel, visible_now: bool) {
        let visible_now = visible_now
            && self.task.as_ref().is_some_and(|t| {
                self.history_task.as_ref() == Some(&t.task_id)
                    && self.history_correlation.as_ref() == Some(&t.correlation_id)
                    && t.project_alias == self.project
            });
        view.visibility
            .push(visible("agents-history-panel", visible_now));
        let task = self.task.as_ref();
        view.texts.push(text(
            "agents-task-correlation",
            task.map_or_else(String::new, |t| {
                format!("Correlation · {}", t.correlation_id)
            }),
        ));
        view.texts.push(text(
            "agents-history-range",
            match (self.history.first(), self.history.last()) {
                (Some(a), Some(b)) => format!("Events {}–{}", a.sequence, b.sequence),
                _ => {
                    if self.history_loaded {
                        "No events recorded".into()
                    } else {
                        "Loading events…".into()
                    }
                }
            },
        ));
        let make_action = |which: u8| {
            task.map(|t| match which {
                0 => RowAction::AgentHistoryFirst {
                    task_id: t.task_id.clone(),
                    correlation_id: t.correlation_id.clone(),
                    after_sequence: self.history_cursor,
                },
                1 => RowAction::AgentHistoryPrevious {
                    task_id: t.task_id.clone(),
                    correlation_id: t.correlation_id.clone(),
                    after_sequence: self.history_cursor,
                },
                _ => RowAction::AgentHistoryNext {
                    task_id: t.task_id.clone(),
                    correlation_id: t.correlation_id.clone(),
                    after_sequence: self.history_cursor,
                },
            })
        };
        for (id, unavailable, which) in [
            ("agents-history-first", self.history_cursor == 0, 0),
            (
                "agents-history-previous",
                self.history_bookmarks.is_empty(),
                1,
            ),
            ("agents-history-next", self.history_next.is_none(), 2),
        ] {
            view.buttons.push(ButtonBinding {
                id,
                disabled: !visible_now || self.locked() || self.pending.is_some() || unavailable,
                pressed: None,
                action: make_action(which),
            });
        }
        view.timelines.push(TimelineBinding {
            id: "agents-history",
            empty_message: if self.history_loaded {
                "No events have been recorded."
            } else {
                "Loading event history…"
            }
            .into(),
            rows: self
                .history
                .iter()
                .filter(|_| visible_now)
                .map(|e| {
                    let summary = self.history_summary(e);
                    let mut details = vec![
                        Property {
                            label: "Event ID".into(),
                            value: e.event_id.clone(),
                        },
                        Property {
                            label: "Correlation".into(),
                            value: e.correlation_id.clone(),
                        },
                        Property {
                            label: "Exact time (UTC)".into(),
                            value: e.created_at.clone(),
                        },
                        Property {
                            label: "Sequence".into(),
                            value: e.sequence.to_string(),
                        },
                    ];
                    if let Some(p) = e.progress.as_ref().filter(|_| !summary.is_empty()) {
                        details.push(Property {
                            label: "Native attempt".into(),
                            value: p.attempt_id.clone(),
                        });
                    }
                    if let Some(p) = e.external_progress.as_ref().filter(|_| !summary.is_empty()) {
                        details.push(Property {
                            label: "External assignment".into(),
                            value: p.assignment_id.clone(),
                        });
                        details.push(Property {
                            label: "Generation".into(),
                            value: p.generation.to_string(),
                        });
                    }
                    if let Some(p) = e.workflow_progress.as_ref().filter(|_| !summary.is_empty()) {
                        details.push(Property {
                            label: "Producer".into(),
                            value: "Forge workflow coordinator".into(),
                        });
                        details.push(Property {
                            label: "Workflow".into(),
                            value: p.workflow_id.clone(),
                        });
                        details.push(Property {
                            label: "Phase".into(),
                            value: p.phase.label().into(),
                        });
                    }
                    if !summary.is_empty() {
                        details.push(Property {
                            label: "Summary".into(),
                            value: summary.clone(),
                        });
                    }
                    let workflow = e.workflow_progress.as_ref().filter(|p| {
                        self.task
                            .as_ref()
                            .and_then(|t| t.workflow.as_ref())
                            .is_some_and(|w| w.workflow_id == p.workflow_id)
                    });
                    let related_action =
                        workflow.and_then(|p| p.related_task_id.as_ref()).map(|id| {
                            RowAction::OpenAgentTask {
                                task_id: id.clone(),
                            }
                        });
                    TimelineRow {
                        event_id: e.event_id.clone(),
                        badge: workflow.map_or_else(
                            || event_badge(e.kind),
                            |p| {
                                badge(
                                    p.phase.label(),
                                    match p.phase {
                                        WorkWorkflowPhase::Completed => BadgeTone::Success,
                                        WorkWorkflowPhase::Escalated => BadgeTone::Warning,
                                        _ => BadgeTone::Accent,
                                    },
                                )
                            },
                        ),
                        time: timestamp(&e.created_at, self.now_ms),
                        summary,
                        details,
                        related_action,
                    }
                })
                .collect(),
        });
    }
}
fn event_badge(kind: WorkEventKind) -> BadgeView {
    let (label, tone) = match kind {
        WorkEventKind::Queued => ("Queued", BadgeTone::Neutral),
        WorkEventKind::Started => ("Started", BadgeTone::Accent),
        WorkEventKind::Checkpoint => ("Progress", BadgeTone::Neutral),
        WorkEventKind::CancelRequested => ("Cancellation requested", BadgeTone::Warning),
        WorkEventKind::Cancelled => ("Cancelled", BadgeTone::Neutral),
        WorkEventKind::AwaitingReview => ("Result received", BadgeTone::Accent),
        WorkEventKind::Completed => ("Completed", BadgeTone::Success),
        WorkEventKind::Failed => ("Failed", BadgeTone::Error),
        WorkEventKind::QuotaBlocked => ("Quota blocked", BadgeTone::Warning),
        WorkEventKind::BlockedAuth => ("Authentication required", BadgeTone::Warning),
        WorkEventKind::BlockedPermission => ("Permission required", BadgeTone::Warning),
        WorkEventKind::NeedsReconcile => ("Reconciliation required", BadgeTone::Warning),
        WorkEventKind::Resumed => ("Resumed", BadgeTone::Accent),
    };
    badge(label, tone)
}
