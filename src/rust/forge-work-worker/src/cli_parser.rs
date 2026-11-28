//! The incremental bounded machine-event parser (work_executor's
//! CliParser). It retains no tool arguments or thoughts - only the
//! final response text, the usage envelope, failure classification,
//! and session identity.

use crate::cli_argv::{blocked_outcomes, provider_error};
use crate::cli_transport::{parse_record, record_sha256};
use crate::error::WorkerError;
use crate::provider_events::{GrokEvents, MuseEvents};
use crate::quota::quota_diagnostic;
use crate::types::{CliOutcome, StopProof};
use std::collections::BTreeSet;

const BLOCKED_FAILURES: [&str; 3] = ["quota_blocked", "blocked_auth", "blocked_permission"];

fn failure_priority(status: &str) -> i64 {
    match status {
        "failed" => 1,
        "blocked_permission" => 2,
        "blocked_auth" => 3,
        "quota_blocked" => 4,
        _ => 0,
    }
}

fn execution(summary: &str) -> WorkerError {
    WorkerError::execution("failed", summary)
}

fn policy_number(key: &str) -> usize {
    crate::policy::policy_document(crate::policy::POLICY_ID)[key]
        .as_u64()
        .unwrap_or(0) as usize
}

#[derive(Debug)]
pub struct CliParser {
    pub backend: String,
    buffer: Vec<u8>,
    pub stream_bytes: usize,
    pub record_ordinal: usize,
    pub conversation_id: Option<String>,
    pub response: String,
    pub usage: Option<serde_json::Value>,
    pub complete: bool,
    pub failure: Option<String>,
    pub observed_blocked: BTreeSet<String>,
    pub denied: bool,
    pub saw_work: bool,
    pub error_messages: Vec<String>,
    pub stderr_tail: Vec<u8>,
    stderr_scan_tail: Vec<u8>,
    stderr_blocked: BTreeSet<String>,
    stderr_denied: bool,
    pub quota_diagnostic: Option<serde_json::Value>,
    grok_events: Option<GrokEvents>,
    muse_events: Option<MuseEvents>,
}

impl CliParser {
    pub fn new(
        backend: &str,
        expected_conversation_id: Option<&str>,
    ) -> Result<CliParser, WorkerError> {
        if !matches!(backend, "antigravity" | "codex" | "grok" | "muse") {
            return Err(WorkerError::Protocol("Invalid CLI backend".to_string()));
        }
        let mut parser = CliParser {
            backend: backend.to_string(),
            buffer: Vec::new(),
            stream_bytes: 0,
            record_ordinal: 0,
            conversation_id: None,
            response: String::new(),
            usage: None,
            complete: false,
            failure: None,
            observed_blocked: BTreeSet::new(),
            denied: false,
            saw_work: false,
            error_messages: Vec::new(),
            stderr_tail: Vec::new(),
            stderr_scan_tail: Vec::new(),
            stderr_blocked: BTreeSet::new(),
            stderr_denied: false,
            quota_diagnostic: None,
            grok_events: if backend == "grok" {
                Some(GrokEvents::new(None))
            } else {
                None
            },
            muse_events: if backend == "muse" {
                Some(MuseEvents::new())
            } else {
                None
            },
        };
        if backend == "grok" {
            parser.grok_events = Some(GrokEvents::new(
                expected_conversation_id.map(|s| s.to_string()),
            ));
        }
        if let Some(expected) = expected_conversation_id {
            parser.session(expected)?;
        }
        Ok(parser)
    }

    pub fn has_blocked_diagnostic(&self) -> bool {
        !self.observed_blocked.is_empty() || !self.stderr_blocked.is_empty() || self.denied
    }

    pub fn grok_cancelled(&self) -> bool {
        self.grok_events
            .as_ref()
            .map(|e| e.cancelled)
            .unwrap_or(false)
    }

    pub fn grok_permission_cancelled(&self) -> bool {
        self.grok_events
            .as_ref()
            .map(|e| e.permission_cancelled)
            .unwrap_or(false)
    }

