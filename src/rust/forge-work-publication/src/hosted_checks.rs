//! Read-only, exact-head GitHub Actions observations for an
//! admitted review (work_hosted_checks): the HostedChecksPort
//! implementation. Only repository APIs are used; this provider
//! never reruns workflows, invokes a model, polls account
//! allowance endpoints, selects another account or merges.

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use forge_work_worker::diagnostic::diagnostic_text;
use forge_work_worker::review_ports::{HostedChecksPort, HostedReportData, ReviewError};

use crate::publication_clients::{
    parent_environment, uuid, CommandExecutor, GitHubPullRequestClient, PublicationError,
};
use crate::publication_config::PublicationConfig;
use crate::verification::project_verification;

pub struct HostedCheckReport {
    pub status: String,
    pub checks: Vec<Value>,
    pub findings: Vec<Value>,
    pub summary: String,
    pub toolchain: String,
    pub evidence_digest: String,
}

fn is_sha(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn query(params: &[(&str, String)]) -> String {
    params
        .iter()
        .map(|(key, value)| format!("{}={}", key, value))
        .collect::<Vec<_>>()
        .join("&")
}

/// The last `rustc X.Y.Z` version in a log (Python:
/// re.findall(rb"\brustc\s+(\d+\.\d+\.\d+)", log)).
fn last_rustc_version(log: &[u8]) -> Option<String> {
    let mut found: Option<String> = None;
    let bytes = log;
    let mut index = 0;
    while index + 5 <= bytes.len() {
        if &bytes[index..index + 5] == b"rustc"
            && (index == 0
                || !(bytes[index - 1].is_ascii_alphanumeric() || bytes[index - 1] == b'_'))
        {
            let mut cursor = index + 5;
            if cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                    cursor += 1;
                }
                let start = cursor;
                let mut parts = 0;
                let mut valid = true;
                let mut current = start;
                loop {
                    let digit_start = current;
                    while current < bytes.len() && bytes[current].is_ascii_digit() {
                        current += 1;
                    }
                    if current == digit_start {
                        valid = false;
                        break;
                    }
                    parts += 1;
                    if parts == 3 {
                        break;
                    }
                    if current < bytes.len() && bytes[current] == b'.' {
                        current += 1;
                    } else {
                        valid = false;
                        break;
                    }
                }
                if valid && parts == 3 {
                    found = Some(String::from_utf8_lossy(&bytes[start..current]).into_owned());
                    index = current;
                    continue;
                }
            }
        }
        index += 1;
    }
    found
}

pub struct GitHubHostedChecksProvider {
    pub config: PublicationConfig,
    pub client: GitHubPullRequestClient,
    pub execute: Box<dyn CommandExecutor>,
}

impl GitHubHostedChecksProvider {
    /// Python injects one stateless `command` executor for
    /// both the API client and log downloads; the Rust port
    /// takes two equivalent instances.
    pub fn new(
        config: PublicationConfig,
        client_execute: Box<dyn CommandExecutor>,
        execute: Box<dyn CommandExecutor>,
    ) -> Self {
        let client = GitHubPullRequestClient::new(config.clone(), client_execute);
        GitHubHostedChecksProvider {
            config,
            client,
            execute,
        }
    }

    fn logs(&mut self, repository: &str, job_id: i64) -> Vec<u8> {
        let argv: Vec<String> = [
            "/usr/bin/gh",
            "api",
            "--hostname",
            "github.com",
            "--method",
            "GET",
            "--header",
            "Accept: application/vnd.github+json",
        ]
        .iter()
        .map(|part| part.to_string())
        .chain([format!("repos/{repository}/actions/jobs/{job_id}/logs")])
        .collect();
        match self
            .execute
            .execute(&argv, &parent_environment(&self.config), None, None, 120.0)
        {
            Ok(bytes) => bytes,
            // Failure metadata still blocks acceptance.
            // Unavailable logs do not authorize a rerun or turn
            // a missing diagnostic into success.
            Err(_) => b"Hosted check failed; its bounded diagnostic log is unavailable.".to_vec(),
        }
    }

