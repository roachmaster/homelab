"""Trusted queued implementation-to-PR service, separate from native agents.

Constructors only select narrow providers. The factory opens configured local
resources on create; claimed jobs alone trigger verification and publication.
The optional source-selected merger updates main only from an independently
approved exact-tree intent. No native agent receives publication authority.
"""

from contextlib import contextmanager, nullcontext
import fcntl
import json
import logging
import os
from pathlib import Path
import signal
import threading

import jenkins_api
import jenkins_config
from jenkins_emitter import InProcessJenkinsEmitter
from work_executor import SystemdSupervisor, WorkspaceProvider
from work_external_delivery import DeliveryConfig, read_private
from work_jenkins_lifecycle import DRAIN_SHUTDOWN_TIMEOUT_SECONDS, IntentDrain
from work_publication_checks import IsolatedCheckRunner
from work_publication_clients import (DIGEST, SHA, GitHubPullRequestClient, GitRepositoryProvider,
                                      PublicationError, WorkspaceSnapshotProvider, branch_name, uuid)
from work_publication_config import CONFIG_ENV, PublicationConfig, load_config
from work_review import ReviewedCandidateProvider, ReviewWorker
from work_consumer_cache import configured_consumer_caches
from work_hosted_checks import GitHubHostedChecksProvider
from work_merge import GitHubExactTreeMergeProvider, MergeWorker
from work_source_revision import VerifiedSourceSyncProvider
from work_native_jenkins import create_native_jenkins
from work_native_repair import BranchFirstRepairProvider
from work_post_merge import PostMergeWorker
from work_post_merge_provider import PostMergeProviderFactory
from work_delivery_ops import SourceObserverFactory, ReleaseProviderFactory
from work_delivery_worker import DeliveryVerificationStore, ReleaseWorker

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
    def __init__(self, config, store, snapshots, git, checks, pull_requests, *, candidates, reviewer=None, merger=None, stop=None,
                 native_ci=None, post_merge=None, source_observer=None, release=None, jenkins_drain=None):
        self.config, self.store, self.snapshots, self.git = config, store, snapshots, git
        self.checks, self.pull_requests = checks, pull_requests
        self.candidates, self.reviewer = candidates, reviewer
        self.merger = merger
        self.native_ci = native_ci
        self.post_merge = post_merge
        self.jenkins_drain = jenkins_drain
        self.automation = tuple(port for port in (post_merge, source_observer, release) if port is not None)
        self._automation_index = 0
        self._yield_post_merge = False
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
                if self.native_ci is not None:
                    job = self.native_ci.publication_job(job)
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
        if self.native_ci is not None:
            self.native_ci.reconcile_once()
        if self.automation:
            # A slow observation can outlast the DB polling delay. Always give
            # review/merge/publication one turn before claiming more main CI.
            if self._yield_post_merge:
                self._yield_post_merge = False
            else:
                # A rotating start gives observation and activation a turn even
                # while Jenkins has a long-running main verification.
                for _ in self.automation:
                    port = self.automation[self._automation_index % len(self.automation)]
                    self._automation_index += 1
                    if port.run_once():
                        self._yield_post_merge = True
                        return True
        if self.reviewer is not None:
            review = self.store.review_claim(self.config.worker.worker_id[:54] + "-reviewer")
            if review is not None:
                self.reviewer.process(review)
                return True
        if self.merger is not None:
            merge = self.store.merge_claim(self.config.worker.worker_id[:54] + "-merger")
            if merge is not None:
                self.merger.process(merge)
                return True
        job = self.store.publication_claim(self.config.worker.worker_id[:54] + "-publisher")
        if job is None:
            return False
        self.process(job)
        return True

    def run_forever(self):
        # The intent drain runs on its own thread, concurrently with the
        # verification loop: no run_once pass happens during a verification,
        # so a run_once hook could never deliver build_started live.
        if self.jenkins_drain is not None:
            self.jenkins_drain.start(self.stop)
        try:
            while not self.stop.is_set():
                try:
                    listener = self.store.listen() if hasattr(self.store, "listen") else nullcontext(None)
                    with listener as committed:
                        ci_listener = (self.native_ci.listen_scope(lambda: self.store.wait(committed, 0))
                            if self.native_ci is not None and committed is not None else nullcontext())
                        with ci_listener:
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
        finally:
            if self.jenkins_drain is not None:
                # Bounded shutdown: finish the in-flight emit (bounded by the
                # 4s/5s transaction bounds), start no new emission, exit. If
                # the join times out the daemon thread is abandoned and the
                # process exits; a stuck drain cannot delay the service stop
                # past its bound (well inside TimeoutStopSec=45).
                self.jenkins_drain.join(timeout=DRAIN_SHUTDOWN_TIMEOUT_SECONDS)


