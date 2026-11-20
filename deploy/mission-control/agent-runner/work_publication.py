"""Trusted queued implementation-to-PR service, separate from native agents.

Constructors only select narrow providers. The factory opens configured local
resources on create; claimed jobs alone trigger verification and publication.
No method pushes main, force-pushes, merges a PR, invokes a model or rotates auth.
"""

from contextlib import contextmanager, nullcontext
import fcntl
import logging
import os
from pathlib import Path
import signal
import threading

from work_executor import SystemdSupervisor, WorkspaceProvider
from work_publication_checks import IsolatedCheckRunner
from work_publication_clients import (DIGEST, SHA, GitHubPullRequestClient, GitRepositoryProvider,
                                      PublicationError, WorkspaceSnapshotProvider, branch_name, uuid)
from work_publication_config import CONFIG_ENV, PublicationConfig, load_config
from work_review import ReviewedCandidateProvider, ReviewWorker
from work_consumer_cache import configured_consumer_caches
from work_hosted_checks import GitHubHostedChecksProvider

LOG = logging.getLogger("forge-publication")


def validate_job(config, job):
    if not isinstance(job, dict):
        raise PublicationError("invalid_publication_lease")
    for name in ("publication_id", "task_id", "attempt_id", "lease_token", "review_id"):
        uuid(job[name])
    if job.get("plan_decision_id") is not None:
        uuid(job["plan_decision_id"])
    elif not ((job.get("parent_task_id") and job.get("feedback_id"))
              or job.get("direct_implementation") is True):
        raise PublicationError("publication_scope_not_authorized")
    if (job["project_alias"] not in config.repositories or not SHA.fullmatch(job["base_revision"])
            or not DIGEST.fullmatch(job["workspace_digest"]) or job["status"] != "checking"
            or not SHA.fullmatch(job.get("candidate_commit", ""))
            or not DIGEST.fullmatch(job.get("gate_manifest_sha256", ""))):
        raise PublicationError("invalid_publication_lease")
    if job.get("target_publication_id") is not None:
        uuid(job["target_publication_id"])
    if job.get("expected_remote_head") is not None and not SHA.fullmatch(job["expected_remote_head"]):
        raise PublicationError("invalid_publication_target")
    if any(job.get(name) is not None for name in ("branch", "commit_sha", "pr_url")):
        raise PublicationError("publication_requires_reconciliation")


