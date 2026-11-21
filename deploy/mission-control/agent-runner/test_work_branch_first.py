"""Real branch-before-build ordering and honest source repair; no remote service."""
from contextlib import contextmanager
from dataclasses import replace
import hashlib
import json
import os
import threading
import unittest
from unittest.mock import Mock, patch
from uuid import uuid4

from test_work_publication import fixture, git
from test_work_review import ReviewStore, review_job
from test_work_native_jenkins import Driver
from test_work_merge import merge_fixture, MergeStore
from work_external_delivery import CHECKS, DeliveryConfig, canonical, durable_json
from work_feedback import validate_findings
from work_merge import MergeWorker
from work_native_jenkins import NativeJenkinsVerification
from work_native_repair import BranchFirstRepairProvider
from work_publication_clients import PublicationError, WorkspaceSnapshotProvider, Snapshot, SnapshotFile, branch_name
from work_review import ReviewedCandidateProvider, ReviewWorker


@contextmanager
def native_fixture(*, diverge=False):
    with fixture() as values:
        root, config, source, workspaces, _accepted, bare, provider, *_ = values
        config = replace(config, native_delivery_mode='branch-first-v1')
        with workspaces.inspect(source) as workspace:
            (workspace.path/'safe-link.py').unlink()
            source = {**source, 'workspace_digest': workspace.digest()}
        canonical_repo = config.worker.projects['homelab']
        if diverge:
            (canonical_repo/'app.py').write_text('concurrent main implementation\n')
            (canonical_repo/'main-only.py').write_text('preserve new main source\n')
            git(canonical_repo,'add','.')
            git(canonical_repo,'commit','--quiet','-m','concurrent main source')
            git(canonical_repo,'push','--quiet',str(bare),'main')
        main = git(bare,'rev-parse','refs/heads/main').decode().strip()
        job = review_job(source, workflow_id=str(uuid4()), correlation_id=str(uuid4()))
        scope = {key:job[key] for key in ('project_alias','task_id','attempt_id','review_id','workflow_id','correlation_id')}
        scope['owner_id'] = config.worker.owner
        store = ReviewStore()
        store.native_ci_acquire_context = Mock(return_value={**scope,'active':True})
        store.native_ci_binding = Mock(return_value={**scope,'active':True})
        store.native_branch_published = Mock(return_value={'event_id':str(uuid4()),'correlation_id':job['correlation_id']})
        driver = Driver(root)
        driver.config.jenkins_job = 'forge-native-candidate'
        candidates = ReviewedCandidateProvider(config,provider)
        supervisor = Mock()
        supervisor.stop.return_value = {'scope_stopped':True}
        events = []
        original_push = provider.push
        def push(*args):
            events.append('push')
            return original_push(*args)
        provider.push = push
        original_verify = driver.verify
        def verify(request, require):
            events.append('jenkins')
            selected = {**job,'publication_id':job['review_id'],'branch_publication_id':job['review_id']}
            assert provider.remote_head(selected) == request['candidate_commit']
            result = original_verify(request,require)
            repository = config.state_root/job['review_id']/'repository'
            try: provider._git(repository,'merge-base','--is-ancestor',main,request['candidate_commit'])
            except PublicationError:
                result = {**result,'status':'failed','summary':'Jenkins verification failed: main_ancestor_required',
                    'receipt':{**result['receipt'],'failure':'main_ancestor_required','checks':[]}}
            return result
        driver.verify = verify
        native = NativeJenkinsVerification(config,store,provider,candidates,driver,supervisor,
            wait=lambda _:None, cleanup_timeout=0)
        repair = BranchFirstRepairProvider(config,provider,candidates,native)
        def gate(selected,files):
            source_digest = hashlib.sha256(canonical(files)).hexdigest()
            helper = hashlib.sha256(selected.verifier.read_bytes()).hexdigest()
            return source_digest,helper,hashlib.sha256(canonical([source_digest,helper,CHECKS])).hexdigest()
        with patch('work_native_jenkins.jenkins_gate_manifest',side_effect=gate):
            worker = ReviewWorker(config,store,WorkspaceSnapshotProvider(workspaces),provider,native,candidates,
                native_ci=native,repair_sources=repair)
            yield values,config,job,main,store,native,repair,candidates,events,worker


