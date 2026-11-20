"""Read-only, exact-head GitHub Actions observations for an admitted review.

Only repository APIs are used. This provider never reruns workflows, invokes a
model, polls account allowance endpoints, selects another account or merges.
"""

from dataclasses import dataclass
import hashlib
import json
import re
from urllib.parse import urlencode

from work_publication_clients import (GitHubPullRequestClient, PublicationError, SHA,
                                      command, parent_environment, uuid)
from work_verification import diagnostic_text, project_verification


@dataclass(frozen=True)
class HostedCheckReport:
    status: str
    checks: tuple
    findings: tuple
    summary: str
    toolchain: str
    evidence_digest: str


class GitHubHostedChecksProvider:
    def __init__(self, config, *, client=None, execute=command):
        self.config, self.execute = config, execute
        self.client = client if client is not None else GitHubPullRequestClient(config, execute=execute)

    def _logs(self, repository, job_id):
        try:
            return self.execute(("/usr/bin/gh", "api", "--hostname", "github.com", "--method", "GET",
                "--header", "Accept: application/vnd.github+json",
                f"repos/{repository}/actions/jobs/{job_id}/logs"), env=parent_environment(self.config))
        except PublicationError:
            # Failure metadata still blocks acceptance. Unavailable logs do not
            # authorize a rerun or turn a missing diagnostic into success.
            return b"Hosted check failed; its bounded diagnostic log is unavailable."

    def observe(self, job):
        head = job.get("candidate_commit")
        if not isinstance(head, str) or not SHA.fullmatch(head):
            raise PublicationError("invalid_hosted_review_head")
        repository = self.config.repositories[job["project_alias"]]
        target = uuid(job["target_publication_id"])
        url = job.get("pr_url")
        prefix = "https://github.com/" + repository + "/pull/"
        if not isinstance(url, str) or not url.startswith(prefix):
            raise PublicationError("invalid_hosted_review_target")
        number = url.removeprefix(prefix)
        if not number.isdigit() or int(number) < 1 or str(int(number)) != number:
            raise PublicationError("invalid_hosted_review_target")
        identity = {**job, "publication_id": target, "target_publication_id": target}
        pr = self.client._api("GET", f"repos/{repository}/pulls/{number}")
        self.client._validated(identity, head, pr)
        if pr.get("state") != "open" or pr.get("merged_at") is not None:
            raise PublicationError("hosted_review_target_closed")
        query = urlencode({"head_sha": head, "event": "pull_request", "per_page": 100})
        data = self.client._api("GET", f"repos/{repository}/actions/runs?{query}")
        if (not isinstance(data, dict) or not isinstance(data.get("workflow_runs"), list)
                or len(data["workflow_runs"]) > 100 or data.get("total_count", 0) > 100):
            raise PublicationError("hosted_run_history_unavailable")
        runs = data["workflow_runs"]
        for run in runs:
            if (not isinstance(run, dict) or type(run.get("id")) is not int or run["id"] < 1
                    or run.get("head_sha") != head or run.get("event") != "pull_request"
                    or run.get("repository", {}).get("full_name") != repository):
                raise PublicationError("hosted_run_identity_changed")
        by_workflow = {}
        for path, _name, _check in self.config.required_hosted_checks:
            matching = [run for run in runs if run.get("path") == path]
            by_workflow[path] = max(matching, key=lambda run: (run["id"], run.get("run_attempt", 1))) if matching else None
        jobs_by_run = {}
        validation_failures = {}
        missing = {path for path, run in by_workflow.items() if run is None}
        if missing:
            # Invalid workflow files can fail on PUSH before GitHub can create
            # their pull-request jobs. Such a run is diagnostic failure only;
            # a successful PUSH never satisfies required PR verification.
            query = urlencode({"head_sha": head, "per_page": 100})
            extra = self.client._api("GET", f"repos/{repository}/actions/runs?{query}")
            if (not isinstance(extra, dict) or not isinstance(extra.get("workflow_runs"), list)
                    or len(extra["workflow_runs"]) > 100 or extra.get("total_count", 0) > 100):
                raise PublicationError("hosted_run_history_unavailable")
            for path in missing:
                matches = [run for run in extra["workflow_runs"] if isinstance(run, dict)
                    and run.get("path") == path and run.get("event") == "push"
                    and run.get("head_sha") == head and run.get("repository", {}).get("full_name") == repository
                    and run.get("status") == "completed" and run.get("conclusion") == "failure"
                    and type(run.get("id")) is int and run["id"] > 0]
                if not matches:
                    continue
                failed = max(matches, key=lambda run: (run["id"], run.get("run_attempt", 1)))
                payload = self.client._api("GET", f"repos/{repository}/actions/runs/{failed['id']}/jobs?filter=latest&per_page=100")
                if isinstance(payload, dict) and payload.get("jobs") == [] and payload.get("total_count", 0) == 0:
                    validation_failures[path] = failed
                    jobs_by_run[failed["id"]] = []
        observations, findings, evidence = [], [], []
        toolchain = "Not reported"
        for path, workflow_name, check_name in self.config.required_hosted_checks:
            validation_failure = path in validation_failures
            run = by_workflow[path] or validation_failures.get(path)
            observed = {"name": check_name, "status": "pending", "head_sha": head}
            selected = None
            invalid_workflow = False
            if run is not None:
                if run.get("name") != workflow_name:
                    invalid_workflow = True
                if run["id"] not in jobs_by_run:
                    payload = self.client._api("GET", f"repos/{repository}/actions/runs/{run['id']}/jobs?filter=latest&per_page=100")
                    if (not isinstance(payload, dict) or not isinstance(payload.get("jobs"), list)
                            or len(payload["jobs"]) > 100 or payload.get("total_count", 0) > 100):
                        raise PublicationError("hosted_job_history_unavailable")
                    jobs_by_run[run["id"]] = payload["jobs"]
                jobs = jobs_by_run[run["id"]]
                for item in jobs:
                    if (not isinstance(item, dict) or item.get("head_sha") != head
                            or item.get("run_id") != run["id"]
                            or type(item.get("id")) is not int or item["id"] < 1):
                        raise PublicationError("hosted_job_identity_changed")
                matching = [item for item in jobs if item.get("name") == check_name]
                if len(matching) > 1:
                    raise PublicationError("duplicate_hosted_check_identity")
                selected = matching[0] if matching else None
                if selected is not None and selected.get("status") == "completed":
                    conclusion = selected.get("conclusion")
                    observed["status"] = ("success" if conclusion == "success" else
                                           conclusion if conclusion in {"cancelled", "skipped"} else "failure")
                elif selected is None and run.get("status") == "completed":
                    observed["status"] = "failure"
                if invalid_workflow:
                    observed["status"] = "failure"
                if validation_failure:
                    observed["status"] = "failure"
            observations.append(observed)
            evidence.append({**observed, "run_id": run["id"] if run else None,
                             "job_id": selected["id"] if selected else None})
            if observed["status"] not in {"pending", "success"}:
                log = self._logs(repository, selected["id"]) if selected is not None else b"Required hosted job did not start."
                versions = re.findall(rb"\brustc\s+(\d+\.\d+\.\d+)", log)
                if versions:
                    toolchain = "rustc " + versions[-1].decode("ascii")
                digest = hashlib.sha256(log).hexdigest()
                projection = project_verification((("hosted-check", check_name),), code="verification_failed",
                    returncode=1, output=log[-32768:], evidence_digest=digest, cleanup_confirmed=True)
                finding = dict(projection.findings[0])
                finding.update(check_id=check_name,
                    finding_id="hosted-" + hashlib.sha256((path + ":" + check_name + ":" + finding["diagnostic_code"]).encode()).hexdigest()[:24],
                    reproduction="Inspect required hosted check " + workflow_name + " / " + check_name + " on the bound PR head.")
                if invalid_workflow or selected is None:
                    finding.update(diagnostic_code="required_workflow_identity", summary="The required hosted workflow/job is missing or changed.",
                                   actual="The required hosted workflow/job is missing or changed.")
                if validation_failure:
                    summary = "GitHub rejected the required workflow before pull-request jobs could start."
                    finding.update(diagnostic_code="workflow_validation_failed", path=path,
                        summary=summary, actual=summary,
                        reproduction="Validate " + path + " against GitHub Actions syntax and context rules. "
                            + f"Run: https://github.com/{repository}/actions/runs/{run['id']}")
                findings.append(finding)
        statuses = {item["status"] for item in observations}
        status = "failed" if statuses - {"pending", "success"} else "passed" if statuses == {"success"} else "pending"
        digest = hashlib.sha256(json.dumps(evidence, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
        summary = {"passed": "Every required hosted check passed on the exact PR head.",
                   "failed": "Required hosted verification reported a failure.",
                   "pending": "Required hosted checks are still pending."}[status]
        # Recheck identity after the bounded observations. A concurrent manual
        # head change cannot be accepted as a pass for this leased candidate.
        self.client._validated(identity, head,
            self.client._api("GET", f"repos/{repository}/pulls/{number}"))
        return HostedCheckReport(status, tuple(observations), tuple(findings), diagnostic_text(summary), toolchain, digest)