class PublicationWorker:
    def __init__(self, config, store, snapshots, git, checks, pull_requests, *, candidates, reviewer=None, stop=None):
        self.config, self.store, self.snapshots, self.git = config, store, snapshots, git
        self.checks, self.pull_requests = checks, pull_requests
        self.candidates, self.reviewer = candidates, reviewer
        self.stop = stop if stop is not None else threading.Event()

    @contextmanager
    def _locked(self, job):
        path = self.config.state_root / (uuid(job["publication_id"]) + ".lock")
        descriptor = os.open(path, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
        with os.fdopen(descriptor, "r+b") as stream:
            try:
                fcntl.flock(stream.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                raise PublicationError("publication_already_owned") from None
            try:
                yield
            finally:
                fcntl.flock(stream.fileno(), fcntl.LOCK_UN)

    def process(self, job):
        validate_job(self.config, job)
        done, stop = threading.Event(), threading.Event()
        branch, head = None, None

        def require_lease():
            if self.stop.is_set() or stop.is_set() or not self.store.publication_renew(job):
                stop.set()
                raise PublicationError("publication_lease_lost")

        def renew():
            while not done.is_set():
                try:
                    require_lease()
                except Exception:
                    stop.set()
                    return
                done.wait(10)

        heartbeat = threading.Thread(target=renew, daemon=True, name="forge-publication-lease")
        heartbeat.start()
        try:
            with self._locked(job):
                require_lease()
                self.checks.quiesce(job)
                # The independently reviewed exact candidate is immutable. A
                # later main merge would invalidate its verification identity.
                repository, head = self.candidates.acquire(job)
                if head != job["candidate_commit"]:
                    raise PublicationError("review_candidate_changed")
                self.git.verify(repository, head)
                require_lease()
                branch = branch_name(job)
                if not self.store.publication_finish(job, "publishing", "Isolated fixed checks passed.",
                                                     branch=branch, commit_sha=head):
                    raise PublicationError("publication_lease_lost")
                require_lease()
                existing_head = self.git.remote_head(job)
                previous = job.get("expected_remote_head")
                if existing_head not in ({None, head} if previous is None else {previous, head}):
                    raise PublicationError("remote_branch_identity_changed")
                prior_url = self.pull_requests.find(job, previous) if previous and existing_head == previous else None
                url = self.pull_requests.find(job, head) if existing_head == head or previous is None else None
                if url is not None and existing_head != head:
                    raise PublicationError("remote_branch_identity_changed")
                if url is None:
                    if existing_head != head:
                        if previous is not None:
                            self.git._git(repository, "merge-base", "--is-ancestor", previous, head)
                        require_lease()
                        self.git.push(job, repository, head)
                        if self.git.remote_head(job) != head:
                            raise PublicationError("remote_branch_identity_changed")
                    require_lease()
                    url = self.pull_requests.find(job, head) if prior_url else self.pull_requests.create(job, head)
                    if url is None:
                        raise PublicationError("pull_request_update_unconfirmed")
                require_lease()
                if not self.store.publication_finish(job, "published", "Verified implementation PR is ready for review.",
                                                     branch=branch, commit_sha=head, pr_url=url):
                    raise PublicationError("publication_lease_lost")
        except Exception as error:
            # A lost push/PR acknowledgement triggers read-only reconciliation,
            # never an automatic second push/create. Keep the known head receipt.
            if branch is not None and head is not None and not stop.is_set():
                try:
                    require_lease()
                    url = self.pull_requests.find(job, head)
                    if url and self.git.remote_head(job) == head:
                        self.store.publication_finish(job, "published", "Existing matching PR reconciled after an uncertain acknowledgement.",
                                                      branch=branch, commit_sha=head, pr_url=url)
                        return
                except Exception:
                    pass
            code = error.code if isinstance(error, PublicationError) else "publication_failed"
            try:
                self.store.publication_finish(job, "failed", "Publication stopped: " + code + ". Review before retrying.",
                                              branch=branch, commit_sha=head)
            except Exception:
                LOG.warning("Publication receipt unavailable; remote actions will not be replayed")
        finally:
            done.set()
            heartbeat.join(timeout=12)

    def run_once(self):
        if self.stop.is_set():
            return False
        if self.reviewer is not None:
            review = self.store.review_claim(self.config.worker.worker_id[:54] + "-reviewer")
            if review is not None:
                self.reviewer.process(review)
                return True
        job = self.store.publication_claim(self.config.worker.worker_id[:54] + "-publisher")
        if job is None:
            return False
        self.process(job)
        return True

    def run_forever(self):
        while not self.stop.is_set():
            try:
                listener = self.store.listen() if hasattr(self.store, "listen") else nullcontext(None)
                with listener as committed:
                    # Subscribe before scanning. Notifications wake this owner;
                    # leases and source-bound rows still authorize every action.
                    while not self.stop.is_set():
                        if self.run_once():
                            continue
                        if committed is not None:
                            self.store.wait(committed, 5)
                        else:
                            self.stop.wait(5)
            except Exception:
                LOG.warning("Publication queue unavailable; no remote action replayed")
            self.stop.wait(5)


class PublicationWorkerFactory:
    def __init__(self, config, store_factory, *, workspace_factory=WorkspaceProvider,
                 snapshot_factory=WorkspaceSnapshotProvider, git_factory=GitRepositoryProvider,
                 supervisor_factory=None, checks_factory=IsolatedCheckRunner,
                 pull_request_factory=GitHubPullRequestClient, candidate_factory=ReviewedCandidateProvider,
                 reviewer_factory=ReviewWorker, hosted_factory=GitHubHostedChecksProvider, stop=None):
        if not isinstance(config, PublicationConfig) or not callable(store_factory):
            raise TypeError("Publication requires frozen config and a store factory")
        self.config, self.store_factory, self.stop = config, store_factory, stop
        self.workspace_factory, self.snapshot_factory, self.git_factory = workspace_factory, snapshot_factory, git_factory
        self.supervisor_factory = (supervisor_factory if supervisor_factory is not None else
                                   lambda config: SystemdSupervisor(config,
                                       consumer_cache_provider=configured_consumer_caches(config)))
        self.checks_factory = checks_factory
        self.pull_request_factory = pull_request_factory
        self.candidate_factory, self.reviewer_factory = candidate_factory, reviewer_factory
        self.hosted_factory = hosted_factory

    def create(self):
        self.config.state_root.mkdir(mode=0o700, parents=True, exist_ok=True)
        if self.config.state_root.is_symlink() or self.config.state_root.stat().st_mode & 0o077:
            raise PublicationError("publication_state_unavailable")
        workspaces = self.workspace_factory(self.config.worker)
        workspaces.prepare()
        supervisor = self.supervisor_factory(self.config.worker)
        store, snapshots = self.store_factory(self.config), self.snapshot_factory(workspaces)
        git, checks = self.git_factory(self.config), self.checks_factory(self.config, supervisor)
        candidates = self.candidate_factory(self.config, git)
        pulls = self.pull_request_factory(self.config)
        hosted = self.hosted_factory(self.config, client=pulls)
        reviewer = self.reviewer_factory(self.config, store, snapshots, git, checks, candidates,
                                        stop=self.stop, hosted_checks=hosted)
        return PublicationWorker(self.config, store, snapshots, git, checks,
                                 pulls, candidates=candidates,
                                 reviewer=reviewer, stop=self.stop)


def default_store_factory(config):
    import psycopg2
    from work_worker_store import WorkerDB

    connection = lambda: psycopg2.connect(**config.worker.database.connection_arguments(reconciler=True))
    return WorkerDB(connection, config.worker.owner, reconcile_connect_factory=connection)


def main():
    logging.basicConfig(level=logging.INFO)
    stop = threading.Event()
    for signum in (signal.SIGTERM, signal.SIGINT):
        signal.signal(signum, lambda _signum, _frame: stop.set())
    PublicationWorkerFactory(load_config(os.environ[CONFIG_ENV]), default_store_factory, stop=stop).create().run_forever()


if __name__ == "__main__":
    main()