    pub fn muse_cancelled(&self) -> bool {
        self.muse_events
            .as_ref()
            .map(|e| e.cancelled)
            .unwrap_or(false)
    }

    fn record_failure(&mut self, status: &str) {
        if BLOCKED_FAILURES.contains(&status) {
            self.observed_blocked.insert(status.to_string());
        }
        let replace = match &self.failure {
            None => true,
            Some(current) => failure_priority(status) > failure_priority(current),
        };
        if replace {
            self.failure = Some(status.to_string());
        }
    }

    fn record_failure_text(&mut self, messages: &[Option<&str>]) {
        let joined = messages
            .iter()
            .filter_map(|m| *m)
            .collect::<Vec<_>>()
            .join("\n");
        if let Some(diagnostic) = quota_diagnostic(&joined) {
            self.quota_diagnostic = Some(diagnostic);
        }
        let mut found = BTreeSet::new();
        for message in messages.iter().flatten() {
            found.extend(blocked_outcomes(message));
        }
        if found.is_empty() {
            self.record_failure("failed");
        } else {
            for status in found {
                self.record_failure(status);
            }
        }
    }

    fn remember_error(&mut self, message: Option<&str>) {
        if let Some(message) = message {
            if !message.trim().is_empty() && self.error_messages.len() < 16 {
                self.error_messages
                    .push(message.chars().take(1024).collect());
            }
        }
    }

    pub fn feed(&mut self, data: &[u8], stderr: bool) -> Result<(), WorkerError> {
        self.stream_bytes += data.len();
        if self.stream_bytes > policy_number("max_stream_bytes") {
            return Err(execution(
                "Agent output exceeded its declared stream resource bound.",
            ));
        }
        if stderr {
            let mut window_bytes = self.stderr_scan_tail.clone();
            window_bytes.extend_from_slice(data);
            let window = String::from_utf8_lossy(&window_bytes).to_lowercase();
            for status in blocked_outcomes(&window) {
                self.stderr_blocked.insert(status.to_string());
            }
            if let Some(diagnostic) = quota_diagnostic(&window) {
                self.quota_diagnostic = Some(diagnostic);
            }
            if window.contains("no output produced")
                || window.contains("permission that headless mode cannot prompt")
            {
                self.stderr_denied = true;
            }
            self.stderr_scan_tail.extend_from_slice(data);
            trim_front(&mut self.stderr_scan_tail, 4096);
            self.stderr_tail.extend_from_slice(data);
            trim_front(&mut self.stderr_tail, 4096);
            return Ok(());
        }
        self.buffer.extend_from_slice(data);
        while let Some(newline) = self.buffer.iter().position(|b| *b == b'\n') {
            let record: Vec<u8> = self.buffer.drain(..=newline).collect();
            let record = &record[..record.len() - 1];
            if !String::from_utf8_lossy(record).trim().is_empty() {
                self.record(record)?;
            }
        }
        Ok(())
    }

    fn record(&mut self, record: &[u8]) -> Result<(), WorkerError> {
        self.record_ordinal += 1;
        let value = parse_record(record, &self.backend).map_err(|failure| {
            let diagnostic = serde_json::json!({
                "version": 1,
                "backend": self.backend,
                "reason": failure.reason,
                "path": failure.path,
                "record_ordinal": self.record_ordinal,
                "record_byte_count": record.len(),
                "record_sha256": record_sha256(record),
            });
            WorkerError::Transport {
                diagnostic: crate::policy::canonical_json(&diagnostic),
            }
        })?;
        match self.backend.as_str() {
            "antigravity" => self.antigravity(&value),
            "grok" => {
                let mut events = self.grok_events.take().unwrap();
                let result = events.record(&value);
                if let Some(session) = &events.session {
                    let session = session.clone();
                    self.grok_events = Some(events);
                    result.map_err(|_| {
                        execution("The coding agent returned malformed Grok machine output.")
                    })?;
                    self.session(&session)?;
                } else {
                    self.grok_events = Some(events);
                    result.map_err(|_| {
                        execution("The coding agent returned malformed Grok machine output.")
                    })?;
                }
                let events = self.grok_events.as_ref().unwrap();
                self.response = events.response.clone();
                self.usage = events.usage.clone();
                self.complete = events.complete;
                self.saw_work = events.saw_work;
                if let Some(failure) = &events.failure {
                    let failure = failure.clone();
                    self.record_failure_text(&[Some(&failure)]);
                }
                Ok(())
            }
            "muse" => {
                let mut events = self.muse_events.take().unwrap();
                let result = events.record(&value);
                if let Some(session) = &events.session {
                    let session = session.clone();
                    self.muse_events = Some(events);
                    result.map_err(|_| {
                        execution("The coding agent returned malformed Muse machine output.")
                    })?;
                    self.session(&session)?;
                } else {
                    self.muse_events = Some(events);
                    result.map_err(|_| {
                        execution("The coding agent returned malformed Muse machine output.")
                    })?;
                }
                let events = self.muse_events.as_ref().unwrap();
                self.response = events.response.clone();
                self.usage = events.usage.clone();
                self.complete = events.complete;
                self.saw_work = events.saw_work;
                if let Some(failure) = &events.failure {
                    let failure = failure.clone();
                    self.record_failure_text(&[Some(&failure)]);
                }
                Ok(())
            }
            _ => self.codex(&value),
        }
    }