class PublicationWorkerFactory:
    def __init__(self, config, store_factory, *, workspace_factory=WorkspaceProvider,
                 snapshot_factory=WorkspaceSnapshotProvider, git_factory=GitRepositoryProvider,
                 supervisor_factory=None, checks_factory=IsolatedCheckRunner,
                 pull_request_factory=GitHubPullRequestClient, candidate_factory=ReviewedCandidateProvider,
                 reviewer_factory=ReviewWorker, hosted_factory=None,
                 merge_provider_factory=GitHubExactTreeMergeProvider, merge_worker_factory=MergeWorker,
                 source_sync_factory=VerifiedSourceSyncProvider,
                 repair_source_factory=BranchFirstRepairProvider,
                 native_jenkins_factory=create_native_jenkins,
                 post_merge_provider_factory=PostMergeProviderFactory,
                 post_merge_worker_factory=PostMergeWorker,
                 source_observer_factory=SourceObserverFactory,
                 release_provider_factory=ReleaseProviderFactory,
                 release_worker_factory=ReleaseWorker, stop=None):
        if not isinstance(config, PublicationConfig) or not callable(store_factory):
            raise TypeError("Publication requires frozen config and a store factory")
        if (type(config.merge_enabled) is not bool or config.merge_method != "merge"
                or config.merge_enabled and not config.repositories.keys() & config.worker.autonomous_projects):
            raise ValueError("Trusted merge requires explicit verified project source selection")
        if (type(config.post_merge_enabled) is not bool or config.post_merge_enabled and
                (not config.merge_enabled or config.hosted_provider != 'jenkins'
                 or config.post_merge_jenkins_delivery_config is None)):
            raise ValueError('Post-merge verification requires explicit trusted Jenkins selection')
        self.config, self.store_factory, self.stop = config, store_factory, stop
        self.workspace_factory, self.snapshot_factory, self.git_factory = workspace_factory, snapshot_factory, git_factory
        self.supervisor_factory = (supervisor_factory if supervisor_factory is not None else
                                   lambda config: SystemdSupervisor(config,
                                       consumer_cache_provider=configured_consumer_caches(config)))
        self.checks_factory = checks_factory
        self.pull_request_factory = pull_request_factory
        self.candidate_factory, self.reviewer_factory = candidate_factory, reviewer_factory
        self.hosted_factory = hosted_factory
        self.native_jenkins_factory = native_jenkins_factory
        self.post_merge_provider_factory = post_merge_provider_factory
        self.post_merge_worker_factory = post_merge_worker_factory
        if (config.delivery_enabled and (not config.post_merge_enabled
                or config.jenkins_verification_profile not in {'forge-sdlc-jenkins-v2', 'forge-sdlc-jenkins-v3'})
                or (config.release_enabled or config.source_observation_enabled) and not config.delivery_enabled):
            raise ValueError('Delivery capabilities require the unified scoped Jenkins consumer')
        if (any(type(value) is not bool for value in (config.delivery_enabled,
                    config.source_observation_enabled, config.release_enabled))
                or type(config.source_observation_interval_seconds) is not int
                or config.source_observation_interval_seconds != 60
                or config.release_enabled and (set(config.repositories) != {'homelab'}
                    or not isinstance(config.release_actor_sha256, str)
                    or DIGEST.fullmatch(config.release_actor_sha256) is None)
                or not config.release_enabled and config.release_actor_sha256 is not None):
            raise ValueError('Invalid frozen delivery capability configuration')
        self.source_observer_factory = source_observer_factory
        self.release_provider_factory, self.release_worker_factory = release_provider_factory, release_worker_factory
        self.merge_provider_factory, self.merge_worker_factory = merge_provider_factory, merge_worker_factory
        self.source_sync_factory = source_sync_factory
        self.repair_source_factory = repair_source_factory

    def _jenkins_events(self):
        """Build the validated emit-only Jenkins services for the lifecycle producer.

        Validates the section against the deployment's JenkinsConfig at
        construction time and refuses to start on a mismatch -- an
        unauthorized instance or job is a startup failure, not a runtime
        surprise. Returns (verification emitter, event lookup, instance
        alias, drain emitter). The two services share only a connect
        factory; each transaction opens its own connection, so no
        connection is ever shared between the poll loop and the drain.
        """
        import psycopg2

        section = self.config.jenkins_events
        selected = jenkins_config.load_jenkins_config(str(section.config_path))
        if selected is None:
            raise ValueError("Jenkins events configuration is unavailable")
        if selected.owner != self.config.worker.owner:
            raise ValueError("Jenkins events owner does not match publication owner")
        if section.instance not in selected.allowed_instances:
            raise ValueError("Jenkins events instance is not allowed")
        for alias in self.config.repositories:
            if alias not in selected.allowed_projects:
                raise ValueError("Jenkins events project is not allowed")
        delivery = DeliveryConfig.parse(json.loads(read_private(self.config.jenkins_delivery_config)))
        for alias in self.config.repositories:
            if not selected.is_job_allowed(alias, delivery.jenkins_job):
                raise ValueError("Jenkins events job is not allowed")
        connect = lambda: psycopg2.connect(**section.database.connection_arguments())
        verification = jenkins_api.create_jenkins_service(selected, connect, with_consumers=False)
        drain = jenkins_api.create_jenkins_service(selected, connect, with_consumers=False)

        def event_lookup(project_alias, job_name, commit_sha):
            result = verification.list_events(jenkins_api.JenkinsEventsList(
                project_alias=project_alias, job_name=job_name, commit_sha=commit_sha, limit=100))
            return result.get("events", [])

        return (InProcessJenkinsEmitter(verification), event_lookup, section.instance,
                InProcessJenkinsEmitter(drain))

    def create(self):
        self.config.state_root.mkdir(mode=0o700, parents=True, exist_ok=True)
        if self.config.state_root.is_symlink() or self.config.state_root.stat().st_mode & 0o077:
            raise PublicationError("publication_state_unavailable")
        workspaces = self.workspace_factory(self.config.worker)
        workspaces.prepare()
        supervisor = self.supervisor_factory(self.config.worker)
        store, snapshots = self.store_factory(self.config), self.snapshot_factory(workspaces)
        git = self.git_factory(self.config)
        candidates = self.candidate_factory(self.config, git)
        pulls = self.pull_request_factory(self.config)
        native_ci = None
        jenkins_drain = None
        if self.hosted_factory is None and self.config.hosted_provider == 'jenkins':
            emitter = event_lookup = jenkins_instance = drain_emitter = None
            if self.config.jenkins_events is not None:
                emitter, event_lookup, jenkins_instance, drain_emitter = self._jenkins_events()
            native_ci = self.native_jenkins_factory(self.config, store, git, candidates, supervisor, client=pulls, stop=self.stop,
                                                    emitter=emitter, event_lookup=event_lookup, jenkins_instance=jenkins_instance)
            if drain_emitter is not None:
                jenkins_drain = IntentDrain(native_ci.delivery.config.state_root / 'actions', drain_emitter)
            checks = hosted = native_ci
        else:
            checks = self.checks_factory(self.config, supervisor)
            hosted = (self.hosted_factory or GitHubHostedChecksProvider)(self.config, client=pulls)
        reviewer_options = {'stop': self.stop, 'hosted_checks': hosted}
        repair_sources = None
        if native_ci is not None:
            reviewer_options['native_ci'] = native_ci
            if getattr(self.config, 'native_delivery_mode', 'integrate-first-v1') == 'branch-first-v1':
                repair_sources = self.repair_source_factory(self.config, git, candidates, native_ci)
                reviewer_options['repair_sources'] = repair_sources
        reviewer = self.reviewer_factory(self.config, store, snapshots, git, checks, candidates, **reviewer_options)
        merge_options = {'stop': self.stop}
        if native_ci is not None:
            merge_options['native_ci'] = native_ci
        if repair_sources is not None:
            merge_options['repair_sources'] = repair_sources
        merger = (self.merge_worker_factory(self.config, store,
                    self.merge_provider_factory(self.config, pulls, git, candidates, hosted),
                    source_sync=self.source_sync_factory(self.config, git), **merge_options)
                  if self.config.merge_enabled else None)
        post_merge = source_observer = release = None
        if self.config.post_merge_enabled:
            available = store.delivery_available if self.config.delivery_enabled else store.post_merge_available
            if not available(self.config.worker.owner):
                raise ValueError('Post-merge schema and scoped worker grant must be provisioned')
            provider = self.post_merge_provider_factory(self.config).create()
            verification_store = DeliveryVerificationStore(store) if self.config.delivery_enabled else store
            post_merge = self.post_merge_worker_factory(self.config, verification_store, provider, stop=self.stop)
        if self.config.source_observation_enabled:
            source_observer = self.source_observer_factory(self.config, store, delivery=provider.delivery).create()
        if self.config.release_enabled:
            release = self.release_worker_factory(self.config, store,
                self.release_provider_factory(self.config).create(), stop=self.stop)
        return PublicationWorker(self.config, store, snapshots, git, checks,
                                 pulls, candidates=candidates,
                                 reviewer=reviewer, merger=merger, stop=self.stop, native_ci=native_ci,
                                 post_merge=post_merge, source_observer=source_observer, release=release,
                                 jenkins_drain=jenkins_drain)


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
