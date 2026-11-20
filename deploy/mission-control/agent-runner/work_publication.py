"""Trusted queued implementation-to-PR service, separate from native agents.

Constructors only select narrow providers. The factory opens configured local
resources on create; claimed jobs alone trigger verification and publication.
No method pushes main, force-pushes, merges a PR, invokes a model or rotates auth.
"""

from contextlib import contextmanager
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

LOG = logging.getLogger("forge-publication")


def validate_job(config, job):
    if not isinstance(job, dict):
        raise PublicationError("invalid_publication_lease")
    for name in ("publication_id", "task_id", "attempt_id", "lease_token", "plan_decision_id"):
        uuid(job[name])
    if (job["project_alias"] not in config.repositories or not SHA.fullmatch(job["base_revision"])
            or not DIGEST.fullmatch(job["workspace_digest"]) or job["status"] != "checking"):
        raise PublicationError("invalid_publication_lease")
    if any(job.get(name) is not None for name in ("branch", "commit_sha", "pr_url")):
        raise PublicationError("publication_requires_reconciliation")


class PublicationWorker:
    def __init__(self, config, store, snapshots, git, checks, pull_requests, *, stop=None):
        self.config, self.store, self.snapshots, self.git = config, store, snapshots, git
        self.checks, self.pull_requests = checks, pull_requests
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
                snapshot = self.snapshots.create(job)
                repository, head = self.git.prepare(job, snapshot)
                self.checks.run(job, repository, stop)
                self.git.verify(repository, head)
                require_lease()
                branch = branch_name(job)
                if not self.store.publication_finish(job, "publishing", "Isolated fixed checks passed.",
                                                     branch=branch, commit_sha=head):
                    raise PublicationError("publication_lease_lost")
                require_lease()
                existing_head = self.git.remote_head(job)
                if existing_head is not None and existing_head != head:
                    raise PublicationError("remote_branch_identity_changed")
                url = self.pull_requests.find(job, head)
                if url is None:
                    if existing_head is None:
                        require_lease()
                        self.git.push(job, repository, head)
                        if self.git.remote_head(job) != head:
                            raise PublicationError("remote_branch_identity_changed")
                    require_lease()
                    url = self.pull_requests.create(job, head)
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
                    if url and self.git.remote_head(job) in (None, head):
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
        job = self.store.publication_claim(self.config.worker.worker_id[:54] + "-publisher")
        if job is None:
            return False
        self.process(job)
        return True

    def run_forever(self):
        while not self.stop.is_set():
            try:
                if self.run_once():
                    continue
            except Exception:
                LOG.warning("Publication queue unavailable; no remote action replayed")
            self.stop.wait(5)


class PublicationWorkerFactory:
    def __init__(self, config, store_factory, *, workspace_factory=WorkspaceProvider,
                 snapshot_factory=WorkspaceSnapshotProvider, git_factory=GitRepositoryProvider,
                 supervisor_factory=SystemdSupervisor, checks_factory=IsolatedCheckRunner,
                 pull_request_factory=GitHubPullRequestClient, stop=None):
        if not isinstance(config, PublicationConfig) or not callable(store_factory):
            raise TypeError("Publication requires frozen config and a store factory")
        self.config, self.store_factory, self.stop = config, store_factory, stop
        self.workspace_factory, self.snapshot_factory, self.git_factory = workspace_factory, snapshot_factory, git_factory
        self.supervisor_factory, self.checks_factory = supervisor_factory, checks_factory
        self.pull_request_factory = pull_request_factory

    def create(self):
        self.config.state_root.mkdir(mode=0o700, parents=True, exist_ok=True)
        if self.config.state_root.is_symlink() or self.config.state_root.stat().st_mode & 0o077:
            raise PublicationError("publication_state_unavailable")
        workspaces = self.workspace_factory(self.config.worker)
        workspaces.prepare()
        supervisor = self.supervisor_factory(self.config.worker)
        return PublicationWorker(self.config, self.store_factory(self.config), self.snapshot_factory(workspaces),
                                 self.git_factory(self.config), self.checks_factory(self.config, supervisor),
                                 self.pull_request_factory(self.config), stop=self.stop)


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