    fn session(&mut self, value: &str) -> Result<(), WorkerError> {
        let parsed = uuid::Uuid::parse_str(value).map_err(|_| {
            execution("The coding agent returned an invalid conversation identity.")
        })?;
        let canonical = parsed.to_string();
        if let Some(existing) = &self.conversation_id {
            if *existing != canonical {
                return Err(execution("The coding agent changed conversation identity."));
            }
        }
        self.conversation_id = Some(canonical);
        Ok(())
    }

    fn antigravity(&mut self, event: &serde_json::Value) -> Result<(), WorkerError> {
        match event.get("event").and_then(|v| v.as_str()) {
            Some("init") => {
                let id = event
                    .get("conversation_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                // Python passes the raw value to _session, which
                // rejects non-UUIDs; a missing id errors the same way.
                return self.session(id);
            }
            Some("step_update") => {
                if let Some(update) = event.get("step_update").filter(|v| v.is_object()) {
                    if let Some(id) = update.get("conversation_id").filter(|v| !v.is_null()) {
                        return self.session(id.as_str().unwrap_or(""));
                    }
                }
                return Ok(());
            }
            Some("result") => {}
            _ => return Ok(()),
        }
        if self.complete {
            return Err(execution(
                "The coding agent returned multiple terminal results.",
            ));
        }
        let result = event
            .get("result")
            .filter(|v| v.is_object())
            .ok_or_else(|| execution("The coding agent returned an invalid terminal result."))?;
        if let Some(id) = result.get("conversation_id").and_then(|v| v.as_str()) {
            self.session(id)?;
        } else {
            self.session("")?;
        }
        let status = result.get("status").and_then(|v| v.as_str());
        if status != Some("SUCCESS") {
            match result.get("error").filter(|v| v.is_object()) {
                Some(error) => {
                    let code = error.get("code").and_then(|v| v.as_str());
                    let message = error.get("message").and_then(|v| v.as_str());
                    self.record_failure_text(&[code, message, status]);
                }
                _ => self.record_failure_text(&[status]),
            }
        }
        let denied = result.get("denied_actions");
        let denied_bad = denied.map(|v| !v.is_array()).unwrap_or(false);
        let denied_any = denied
            .and_then(|v| v.as_array())
            .map(|a| !a.is_empty())
            .unwrap_or(false);
        if denied_bad || denied_any {
            self.denied = true;
            self.observed_blocked
                .insert("blocked_permission".to_string());
        }
        let response = result
            .get("response")
            .and_then(|v| v.as_str())
            .ok_or_else(|| execution("The coding agent returned no readable response."))?;
        self.response = response.to_string();
        self.usage = result.get("usage").filter(|v| v.is_object()).cloned();
        self.complete = true;
        Ok(())
    }

    fn codex(&mut self, event: &serde_json::Value) -> Result<(), WorkerError> {
        let kind = event.get("type").and_then(|v| v.as_str());
        if let Some(kind) = kind {
            if kind == "turn.started" || kind.starts_with("item.") {
                self.saw_work = true;
            }
        }
        match kind {
            Some("thread.started") => {
                let id = event
                    .get("thread_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                self.session(id)?;
            }
            Some("item.completed") => {
                let item = event
                    .get("item")
                    .filter(|v| v.is_object())
                    .ok_or_else(|| execution("The coding agent returned an invalid item."))?;
                match item.get("type").and_then(|v| v.as_str()) {
                    Some("agent_message") => {
                        let text = item.get("text").and_then(|v| v.as_str()).ok_or_else(|| {
                            execution("The coding agent returned invalid response text.")
                        })?;
                        self.response = text.to_string();
                    }
                    Some("command_execution")
                        if item.get("status").and_then(|v| v.as_str()) == Some("declined") =>
                    {
                        self.denied = true;
                        self.observed_blocked
                            .insert("blocked_permission".to_string());
                    }
                    _ => {}
                }
            }
            Some("turn.failed") => {
                let error = event.get("error");
                let (message, code) = match error {
                    Some(e) if e.is_object() => (
                        e.get("message").and_then(|v| v.as_str()).unwrap_or(""),
                        e.get("code").and_then(|v| v.as_str()).unwrap_or(""),
                    ),
                    Some(e) if e.is_string() => (e.as_str().unwrap(), ""),
                    _ => ("", ""),
                };
                self.remember_error(Some(message));
                self.record_failure_text(&[Some(message), Some(code)]);
            }
            Some("error") => {
                let message = event.get("message").and_then(|v| v.as_str());
                let code = event.get("code").and_then(|v| v.as_str());
                self.remember_error(message);
                self.record_failure_text(&[message, code]);
            }
            Some("turn.completed") => {
                if self.complete {
                    return Err(execution(
                        "The coding agent returned multiple completed turns.",
                    ));
                }
                self.saw_work = true;
                self.usage = event.get("usage").filter(|v| v.is_object()).cloned();
                self.complete = true;
            }
            _ => {}
        }
        Ok(())
    }

    /// Settle the run into its final outcome (CliParser.finish).
    pub fn finish(
        &mut self,
        returncode: i32,
        proof: StopProof,
        interrupted: bool,
    ) -> Result<CliOutcome, WorkerError> {
        if !String::from_utf8_lossy(&self.buffer).trim().is_empty() {
            let rest = std::mem::take(&mut self.buffer);
            self.record(&rest)?;
        }
        self.buffer.clear();
        let conversation_id = self.conversation_id.clone();
        let usage_snapshot = self.usage.clone();
        let outcome = |status: &str, text: String, summary: &str| CliOutcome {
            status: status.to_string(),
            text,
            conversation_id: conversation_id.clone(),
            usage: usage_snapshot.clone(),
            summary: summary.to_string(),
            stop_proof: proof.clone(),
            fresh_session: true,
            quota_diagnostic: None,
        };
        if !proof.scope_stopped {
            return Ok(outcome(
                "needs_reconcile",
                String::new(),
                "Agent scope stop could not be verified.",
            ));
        }
        if interrupted {
            return Ok(outcome(
                "cancelled",
                String::new(),
                "Agent scope stopped after an interruption request.",
            ));
        }
        if self.grok_cancelled() {
            if self.grok_permission_cancelled() {
                return Ok(outcome(
                    "blocked_permission",
                    String::new(),
                    "Grok permission_cancelled: its headless permission gate cancelled a tool request; the process scope stopped.",
                ));
            }
            return Ok(outcome(
                "cancelled",
                String::new(),
                "Grok cancelled the run and its process scope stopped.",
            ));
        }
        if self.muse_cancelled() {
            return Ok(outcome(
                "cancelled",
                String::new(),
                "The Muse run was cancelled and its process scope stopped.",
            ));
        }
        if returncode != 0 || self.failure.is_some() {
            let stderr_blocked: Vec<String> = self.stderr_blocked.iter().cloned().collect();
            for status in &stderr_blocked {
                self.record_failure(status);
            }
            if self.failure.is_none() {
                self.record_failure("failed");
            }
        }
        let failure = self.failure.clone();
        // An empty-output diagnostic is less specific than a
        // provider's quota or authentication failure; preserve the
        // actionable blocked state.
        if let Some(failure) = &failure {
            if failure == "quota_blocked" {
                let mut result = outcome(
                    "quota_blocked",
                    String::new(),
                    "Provider quota is exhausted; the stopped assignment is waiting for allowance.",
                );
                result.quota_diagnostic = self.quota_diagnostic.clone();
                return Ok(result);
            }
            if failure == "blocked_auth" {
                return Ok(outcome(
                    "blocked_auth",
                    String::new(),
                    "Agent invocation did not complete successfully.",
                ));
            }
        }
        if self.stderr_denied {
            self.denied = true;
        }
        if self.denied {
            self.observed_blocked
                .insert("blocked_permission".to_string());
            return Ok(outcome(
                "blocked_permission",
                String::new(),
                "Agent required a permission unavailable in the approved profile.",
            ));
        }
        if returncode != 0 || self.failure.is_some() {
            return Ok(outcome(
                self.failure.as_deref().unwrap_or("failed"),
                String::new(),
                "Agent invocation did not complete successfully.",
            ));
        }
        let encoded_len = self.response.len();
        if !self.complete
            || self.conversation_id.is_none()
            || self.response.trim().is_empty()
            || encoded_len == 0
        {
            return Ok(outcome(
                "failed",
                String::new(),
                "Agent returned no coherent nonempty final response.",
            ));
        }
        if encoded_len > policy_number("max_result_bytes") {
            return Ok(outcome(
                "failed",
                String::new(),
                "Agent response exceeded its declared result resource bound.",
            ));
        }
        Ok(outcome(
            "awaiting_review",
            self.response.clone(),
            "Agent response is ready for explicit review acceptance.",
        ))
    }
}

fn trim_front(buffer: &mut Vec<u8>, keep: usize) {
    if buffer.len() > keep {
        buffer.drain(..buffer.len() - keep);
    }
}

/// The provider error entry point the runner uses for stderr-only
/// failures.
pub fn classify_provider_error(message: &str) -> &'static str {
    provider_error(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proof() -> StopProof {
        StopProof {
            unit_name: "forge-coding-x".to_string(),
            scope_stopped: true,
            source: "test".to_string(),
            checked_at: "2026-11-27T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn codex_happy_path() {
        let mut parser = CliParser::new("codex", None).unwrap();
        let id = "12345678-1234-1234-1234-123456789012";
        parser
            .feed(
                format!("{{\"type\":\"thread.started\",\"thread_id\":\"{id}\"}}\n").as_bytes(),
                false,
            )
            .unwrap();
        parser
            .feed(
                b"{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"done\"}}\n",
                false,
            )
            .unwrap();
        parser
            .feed(
                b"{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":3}}\n",
                false,
            )
            .unwrap();
        let outcome = parser.finish(0, proof(), false).unwrap();
        assert_eq!(outcome.status, "awaiting_review");
        assert_eq!(outcome.text, "done");
        assert_eq!(outcome.conversation_id.as_deref(), Some(id));
    }

    #[test]
    fn codex_quota_failure() {
        let mut parser = CliParser::new("codex", None).unwrap();
        parser
            .feed(
                b"{\"type\":\"error\",\"message\":\"usage_limit_reached; resets in 10m\"}\n",
                false,
            )
            .unwrap();
        let outcome = parser.finish(1, proof(), false).unwrap();
        assert_eq!(outcome.status, "quota_blocked");
        assert!(outcome.quota_diagnostic.is_some());
    }

    #[test]
    fn malformed_record_carries_diagnostic() {
        let mut parser = CliParser::new("codex", None).unwrap();
        let error = parser.feed(b"{\"a\":1,\"a\":2}\n", false).unwrap_err();
        match error {
            WorkerError::Transport { diagnostic } => {
                assert!(diagnostic.contains("duplicate_field"));
            }
            other => panic!("unexpected error: {other}"),
        }
    }
}
