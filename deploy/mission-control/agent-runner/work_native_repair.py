"""Parent-owned, explicitly unapproved Git sources for bounded delivery repair."""
import hashlib
import json
from uuid import UUID, uuid5

from work_executor import WorkspaceLease
from work_external_delivery import canonical, durable_json, read_private, sha
from work_publication_clients import PublicationError
from work_review import gate_source_digest
from work_ci_contract import LEGACY_PROFILE
from work_verification import diagnostic_text, gate_manifest_sha256


class BranchFirstRepairProvider:
    """Preserve tested sources and expose actual integration conflicts to authors.

    A repair seed is never a passed candidate or a main update. Its Git parents
    identify the tested source and observed main, while its tree preserves the
    real merge's nonconflicting files and unresolved conflict markers.
    """
    def __init__(self, config, git, candidates, native_ci):
        self.config, self.git, self.candidates, self.native_ci = config, git, candidates, native_ci

    def _seed(self, job, source, head, main, directory, require_lease):
        require_lease()
        self.git.verify(source, head)
        repository = directory / 'repository'
        intent = {'version': 1, 'tested_candidate_commit': head, 'current_main_commit': main}
        marker = directory / 'seed-intent.json'
        directory.mkdir(mode=0o700, parents=True, exist_ok=True)
        if directory.is_symlink() or repository.is_symlink():
            raise PublicationError('native_repair_source_changed')
        if marker.exists():
            if json.loads(read_private(marker)) != intent:
                raise PublicationError('native_repair_source_changed')
        else:
            durable_json(marker, intent)
        finished = directory / 'seed-result.json'
        if finished.exists():
            result = json.loads(read_private(finished))
            self.git.verify(repository, result['candidate_commit'])
            return repository, result
        if repository.exists():
            # Preserve a pre-receipt crash's private clone for audit. Nothing
            # from an incomplete seed may masquerade as an admitted receipt.
            interrupted = directory / ('repository.interrupted-' + str(uuid5(UUID(job['review_id']),
                sha(self.git._git(repository, 'rev-parse', 'HEAD').decode().strip()))))
            if interrupted.exists():
                raise PublicationError('native_repair_source_requires_reconcile')
            repository.rename(interrupted)
        self.git._git(None, 'clone', '--quiet', '--no-local', '--no-checkout', '--', str(source), str(repository))
        self.git._git(repository, 'checkout', '--quiet', '--detach', head)
        self.git._git(repository, 'fetch', '--quiet', '--no-tags', '--',
            self.git.remote_url_factory(job['project_alias']), main, network=True, timeout=120)
        if self.git._git(repository, 'rev-parse', 'FETCH_HEAD').decode().strip() != main:
            raise PublicationError('native_repair_main_changed')
        require_lease()
        conflicts = []
        try:
            self.git._git(repository, 'merge', '--no-edit', '--no-commit', '--no-ff', '--no-gpg-sign', main)
        except PublicationError:
            conflicts = [path.decode('utf-8') for path in self.git._git(repository,
                'diff', '--name-only', '--diff-filter=U', '-z').split(b'\0') if path]
            if not conflicts:
                raise PublicationError('native_repair_merge_unavailable') from None
        if len(conflicts) > 32 or any(len(path.encode()) > 1024 for path in conflicts):
            raise PublicationError('native_repair_conflicts_out_of_scope')
        # Commit the honest unresolved work-in-progress, not an assertion that
        # either side won. The model receives the markers and full main diff.
        self.git._git(repository, 'add', '--all', '--', '.')
        message = 'Unverified Forge delivery repair seed; resolve integration before review'
        try:
            self.git._git(repository, 'rev-parse', '--verify', 'MERGE_HEAD')
        except PublicationError:
            # Main may have raced to an ancestor already present in the tested
            # candidate. Git honestly has no new merge content in that case;
            # retain the unchanged tree and explicit source parents without
            # claiming the new commit has passed its required fresh gates.
            if main == head:
                raise PublicationError('native_repair_identical_source') from None
            tree = self.git._git(repository, 'write-tree').decode().strip()
            seed = self.git._git(repository, 'commit-tree', tree, '-p', head, '-p', main, '-m', message).decode().strip()
            self.git._git(repository, 'checkout', '--quiet', '--detach', seed)
        else:
            self.git._git(repository, 'commit', '--quiet', '--no-gpg-sign', '-m', message)
        candidate = sha(self.git._git(repository, 'rev-parse', 'HEAD').decode().strip())
        parents = self.git._git(repository, 'show', '-s', '--format=%P', candidate).decode().split()
        if parents != [head, main]:
            raise PublicationError('native_repair_parent_changed')
        self.git.verify(repository, candidate)
        source_gate = gate_source_digest(self.git, repository, candidate,
            profile=getattr(self.config, "jenkins_verification_profile", LEGACY_PROFILE))
        result = {'candidate_commit': candidate, 'integration_base': main,
            'candidate_workspace_digest': WorkspaceLease(repository,
                self.config.worker.projects[job['project_alias']], repository / '.git',
                job['task_id'], candidate).source_digest(),
            'gate_source_sha256': source_gate,
            'gate_manifest_sha256': gate_manifest_sha256(self.config.checks,
                self.config.required_hosted_checks, source_gate), 'conflict_paths': conflicts}
        require_lease()
        durable_json(finished, result)
        return repository, result

    def _details(self, head, main, build, conflicts):
        return {'version': 1, 'tested_candidate': head, 'current_main': main,
                'build': build, 'conflict_paths': conflicts}

    def finding(self, details, *, evidence_digest):
        main, build = details['current_main'], details['build']
        summary = ('Integrate the tested branch with current main and resolve the preserved Git conflicts. '
            'Jenkins build #' + str(build['build_number']) + ' (action ' + build['action_id'] + ').')
        return {'finding_id': 'delivery-integration-' + evidence_digest[:24], 'category': 'source',
            'check_id': 'main-integration', 'path': details['conflict_paths'][0] if details['conflict_paths'] else '',
            'diagnostic_code': 'main_ancestor_required', 'summary': summary,
            'reproduction': 'Use git diff ' + main + ' HEAD and inspect conflict markers; keep all current-main changes.',
            'expected': 'Resolve the source, then pass fresh Jenkins checks and independent code review.',
            'actual': diagnostic_text(canonical(details).decode()), 'evidence_digest': evidence_digest, 'status': 'open'}

    def prepare_local(self, job, repository, binding, build, require_lease):
        """The failed local review has never approved either tree."""
        head, main = sha(job['candidate_commit']), sha(job['integration_base'])
        directory = self.candidates.directory(job['review_id'])
        seed_directory = directory / 'delivery-repair'
        destination = directory / 'repository'
        tested = directory / 'repository.tested'
        install = directory / 'local-seed-install.json'
        if install.exists():
            result = json.loads(read_private(seed_directory / 'seed-result.json'))
            seed = seed_directory / 'repository'
        else:
            seed, result = self._seed(job, repository, head, main, seed_directory, require_lease)
        intent = {'version': 1, 'tested_candidate': head, 'candidate_commit': result['candidate_commit'],
                  'current_main': main}
        if install.exists():
            if json.loads(read_private(install)) != intent:
                raise PublicationError('native_repair_tested_source_changed')
        else:
            durable_json(install, intent)
        details = self._details(head, main, build, result['conflict_paths'])
        evidence = hashlib.sha256(canonical({'binding': binding, 'seed': result, 'details': details})).hexdigest()
        require_lease()
        if tested.is_symlink() or destination.is_symlink() or seed.is_symlink():
            raise PublicationError('native_repair_tested_source_changed')
        if not tested.exists():
            self.git.verify(destination, head)
            destination.rename(tested)
        self.git.verify(tested, head)
        if not destination.exists():
            self.git.verify(seed, result['candidate_commit'])
            seed.rename(destination)
        self.git.verify(destination, result['candidate_commit'])
        return {**result, 'repository': destination, 'delivery_failure': details, 'evidence_digest': evidence,
                'findings': [self.finding(details, evidence_digest=evidence)]}

    def prepare(self, job, current_main, require_lease):
        """Create a distinct source review after a previously approved merge fails."""
        main, head = sha(current_main), sha(job['candidate_commit'])
        original = self.candidates.read(job['review_id'])
        source, _ = self.candidates.acquire({**job, 'attempt_id': original['attempt_id'],
            'workspace_digest': original['workspace_digest']})
        build = self.native_ci.build_reference(job)
        review_id = str(uuid5(UUID(job['merge_id']), 'branch-first-repair-source-v1'))
        selected = {**job, 'review_id': review_id, 'attempt_id': original['attempt_id']}
        directory = self.candidates.directory(review_id)
        repository, result = self._seed(selected, source, head, main, directory, require_lease)
        details = self._details(head, main, build, result['conflict_paths'])
        evidence = hashlib.sha256(canonical({'source_review_id': job['review_id'],
            'merge_id': job['merge_id'], 'seed': result, 'details': details})).hexdigest()
        finding = self.finding(details, evidence_digest=evidence)
        manifest = {**original, 'review_id': review_id, 'candidate_commit': result['candidate_commit'],
            'candidate_workspace_digest': result['candidate_workspace_digest'],
            'integration_base': main, 'repair_requires_new_workspace': True, 'checks_passed': False,
            'gate_manifest_sha256': result['gate_manifest_sha256'],
            'gate_source_sha256': result['gate_source_sha256'], 'delivery_failure': details}
        manifest.pop('native_ci', None)
        self.candidates.record(selected, manifest)
        require_lease()
        return {'version': 1, 'reason': 'merge_main_requires_reverification', 'summary': finding['summary'],
            'source_review_id': review_id, 'candidate_commit': result['candidate_commit'],
            'candidate_workspace_digest': result['candidate_workspace_digest'], 'integration_base': main,
            'gate_manifest_sha256': result['gate_manifest_sha256'], 'gate_source_sha256': result['gate_source_sha256'],
            'tested_candidate_commit': head, 'evidence_digest': evidence, 'findings': [finding],
            'build': build, 'delivery_failure': details, 'no_merge_confirmed': True}