    pub fn observe_inner(&mut self, job: &Value) -> Result<HostedCheckReport, PublicationError> {
        let head = job
            .get("candidate_commit")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        if !is_sha(head) {
            return Err(PublicationError::Code(
                "invalid_hosted_review_head".to_string(),
            ));
        }
        let head = head.to_string();
        let repository = self
            .config
            .repositories
            .get(job["project_alias"].as_str().unwrap_or_default())
            .cloned()
            .ok_or_else(|| PublicationError::Code("invalid_hosted_review_target".to_string()))?;
        let target = uuid(Some(&job["target_publication_id"]))?;
        let url = job
            .get("pr_url")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        let prefix = format!("https://github.com/{repository}/pull/");
        let number = url.strip_prefix(&prefix).unwrap_or_default();
        if number.is_empty()
            || !number.bytes().all(|byte| byte.is_ascii_digit())
            || number.parse::<i64>().unwrap_or(0) < 1
            || number.parse::<i64>().unwrap_or(0).to_string() != number
        {
            return Err(PublicationError::Code(
                "invalid_hosted_review_target".to_string(),
            ));
        }
        let number = number.to_string();
        let mut identity = job.clone();
        identity["publication_id"] = json!(target);
        identity["target_publication_id"] = json!(target);
        let pr = self
            .client
            .api("GET", &format!("repos/{repository}/pulls/{number}"), None)?;
        self.client.validated(&identity, &head, &pr)?;
        if pr.get("state").and_then(|value| value.as_str()) != Some("open")
            || !pr
                .get("merged_at")
                .map(|value| value.is_null())
                .unwrap_or(false)
        {
            return Err(PublicationError::Code(
                "hosted_review_target_closed".to_string(),
            ));
        }
        let run_query = query(&[
            ("head_sha", head.clone()),
            ("event", "pull_request".to_string()),
            ("per_page", "100".to_string()),
        ]);
        let data = self.client.api(
            "GET",
            &format!("repos/{repository}/actions/runs?{run_query}"),
            None,
        )?;
        let runs = validated_runs(&data)?;
        for run in &runs {
            if !run.is_object()
                || run
                    .get("id")
                    .and_then(|value| value.as_i64())
                    .map(|id| id < 1)
                    .unwrap_or(true)
                || run.get("head_sha").and_then(|value| value.as_str()) != Some(head.as_str())
                || run.get("event").and_then(|value| value.as_str()) != Some("pull_request")
                || run
                    .get("repository")
                    .and_then(|value| value.get("full_name"))
                    .and_then(|value| value.as_str())
                    != Some(repository.as_str())
            {
                return Err(PublicationError::Code(
                    "hosted_run_identity_changed".to_string(),
                ));
            }
        }
        let required = self.config.required_hosted_checks.clone();
        let mut by_workflow: BTreeMap<String, Option<Value>> = BTreeMap::new();
        for [path, _name, _check] in &required {
            let matching: Vec<&Value> = runs
                .iter()
                .filter(|run| {
                    run.get("path").and_then(|value| value.as_str()) == Some(path.as_str())
                })
                .collect();
            let selected = matching
                .into_iter()
                .max_by_key(|run| {
                    (
                        run.get("id").and_then(|value| value.as_i64()).unwrap_or(0),
                        run.get("run_attempt")
                            .and_then(|value| value.as_i64())
                            .unwrap_or(1),
                    )
                })
                .cloned();
            by_workflow.insert(path.clone(), selected);
        }
        let mut jobs_by_run: BTreeMap<i64, Vec<Value>> = BTreeMap::new();
        let mut validation_failures: BTreeMap<String, Value> = BTreeMap::new();
        let missing: Vec<String> = by_workflow
            .iter()
            .filter(|(_, run)| run.is_none())
            .map(|(path, _)| path.clone())
            .collect();
        if !missing.is_empty() {
            // Invalid workflow files can fail on PUSH before
            // GitHub can create their pull-request jobs. Such a
            // run is diagnostic failure only; a successful PUSH
            // never satisfies required PR verification.
            let push_query = query(&[("head_sha", head.clone()), ("per_page", "100".to_string())]);
            let extra = self.client.api(
                "GET",
                &format!("repos/{repository}/actions/runs?{push_query}"),
                None,
            )?;
            let extra_runs = validated_runs(&extra)?;
            for path in &missing {
                let matches: Vec<&Value> = extra_runs
                    .iter()
                    .filter(|run| {
                        run.is_object()
                            && run.get("path").and_then(|value| value.as_str())
                                == Some(path.as_str())
                            && run.get("event").and_then(|value| value.as_str()) == Some("push")
                            && run.get("head_sha").and_then(|value| value.as_str())
                                == Some(head.as_str())
                            && run
                                .get("repository")
                                .and_then(|value| value.get("full_name"))
                                .and_then(|value| value.as_str())
                                == Some(repository.as_str())
                            && run.get("status").and_then(|value| value.as_str())
                                == Some("completed")
                            && run.get("conclusion").and_then(|value| value.as_str())
                                == Some("failure")
                            && run
                                .get("id")
                                .and_then(|value| value.as_i64())
                                .map(|id| id > 0)
                                .unwrap_or(false)
                    })
                    .collect();
                let failed = match matches.into_iter().max_by_key(|run| {
                    (
                        run.get("id").and_then(|value| value.as_i64()).unwrap_or(0),
                        run.get("run_attempt")
                            .and_then(|value| value.as_i64())
                            .unwrap_or(1),
                    )
                }) {
                    Some(run) => run.clone(),
                    None => continue,
                };
                let payload = self.client.api(
                    "GET",
                    &format!(
                        "repos/{repository}/actions/runs/{}/jobs?filter=latest&per_page=100",
                        failed["id"]
                    ),
                    None,
                )?;
                if payload.get("jobs") == Some(&Value::Array(Vec::new()))
                    && payload
                        .get("total_count")
                        .and_then(|value| value.as_i64())
                        .unwrap_or(0)
                        == 0
                {
                    let run_id = failed["id"].as_i64().unwrap_or(0);
                    validation_failures.insert(path.clone(), failed);
                    jobs_by_run.insert(run_id, Vec::new());
                }
            }
        }
        let mut observations: Vec<Value> = Vec::new();
        let mut findings: Vec<Value> = Vec::new();
        let mut evidence: Vec<Value> = Vec::new();
        let mut toolchain = "Not reported".to_string();
        for [path, workflow_name, check_name] in &required {
            let validation_failure = validation_failures.contains_key(path);
            let run = by_workflow
                .get(path)
                .cloned()
                .flatten()
                .or_else(|| validation_failures.get(path).cloned());
            let mut observed = Map::new();
            observed.insert("name".to_string(), json!(check_name));
            observed.insert("status".to_string(), json!("pending"));
            observed.insert("head_sha".to_string(), json!(head));
            let mut selected: Option<Value> = None;
            let mut invalid_workflow = false;
            if let Some(run) = &run {
                if run.get("name").and_then(|value| value.as_str()) != Some(workflow_name.as_str())
                {
                    invalid_workflow = true;
                }
                let run_id = run["id"].as_i64().unwrap_or(0);
                if let std::collections::btree_map::Entry::Vacant(e) = jobs_by_run.entry(run_id) {
                    let payload = self
                        .client
                        .api(
                            "GET",
                            &format!(
                                "repos/{repository}/actions/runs/{run_id}/jobs?filter=latest&per_page=100"
                            ),
                            None,
                        )
                        ?;
                    let jobs = payload
                        .get("jobs")
                        .and_then(|value| value.as_array())
                        .cloned();
                    let total = payload
                        .get("total_count")
                        .and_then(|value| value.as_i64())
                        .unwrap_or(0);
                    match jobs {
                        Some(jobs) if jobs.len() <= 100 && total <= 100 => {
                            e.insert(jobs);
                        }
                        _ => {
                            return Err(PublicationError::Code(
                                "hosted_job_history_unavailable".to_string(),
                            ))
                        }
                    }
                }
                let jobs = jobs_by_run.get(&run_id).cloned().unwrap_or_default();
                for item in &jobs {
                    if !item.is_object()
                        || item.get("head_sha").and_then(|value| value.as_str())
                            != Some(head.as_str())
                        || item.get("run_id").and_then(|value| value.as_i64()) != Some(run_id)
                        || item
                            .get("id")
                            .and_then(|value| value.as_i64())
                            .map(|id| id < 1)
                            .unwrap_or(true)
                    {
                        return Err(PublicationError::Code(
                            "hosted_job_identity_changed".to_string(),
                        ));
                    }
                }
                let matching: Vec<&Value> = jobs
                    .iter()
                    .filter(|item| {
                        item.get("name").and_then(|value| value.as_str())
                            == Some(check_name.as_str())
                    })
                    .collect();
                if matching.len() > 1 {
                    return Err(PublicationError::Code(
                        "duplicate_hosted_check_identity".to_string(),
                    ));
                }
                selected = matching.first().map(|item| (*item).clone());
                if let Some(item) = &selected {
                    if item.get("status").and_then(|value| value.as_str()) == Some("completed") {
                        let conclusion = item
                            .get("conclusion")
                            .and_then(|value| value.as_str())
                            .unwrap_or_default();
                        observed["status"] = json!(if conclusion == "success" {
                            "success"
                        } else if matches!(conclusion, "cancelled" | "skipped") {
                            conclusion
                        } else {
                            "failure"
                        });
                    }
                } else if run.get("status").and_then(|value| value.as_str()) == Some("completed") {
                    observed["status"] = json!("failure");
                }
                if invalid_workflow {
                    observed["status"] = json!("failure");
                }
                if validation_failure {
                    observed["status"] = json!("failure");
                }
            }
            observations.push(Value::Object(observed.clone()));
            evidence.push(json!({
                "name": observed["name"],
                "status": observed["status"],
                "head_sha": head,
                "run_id": run.as_ref().map(|run| run["id"].clone()).unwrap_or(Value::Null),
                "job_id": selected.as_ref().map(|item| item["id"].clone()).unwrap_or(Value::Null),
            }));
            if observed["status"].as_str() != Some("pending")
                && observed["status"].as_str() != Some("success")
            {
                let log = match &selected {
                    Some(item) => self.logs(&repository, item["id"].as_i64().unwrap_or(0)),
                    None => b"Required hosted job did not start.".to_vec(),
                };
                if let Some(version) = last_rustc_version(&log) {
                    toolchain = format!("rustc {version}");
                }
                let digest = format!("{:x}", Sha256::digest(&log));
                let tail = &log[log.len().saturating_sub(32768)..];
                let projection = project_verification(
                    &[vec!["hosted-check".to_string(), check_name.clone()]],
                    Some("verification_failed"),
                    1,
                    tail,
                    &digest,
                    true,
                    None,
                );
                let mut finding = projection
                    .findings
                    .first()
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                let finding_id = format!(
                    "hosted-{}",
                    &format!(
                        "{:x}",
                        Sha256::digest(
                            format!(
                                "{}:{}:{}",
                                path,
                                check_name,
                                finding["diagnostic_code"].as_str().unwrap_or_default()
                            )
                            .as_bytes()
                        )
                    )[..24]
                );
                if let Some(object) = finding.as_object_mut() {
                    object.insert("check_id".to_string(), json!(check_name));
                    object.insert("finding_id".to_string(), json!(finding_id));
                    object.insert(
                        "reproduction".to_string(),
                        json!(format!(
                            "Inspect required hosted check {workflow_name} / {check_name} on the bound PR head."
                        )),
                    );
                    if invalid_workflow || selected.is_none() {
                        object.insert(
                            "diagnostic_code".to_string(),
                            json!("required_workflow_identity"),
                        );
                        object.insert(
                            "summary".to_string(),
                            json!("The required hosted workflow/job is missing or changed."),
                        );
                        object.insert(
                            "actual".to_string(),
                            json!("The required hosted workflow/job is missing or changed."),
                        );
                    }
                    if validation_failure {
                        let summary = "GitHub rejected the required workflow before pull-request jobs could start.";
                        object.insert(
                            "diagnostic_code".to_string(),
                            json!("workflow_validation_failed"),
                        );
                        object.insert("path".to_string(), json!(path));
                        object.insert("summary".to_string(), json!(summary));
                        object.insert("actual".to_string(), json!(summary));
                        object.insert(
                            "reproduction".to_string(),
                            json!(format!(
                                "Validate {path} against GitHub Actions syntax and context rules. Run: https://github.com/{repository}/actions/runs/{}",
                                run.as_ref().map(|run| run["id"].to_string()).unwrap_or_default()
                            )),
                        );
                    }
                }
                findings.push(finding);
            }
        }
        let statuses: std::collections::BTreeSet<&str> = observations
            .iter()
            .filter_map(|item| item["status"].as_str())
            .collect();
        let status = if statuses
            .iter()
            .any(|status| *status != "pending" && *status != "success")
        {
            "failed"
        } else if statuses == ["success"].into_iter().collect() {
            "passed"
        } else {
            "pending"
        };
        let evidence_json =
            serde_json::to_string(&Value::Array(evidence.clone())).unwrap_or_default();
        let digest = format!("{:x}", Sha256::digest(evidence_json.as_bytes()));
        let summary = match status {
            "passed" => "Every required hosted check passed on the exact PR head.",
            "failed" => "Required hosted verification reported a failure.",
            _ => "Required hosted checks are still pending.",
        };
        // Recheck identity after the bounded observations. A
        // concurrent manual head change cannot be accepted as a
        // pass for this leased candidate.
        let pr = self
            .client
            .api("GET", &format!("repos/{repository}/pulls/{number}"), None)?;
        self.client.validated(&identity, &head, &pr)?;
        Ok(HostedCheckReport {
            status: status.to_string(),
            checks: observations,
            findings,
            summary: diagnostic_text(summary, forge_work_worker::diagnostic::MAX_DIAGNOSTIC_BYTES),
            toolchain,
            evidence_digest: digest,
        })
    }
}

fn validated_runs(data: &Value) -> Result<Vec<Value>, PublicationError> {
    let runs = data.get("workflow_runs").and_then(|value| value.as_array());
    let total = data
        .get("total_count")
        .and_then(|value| value.as_i64())
        .unwrap_or(0);
    match runs {
        Some(runs) if runs.len() <= 100 && total <= 100 => Ok(runs.clone()),
        _ => Err(PublicationError::Code(
            "hosted_run_history_unavailable".to_string(),
        )),
    }
}

impl HostedChecksPort for GitHubHostedChecksProvider {
    fn observe(&mut self, job: &Value) -> Result<HostedReportData, ReviewError> {
        let report = self.observe_inner(job)?;
        Ok(HostedReportData {
            status: report.status,
            checks: report.checks,
            findings: report.findings,
            summary: report.summary,
            toolchain: report.toolchain,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rustc_versions_scan_like_the_regex() {
        assert_eq!(
            last_rustc_version(b"noise rustc 1.85.0 more\nrustc 1.86.1\n"),
            Some("1.86.1".to_string())
        );
        assert_eq!(last_rustc_version(b"rustc1.2.3 xrustc 9.9.9"), None);
        assert_eq!(last_rustc_version(b"nothing"), None);
    }
}
