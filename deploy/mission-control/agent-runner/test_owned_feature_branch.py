"""Explicit owned branches, preserved task repositories and real Git handoff."""
from pathlib import Path
import subprocess
from tempfile import TemporaryDirectory
import unittest
from uuid import uuid4
from unittest.mock import Mock

from work_agent_branch import AgentBranchPolicy, validate_profile_branch
from work_config import WorkProfile
from work_executor import ExecutionError, WorkspaceProvider
from work_policy import POLICY_ID
from work_worker_config import parse_config, verify_recipe
from test_work_executor import raw_config, lease


def git(path, *args):
    return subprocess.check_output(['git', '-C', str(path), *args], stderr=subprocess.DEVNULL).decode().strip()


class OwnedFeatureBranchTest(unittest.TestCase):
    def test_all_profile_aliases_receive_identical_instruction_policy(self):
        from work_agent_branch import AGENT_BRANCH_INSTRUCTIONS, OWNED_FEATURE_BRANCHES
        from work_external_workflow import ExternalWorkflowStore
        for alias in (*OWNED_FEATURE_BRANCHES, 'configured-review-alias'):
            profile = WorkProfile(alias, 'codex', 'fixture-model', POLICY_ID)
            self.assertEqual(profile.instructions, AGENT_BRANCH_INSTRUCTIONS)
            from work_worker_config import WorkerProfile
            worker = WorkerProfile(alias, 'codex', 'fixture-model', ('review',))
            self.assertEqual(worker.instructions, AGENT_BRANCH_INSTRUCTIONS)
        for mode in ('plan', 'implement'):
            self.assertIn(AGENT_BRANCH_INSTRUCTIONS, ExternalWorkflowStore._author_prompt(mode))
        for rule in ('never renamed or replaced', 'Never create agent-owned per-task',
                     'rebase onto current main', 'incrementally after green tests',
                     'own local build cache', 'must not commit or push',
                     'Plan and review remain read-only', 'Legacy machinery-owned publication exception'):
            self.assertIn(rule, AGENT_BRANCH_INSTRUCTIONS)

    def test_codex_author_feature_branch_and_enforced_readonly_reviewer(self):
        with TemporaryDirectory() as root:
            raw = raw_config(root)
            raw['profiles'] = {'codex-author': {'backend':'codex','model':'fixture-model',
                'modes':['plan','implement'],'branch':'feature/codex'}}
            raw['binaries'] = {'codex':'/usr/bin/true'}
            raw['allow_implement'] = True
            author_config = parse_config(raw)
            repository, base = self.seed(author_config)
            provider = WorkspaceProvider(author_config); provider.prepare()
            assignment = {**lease(author_config,backend='codex',mode='implement'),
                          'agent_profile':'codex-author','base_revision':base}
            with provider.acquire(assignment) as workspace:
                self.assertEqual(git(workspace.path,'branch','--show-current'),'feature/codex')
            raw['profiles'] = {'codex-primary': {'backend':'codex','model':'fixture-model','modes':['review']}}
            reviewer_config = parse_config(raw)
            review = {**lease(reviewer_config,backend='codex',mode='review'),'agent_profile':'codex-primary','base_revision':base}
            self.assertTrue(verify_recipe(reviewer_config,review))
            self.assertFalse(verify_recipe(reviewer_config,{**review,'mode':'implement',
                'recipe':{**review['recipe'],'mode':'implement'}}))
            reviewer = WorkspaceProvider(reviewer_config); reviewer.prepare()
            with reviewer.acquire(review) as workspace:
                self.assertEqual(git(workspace.path,'rev-parse','--abbrev-ref','HEAD'),'HEAD')
            self.assertEqual(git(repository,'branch','--show-current'),'main')

    def config(self, root):
        raw = raw_config(root)
        raw['profiles'] = {'grok-primary': {'backend': 'grok', 'model': 'fixture-model',
            'modes': ['plan', 'implement'], 'execution_enabled': True, 'branch': 'feature/grok'}}
        raw['binaries'] = {'grok': '/usr/bin/true'}
        raw['allow_implement'] = True
        return parse_config(raw)

    def seed(self, config):
        repo = config.projects['homelab']; repo.mkdir()
        git(repo, 'init', '--quiet', '--initial-branch=main')
        git(repo, 'config', 'user.name', 'Fixture')
        git(repo, 'config', 'user.email', 'fixture@example.invalid')
        (repo / 'file.txt').write_text('base')
        git(repo, 'add', '.'); git(repo, 'commit', '--quiet', '-m', 'base')
        return repo, git(repo, 'rev-parse', 'HEAD')

    def assignment(self, config, base, mode='plan'):
        value = lease(config, mode=mode)
        value.update(agent_profile='grok-primary', base_revision=base)
        return value

    def test_profile_and_project_configuration_reject_cross_agent_feature_branches(self):
        self.assertEqual(WorkProfile('grok-primary', 'grok', 'model', POLICY_ID,
                                    branch='feature/grok').branch, 'feature/grok')
        self.assertEqual(AgentBranchPolicy(profile_branches={'grok-primary': 'feature/grok'})
                         .resolve_branch('homelab', 'grok-primary'), 'feature/grok')
        for bad in ('feature/agy', 'feature/unknown', 'main', 'forge/task-any', '../feature/grok'):
            with self.subTest(branch=bad), self.assertRaises(ValueError):
                validate_profile_branch('grok-primary', bad)
        with self.assertRaises(ValueError):
            AgentBranchPolicy(profile_branches={'grok-primary': 'feature/grok'},
                project_overrides={'homelab': {'grok-primary': 'feature/agy'}})
        self.assertEqual(WorkProfile('grok-primary', 'grok', 'model', POLICY_ID).branch,
                         'forge/agent/grok-primary')

    def test_plan_to_implementation_keeps_metadata_and_digest_and_preserves_stopped_work(self):
        with TemporaryDirectory() as root:
            config = self.config(root); repo, base = self.seed(config)
            provider = WorkspaceProvider(config); provider.prepare()
            plan = self.assignment(config, base)
            with provider.acquire(plan) as workspace:
                self.assertEqual(git(workspace.path, 'rev-parse', '--abbrev-ref', 'HEAD'), 'HEAD')
                self.assertNotEqual(workspace.repository, repo)
                self.assertEqual(workspace.source_repository, repo)
                self.assertFalse((workspace.repository / 'objects/info/alternates').exists())
                digest = workspace.digest(); gitfile = (workspace.path / '.git').read_bytes()
            metadata = config.state_root / 'workspaces' / (plan['task_id'] + '.json')
            original_metadata = metadata.read_bytes()
            implement = {**plan, 'mode': 'implement', 'resume_workspace_digest': digest}
            with provider.acquire(implement) as workspace:
                self.assertEqual(git(workspace.path, 'branch', '--show-current'), 'feature/grok')
                self.assertEqual(workspace.digest(), digest)
                self.assertEqual((workspace.path / '.git').read_bytes(), gitfile)
                (workspace.path / 'file.txt').write_text('preserved implementation')
                repaired_digest = workspace.digest()
            self.assertEqual(metadata.read_bytes(), original_metadata)
            with provider.acquire({**implement, 'resume_workspace_digest': repaired_digest}) as workspace:
                self.assertEqual((workspace.path / 'file.txt').read_text(), 'preserved implementation')
            second = self.assignment(config, base, 'implement')
            with provider.acquire(second) as workspace:
                self.assertEqual(git(workspace.path, 'branch', '--show-current'), 'feature/grok')
            self.assertEqual(git(repo, 'branch', '--show-current'), 'main')
            self.assertFalse(git(repo, 'branch', '--list', 'feature/grok'))
            self.assertEqual((repo / 'file.txt').read_text(), 'base')

    def test_branch_tampering_refuses_snapshot_and_resume(self):
        with TemporaryDirectory() as root:
            config = self.config(root); _, base = self.seed(config)
            provider = WorkspaceProvider(config); provider.prepare()
            value = self.assignment(config, base, 'implement')
            with provider.acquire(value) as workspace:
                digest = workspace.digest()
                git(workspace.path, 'checkout', '--quiet', '--detach')
                with self.assertRaises(ExecutionError): workspace.digest()
            with self.assertRaises(ExecutionError):
                with provider.acquire({**value, 'resume_workspace_digest': digest}): pass

    def test_pinned_repair_candidate_outside_main_is_imported_without_alternates(self):
        with TemporaryDirectory() as root:
            config = self.config(root); repo, main = self.seed(config)
            git(repo, 'checkout', '--quiet', '-b', 'fixture-repair')
            (repo/'file.txt').write_text('verified repair candidate')
            git(repo, 'add', '.'); git(repo, 'commit', '--quiet', '-m', 'repair candidate')
            candidate = git(repo, 'rev-parse', 'HEAD')
            git(repo, 'checkout', '--quiet', 'main')
            source = Mock()
            provider = WorkspaceProvider(config, candidate_source_provider=source); provider.prepare()
            value = self.assignment(config, candidate, 'implement')
            value.update(repair_source_review_id=str(uuid4()), repair_candidate_commit=candidate)
            with provider.acquire(value) as workspace:
                self.assertEqual(git(workspace.path, 'rev-parse', 'HEAD'), candidate)
                self.assertEqual(git(workspace.path, 'branch', '--show-current'), 'feature/grok')
                self.assertEqual((workspace.path/'file.txt').read_text(), 'verified repair candidate')
                self.assertFalse((workspace.repository/'objects/info/alternates').exists())
            source.ensure.assert_called_once_with(value, repo)
            self.assertEqual(git(repo, 'rev-parse', 'main'), main)

    def test_substituted_common_repository_or_parent_symlink_is_rejected(self):
        with TemporaryDirectory() as root:
            config = self.config(root); repo, base = self.seed(config)
            provider = WorkspaceProvider(config); provider.prepare()
            value = self.assignment(config, base, 'implement')
            with provider.acquire(value) as workspace:
                common = workspace.git_directory/'commondir'
                common.write_text(str(repo/'.git')+'\n')
                with self.assertRaises(ExecutionError) as error: workspace.digest()
                self.assertEqual(error.exception.outcome, 'needs_reconcile')
            with self.assertRaises(ExecutionError):
                with provider.inspect(value): pass
        with TemporaryDirectory() as root:
            config = self.config(root); repo, base = self.seed(config)
            provider = WorkspaceProvider(config); provider.prepare()
            other = Path(root)/'substituted'; other.mkdir()
            (repo/'.git/forge-execution-repositories').symlink_to(other, target_is_directory=True)
            with self.assertRaises(ExecutionError):
                with provider.acquire(self.assignment(config, base)): pass


if __name__ == '__main__': unittest.main()