class BranchFirstTest(unittest.TestCase):
    def test_exact_source_is_pushed_before_jenkins_and_reuses_its_owned_branch(self):
        with native_fixture() as (values,config,job,main,store,native,_repair,candidates,events,worker):
            worker.process(job)
            outcome = store.outcomes[-1][1]
            self.assertEqual(outcome['disposition'],'awaiting_review',outcome)
            self.assertEqual(events,['push','jenkins'])
            manifest = candidates.read(job['review_id'])
            repository = candidates.directory(job['review_id'])/'repository'
            parents = git(repository,'show','-s','--format=%P',outcome['candidate_commit']).decode().split()
            self.assertEqual(parents,[job['base_revision']])
            selected = native.publication_job({**job,'publication_id':str(uuid4()),
                'candidate_commit':outcome['candidate_commit']})
            self.assertEqual(branch_name(selected),'forge/task-'+job['review_id'])
            binding = manifest['native_ci']
            bundle = native._directory(binding['request']['prepare_action_id'])/'candidate.bundle'
            refs = git(repository,'bundle','list-heads',str(bundle)).decode().splitlines()
            self.assertEqual(len(refs),2)
            self.assertTrue(any(line.startswith(main+' ') and line.endswith('-main') for line in refs))

    def test_restart_after_remote_push_reuses_source_commit_and_requires_committed_event(self):
        with native_fixture() as (_values,_config,job,_main,store,native,_repair,candidates,events,worker):
            acknowledged = store.native_branch_published.return_value
            store.native_branch_published.side_effect = [KeyboardInterrupt('crash before event ACK'),acknowledged]
            with self.assertRaises(KeyboardInterrupt):worker.process(job)
            self.assertEqual(events,['push'])
            raw = git(candidates.directory(job['review_id'])/'repository','rev-parse','HEAD')
            self.assertEqual(native.delivery.submissions,0)
            worker.process(job)
            self.assertEqual(store.outcomes[-1][1]['disposition'],'awaiting_review',store.outcomes[-1][1])
            self.assertEqual(events,['push','jenkins'])
            self.assertEqual(git(candidates.directory(job['review_id'])/'repository','rev-parse','HEAD'),raw)
            binding = candidates.read(job['review_id'])['native_ci']
            self.assertEqual(binding['source_publication']['event_id'],acknowledged['event_id'])

    def test_restart_between_seed_renames_finishes_same_unapproved_source(self):
        from pathlib import Path
        with native_fixture(diverge=True) as (_values,_config,job,_main,store,native,_repair,candidates,events,worker):
            original = Path.rename
            def interrupted(path,target):
                if path.name=='repository' and path.parent.name=='delivery-repair':
                    raise KeyboardInterrupt('crash after preserving tested source')
                return original(path,target)
            with patch.object(Path,'rename',interrupted),self.assertRaises(KeyboardInterrupt):worker.process(job)
            self.assertFalse((candidates.directory(job['review_id'])/'repository').exists())
            seed = json.loads((candidates.directory(job['review_id'])/'delivery-repair'/'seed-result.json').read_text())
            worker.process(job)
            outcome = store.outcomes[-1][1]
            self.assertEqual(outcome['disposition'],'repair',outcome)
            self.assertEqual(outcome['candidate_commit'],seed['candidate_commit'])
            self.assertEqual(native.delivery.submissions,1)
            self.assertEqual(events.count('push'),1)

    def test_genuine_ci_main_failure_preserves_tested_source_and_honest_conflict_seed(self):
        with native_fixture(diverge=True) as (values,config,job,main,store,native,_repair,candidates,events,worker):
            worker.process(job)
            outcome = store.outcomes[-1][1]
            self.assertEqual(outcome['disposition'],'repair',outcome)
            self.assertEqual(events,['push','jenkins'])
            manifest = candidates.read(job['review_id'])
            details = outcome['verification']['delivery_failure']
            self.assertEqual(details,manifest['delivery_failure'])
            self.assertEqual(details['current_main'],main)
            self.assertEqual(details['build']['job'],'forge-native-candidate')
            self.assertEqual(details['build']['build_number'],4)
            self.assertFalse(manifest['checks_passed'])
            directory = candidates.directory(job['review_id'])
            self.assertEqual(git(directory/'repository.tested','rev-parse','HEAD').decode().strip(),details['tested_candidate'])
            self.assertEqual(git(directory/'repository','show','-s','--format=%P','HEAD').decode().split(),
                [details['tested_candidate'],main])
            self.assertIn('<<<<<<<',(directory/'repository'/'app.py').read_text())
            self.assertEqual((directory/'repository'/'main-only.py').read_text(),'preserve new main source\n')
            self.assertTrue(outcome['verification']['repair_requires_new_workspace'])
            self.assertTrue(all(check['status']=='not_run' for check in outcome['verification']['checks']))
            self.assertEqual(manifest['native_ci']['request']['candidate_commit'],details['tested_candidate'])
            self.assertEqual(git(values[5],'rev-parse','refs/heads/main').decode().strip(),main)

    def test_uncertain_push_never_starts_jenkins_or_repeats_the_write(self):
        with native_fixture() as (_values,config,job,_main,store,native,_repair,_candidates,_events,_worker):
            source = _values[3]
            snapshot = WorkspaceSnapshotProvider(source).create(job)
            selected = {**job,'publication_id':job['review_id']}
            repository,head = native.git.prepare_source(selected,snapshot)
            selected['candidate_commit'] = head
            native.git.remote_head = Mock(return_value=None)
            native.git.push = Mock(side_effect=OSError('lost push ACK'))
            for _ in range(2):
                with self.assertRaises(PublicationError):native.publish_source(selected,repository)
            self.assertEqual(native.git.push.call_count,1)
            self.assertEqual(native.delivery.submissions,0)

    def test_stop_before_controlled_push_has_no_remote_effect(self):
        with native_fixture() as (_values,config,job,_main,store,native,_repair,_candidates,_events,_worker):
            snapshot = WorkspaceSnapshotProvider(_values[3]).create(job)
            selected = {**job,'publication_id':job['review_id']}
            repository,head = native.git.prepare_source(selected,snapshot)
            selected['candidate_commit'] = head
            native.git.push = Mock()
            stop = threading.Event();stop.set()
            with self.assertRaises(PublicationError):native.run(selected,repository,stop)
            native.git.push.assert_not_called()
            self.assertEqual(native.delivery.submissions,0)

    def test_delivery_diagnostic_is_bounded_independently_of_full_conflict_context(self):
        with native_fixture() as (_values,_config,_job,main,_store,_native,repair,_candidates,_events,_worker):
            details = {'version':1,'tested_candidate':'a'*40,'current_main':main,
                'build':{'provider':'jenkins','job':'forge-native-candidate','action_id':str(uuid4()),'queue_id':7,'build_number':4},
                'conflict_paths':['x'*1000+str(i) for i in range(32)]}
            finding = repair.finding(details,evidence_digest='b'*64)
            self.assertLessEqual(len(finding['actual'].encode()),2048)
            self.assertEqual(len(validate_findings([finding])),1)

    def test_fixed_jenkins_job_is_explicit_and_old_configuration_stays_default(self):
        raw = dict(owner='owner',worker_id='fixture',database={'host':'/var/run/postgresql','port':5432,
            'name':'forge_agent','user':'forge_coding_reconciler'},
            projects={'homelab':{'repository':'fixture/project','canonical_repo':'/canonical'}},
            **{key:'/'+key for key in ('state_root','spool_root','results_root','github_config_dir',
                'jenkins_binary','jenkins_config','verifier')},jenkins_binary_sha256='a'*64)
        self.assertEqual(DeliveryConfig.parse(raw).jenkins_job,'forge-exact-candidate')
        self.assertEqual(DeliveryConfig.parse({**raw,'jenkins_job':'forge-native-candidate'}).jenkins_job,'forge-native-candidate')
        for value in ('other','../main',True,None):
            with self.subTest(value=value),self.assertRaises(ValueError):
                DeliveryConfig.parse({**raw,'jenkins_job':value})

    def test_post_ci_main_drift_queues_a_distinct_unapproved_repair_source(self):
        with merge_fixture() as (config,job,provider,client,candidates,hosted):
            base = client.git('rev-parse','main').stdout.decode().strip()
            tree = client.git('rev-parse','main^{tree}').stdout.decode().strip()
            main = client.git('-c','user.name=Fixture','-c','user.email=fixture@localhost',
                'commit-tree',tree,'-p',base,data=b'concurrent main\n').stdout.decode().strip()
            client.git('update-ref','refs/heads/main',main)
            hosted.build_reference = Mock(return_value={'provider':'jenkins','job':'forge-native-candidate',
                'action_id':str(uuid4()),'queue_id':7,'build_number':4})
            repair = BranchFirstRepairProvider(config,provider.git,candidates,hosted)
            store = MergeStore()
            store.native_merge_failure = Mock(return_value={'status':'repair'})
            original = candidates.read(job['review_id'])
            MergeWorker(config,store,provider,source_sync=Mock(),repair_sources=repair).process(job)
            store.native_merge_failure.assert_called_once()
            outcome = store.native_merge_failure.call_args.args[1]
            self.assertTrue(outcome['no_merge_confirmed'])
            self.assertNotEqual(outcome['source_review_id'],job['review_id'])
            seed = candidates.read(outcome['source_review_id'])
            self.assertFalse(seed['checks_passed'])
            self.assertEqual(seed['delivery_failure'],outcome['delivery_failure'])
            self.assertEqual(candidates.read(job['review_id']),original)
            self.assertEqual(client.git('rev-parse','main').stdout.decode().strip(),main)
            self.assertFalse(any(method=='PATCH' for method,*_ in client.calls))

            # A clean integration still needs new CI/review, but should not
            # require an artificial source edit just to publish the seed.
            canonical = config.worker.projects['homelab']
            seed_repository = candidates.directory(outcome['source_review_id'])/'repository'
            candidate = outcome['candidate_commit']
            provider.git._git(canonical,'fetch','--quiet','--no-write-fetch-head','--',str(seed_repository),candidate)
            files = tuple(SnapshotFile(path.decode(),'link' if (seed_repository/path.decode()).is_symlink() else 'file',
                os.fsencode(os.readlink(seed_repository/path.decode())) if (seed_repository/path.decode()).is_symlink()
                    else (seed_repository/path.decode()).read_bytes(),False)
                for path in git(seed_repository,'ls-files','-z').split(b'\0') if path)
            repair_job = {**job,'publication_id':str(uuid4()),'base_revision':candidate,
                'attempt_id':original['attempt_id'], 'workspace_digest':'a'*64,
                'parent_task_id':job['task_id'],'feedback_id':str(uuid4())}
            _repository,new_head = provider.git.prepare_source(repair_job,Snapshot(files,'a'*64))
            self.assertEqual(new_head,candidate)

    def test_uncertain_merge_observation_never_claims_a_no_merge_repair_event(self):
        with merge_fixture() as (config,job,provider,_client,_candidates,_hosted):
            intent = provider.prepare(job)
            provider.prepare = Mock(return_value=intent)
            provider.reconcile = Mock(side_effect=[False,OSError('main observation unavailable')])
            provider.apply = Mock(side_effect=PublicationError('reference_not_fast_forward'))
            repair,store = Mock(),MergeStore()
            store.native_merge_failure = Mock()
            MergeWorker(config,store,provider,source_sync=Mock(),repair_sources=repair).process(job)
            repair.prepare.assert_not_called()
            store.native_merge_failure.assert_not_called()

    def test_exhausted_merge_repair_budget_finishes_handoff_without_repeating_source_work(self):
        from work_worker_store import OutcomeConflictError
        with merge_fixture() as (config,job,provider,_client,_candidates,_hosted):
            provider.prepare = Mock(side_effect=PublicationError('merge_main_requires_reverification'))
            repair,store = Mock(),MergeStore()
            store.native_merge_failure = Mock(side_effect=OutcomeConflictError())
            MergeWorker(config,store,provider,source_sync=Mock(),repair_sources=repair).process(job)
            repair.prepare.assert_called_once()
            store.native_merge_failure.assert_called_once()
            self.assertEqual(store.results[-1][0],'handoff')

    def test_main_already_in_tested_history_still_creates_an_unapproved_exact_tree_seed(self):
        with merge_fixture() as (config,job,provider,client,candidates,hosted):
            main = client.git('rev-parse','main').stdout.decode().strip()
            hosted.build_reference = Mock(return_value={'provider':'jenkins','job':'forge-native-candidate',
                'action_id':str(uuid4()),'queue_id':7,'build_number':4})
            repair = BranchFirstRepairProvider(config,provider.git,candidates,hosted)
            outcome = repair.prepare(job,main,lambda:None)
            repository = candidates.directory(outcome['source_review_id'])/'repository'
            self.assertEqual(git(repository,'show','-s','--format=%P','HEAD').decode().split(),[job['candidate_commit'],main])
            self.assertEqual(git(repository,'rev-parse','HEAD^{tree}'),git(repository,'rev-parse',job['candidate_commit']+'^{tree}'))
            self.assertFalse(candidates.read(outcome['source_review_id'])['checks_passed'])


if __name__ == '__main__':unittest.main()
