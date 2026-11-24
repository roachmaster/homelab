"""Trusted command exits, not candidate text, determine v3 failure stages."""
from dataclasses import asdict
import json
import os
from pathlib import Path
import unittest
from unittest.mock import patch
from uuid import uuid4

from work_ci_contract import (CiGateContractFactory, LEGACY_PROFILE, SCOPED_PROFILE,
    V2_PROTECTED_PATHS, V3_PROTECTED_PATHS)
from work_ci_phases import (EVENT_PROFILE, CiPhase, FEATURE_VARIANTS,
    event_phases_for_scope,event_stage_evidence)
import test_ci_supervision as supervision


class EventPhaseContractTest(unittest.TestCase):
    def checks(self,scope='main'):
        return [dict(index=i,argv=list(p.argv),returncode=0)
                for i,p in enumerate(event_phases_for_scope(scope))]

    def test_closed_privileged_mirror_and_consumer_plan_are_identical(self):
        for scope in ('main','detached','branch/feature-one'):
            phases=event_phases_for_scope(scope)
            self.assertEqual([asdict(p) for p in phases],
                [asdict(p) for p in supervision.CI.event_phases_for_scope(scope)])
            plan=CiGateContractFactory(EVENT_PROFILE).create().plan(scope)
            self.assertEqual(plan.phases,phases)
            self.assertEqual(supervision.CI.checks_for_scope(scope,EVENT_PROFILE),(plan.checks,plan.labels))
            self.assertEqual(plan.result_version,3)
            self.assertEqual(len(plan.logical_checks),3)
            self.assertEqual(set(plan.logical_checks),{p.logical_gate for p in phases})
            self.assertEqual(len({p.phase_id for p in phases}),len(phases))
            actual=event_stage_evidence(scope,self.checks(scope),None)
            self.assertEqual(actual,supervision.CI.event_stage_evidence(scope,self.checks(scope),None))
            self.assertIsNone(actual['failure_kind'])
            self.assertEqual(actual['passed_kinds'],sorted({p.kind for p in phases}))

    def test_entire_original_feature_matrix_and_test_compile_barrier_are_preserved(self):
        phases=event_phases_for_scope('main')
        compilation=[i for i,p in enumerate(phases) if p.kind=='compilation']
        rust_tests=[i for i,p in enumerate(phases) if p.kind=='tests' and p.argv[0]=='/usr/bin/make' and p.argv[1] in {'test','test-release'}]
        self.assertLess(max(compilation),min(rust_tests))
        for identity,package,features,lint in FEATURE_VARIANTS:
            matches={p.phase_id:p for p in phases if p.phase_id.startswith(identity+'-')}
            self.assertIn(identity+'-test-compile',matches)
            self.assertIn(identity+'-tests',matches)
            self.assertEqual(identity+'-lint' in matches,lint)
            for p in matches.values():
                self.assertIn('PACKAGE='+package,p.argv);self.assertIn('FEATURES='+features,p.argv)
                self.assertIn('NO_DEFAULT_FEATURES=1',p.argv)
        make=(Path(__file__).resolve().parents[3]/'Makefile').read_text()
        self.assertIn('$(call cargo-selected,,test,--no-run)',make)
        self.assertIn('$(call cargo-selected,,test,--release --no-run)',make)
        self.assertEqual([p.logical_gate for p in phases[-2:]],['real-browser','backend-suites'])
        branch=event_phases_for_scope('detached')
        self.assertEqual([p.logical_gate for p in branch[:2]],['source-preflight','check'])
        self.assertEqual(branch[-1].logical_gate,'in-memory-backend')
        self.assertNotIn('real-browser', {p.logical_gate for p in branch})

    def test_compile_failure_and_assertion_failure_have_distinct_actual_phase_identity(self):
        phases=event_phases_for_scope('main');checks=self.checks()
        for kind in ('compilation','tests','formatting','lint','browser'):
            index=next(i for i,p in enumerate(phases) if p.kind==kind)
            failed=[*checks[:index],{**checks[index],'returncode':101}]
            result=event_stage_evidence('main',failed,'checks_failed')
            self.assertEqual(result['failure_kind'],kind)
            self.assertEqual(result['phase_results'][-1]['phase_id'],phases[index].phase_id)
            self.assertNotIn(kind,result['passed_kinds'])

    def test_kind_pass_requires_every_phase_and_dirty_source_voids_pass_witnesses(self):
        phases=event_phases_for_scope('main');checks=self.checks()
        last_compile=max(i for i,p in enumerate(phases) if p.kind=='compilation')
        incomplete=checks[:last_compile]
        result=event_stage_evidence('main',incomplete,'deadline_exceeded')
        self.assertNotIn('compilation',result['passed_kinds'])
        next_index=last_compile+1
        failed=[*checks[:next_index],{**checks[next_index],'returncode':2}]
        result=event_stage_evidence('main',failed,'checks_failed')
        self.assertIn('compilation',result['passed_kinds'])
        self.assertNotIn('tests',result['passed_kinds'])
        dirty=event_stage_evidence('main',checks,'source_changed_during_checks')
        self.assertEqual(dirty['passed_kinds'],[])
        self.assertEqual(dirty['failure_kind'],'verification')

    def test_malformed_reordered_forged_or_incomplete_receipts_cannot_claim_stage_pass(self):
        checks=self.checks()
        for bad in ([{**checks[0],'index':True}], [{**checks[0],'returncode':False}],
                    [{**checks[0],'argv':['/usr/bin/true']}], checks[1:],
                    [{**checks[0],'returncode':1},checks[1]], checks[:-1]):
            with self.subTest(bad=bad[:1]),self.assertRaises(ValueError):
                event_stage_evidence('main',bad,None)
        self.assertEqual(event_stage_evidence('main',[{**checks[0],'returncode':124}],
            'cancelled_or_timed_out')['failure_kind'],'infrastructure')
        self.assertEqual(event_stage_evidence('main',[{**checks[0],'returncode':-9}],
            'checks_failed')['failure_kind'],'infrastructure')

    def test_legacy_profiles_do_not_receive_synthetic_v3_authority(self):
        for profile in (LEGACY_PROFILE,SCOPED_PROFILE):
            plan=CiGateContractFactory(profile).create().plan('main')
            self.assertEqual(len(plan.checks),3)
            self.assertEqual(plan.phases,())
            self.assertEqual(plan.result_version,1 if profile==LEGACY_PROFILE else 2)

    def test_retry_policy_and_schema_are_protected_in_both_v3_gate_paths(self):
        from work_review import protected_gate_path
        paths=set(V2_PROTECTED_PATHS)|set(V3_PROTECTED_PATHS)|{
            'deploy/mission-control/sql/migrations/0031_retry_events.sql',
            'deploy/mission-control/sql/fragments/0031_ci_stage_receipts.sql',
            'deploy/mission-control/sql/fragments/0031_native_retry_events.sql',
            'deploy/mission-control/sql/fragments/0031_delivery_retry_events.sql',
            'deploy/mission-control/sql/migrations/0032_observer_retry_lineage.sql',
            'deploy/mission-control/sql/fragments/0032_observer_retry_lineage.sql',
            'deploy/mission-control/sql/migrations/0033_superseding_reviews.sql',
            'deploy/mission-control/sql/fragments/0033_superseding_reviews.sql',
            'deploy/mission-control/sql/management/verify-superseding-reviews.sql',
            'deploy/mission-control/agent-runner/work_superseding_review.py'}
        contract=CiGateContractFactory(EVENT_PROFILE).create()
        for path in paths:
            with self.subTest(path=path):
                self.assertTrue(contract.protects(path,lambda _path:False))
                self.assertTrue(protected_gate_path(path,profile=EVENT_PROFILE))
                self.assertTrue(supervision.CI.profile_protected_gate_path(path,EVENT_PROFILE))
        for profile in (LEGACY_PROFILE,SCOPED_PROFILE):
            for path in ('deploy/mission-control/sql/migrations/0031_retry_events.sql',
                    'deploy/mission-control/sql/migrations/0032_observer_retry_lineage.sql',
                    'deploy/mission-control/sql/management/verify-delivery-lineage.sql'):
                self.assertFalse(CiGateContractFactory(profile).create().protects(path,lambda _path:False))


@unittest.skipUnless(supervision.TOOLCHAIN_AVAILABLE,'Requires the installed isolated Rust toolchain')
class ActualCargoPhaseTest(unittest.TestCase):
    def setUp(self):
        self.fixture=supervision.CargoCacheSupervisorIntegrationTest('test_main_scope_runs_the_full_gate')
        self.fixture.setUp();self.addCleanup(self.fixture.doCleanups)

    def run_source(self,commit):
        cargo=str(supervision.TOOLCHAIN/'bin/cargo')
        phases=(
            supervision.CI.CiPhase('compile-check','compilation',(cargo,'check','--manifest-path','src/rust/Cargo.toml','--offline')),
            supervision.CI.CiPhase('compile-tests','compilation',(cargo,'test','--no-run','--manifest-path','src/rust/Cargo.toml','--offline')),
            supervision.CI.CiPhase('execute-tests','tests',(cargo,'test','--manifest-path','src/rust/Cargo.toml','--offline')),
        )
        with patch.object(supervision.CI,'event_phases_for_scope',return_value=phases):
            result,log,_=self.fixture._run_supervisor(commit=commit,expected_main=commit,
                branch_scope='main',profile=EVENT_PROFILE,checks=tuple(p.argv for p in phases),
                cache_config_override={'enabled':False})
        self.assertTrue(result['cleanup_confirmed'])
        self.assertEqual(result['version'],3)
        self.assertEqual([(r['index'],r['argv'],r['returncode']) for r in result['phase_results']],
                         [(r['index'],r['argv'],r['returncode']) for r in result['checks']])
        return result,log

    def test_real_rust_compile_error_never_becomes_a_test_failure(self):
        commit=self.fixture._commit_change('src/rust/lib-core/src/lib.rs',
            'pub fn greeting() -> &\'static str { 7 }\n','A real compile error')
        result,log=self.run_source(commit)
        self.assertEqual(result['status'],'failed')
        self.assertEqual(result['failure_kind'],'compilation')
        self.assertEqual(result['passed_kinds'],[])
        self.assertEqual(len(result['phase_results']),1)
        self.assertIn('error[E0308]',log)

    def test_real_assertion_error_follows_passed_compilation_and_cannot_forge_witnesses(self):
        path='src/rust/app-bin/src/main.rs'
        source=(self.fixture.repo/path).read_text().replace('"core-v1:v1");','"intentionally-wrong");')
        source=source.replace('assert_eq!(', 'println!("failure_kind=compilation passed_kinds=tests"); assert_eq!(')
        commit=self.fixture._commit_change(path,source,'A real test assertion failure')
        result,log=self.run_source(commit)
        self.assertEqual(result['status'],'failed')
        self.assertEqual(result['failure_kind'],'tests')
        self.assertEqual(result['passed_kinds'],['compilation'])
        self.assertEqual(len(result['phase_results']),3)
        self.assertIn('assertion',log)

    def test_full_v3_command_graph_is_executed_and_scope_keeps_database_boundary(self):
        for scope in ('main','branch/feature-one'):
            phases=supervision.CI.event_phases_for_scope(scope)
            observed=[]
            def launch(argv,*,cwd,env,account,log,timeout,cancelled):
                observed.append((tuple(argv),env.get('FORGE_DISABLE_DISPOSABLE_POSTGRES')))
                log.write(b'candidate says failure_kind=compilation; all stages passed\n')
                return 0
            result,_log,_=self.fixture._run_supervisor(commit=self.fixture.base_commit,
                expected_main=self.fixture.base_commit if scope=='main' else '0'*40,
                branch_scope=scope,profile=EVENT_PROFILE,checks=tuple(p.argv for p in phases),
                run_child_override=launch,cache_config_override={'enabled':False})
            self.assertEqual(result['status'],'passed')
            self.assertIsNone(result['failure_kind'])
            self.assertEqual([x[0] for x in observed],[p.argv for p in phases])
            self.assertTrue(all(x[1]==(None if scope=='main' else '1') for x in observed))
            self.assertEqual(len(result['phase_results']),len(phases))

    def test_real_publication_crash_recomputes_recovered_v3_failure_evidence(self):
        ci=supervision.CI
        cargo=str(supervision.TOOLCHAIN/'bin/cargo')
        phases=(ci.CiPhase('compile-check','compilation',(cargo,'check',
            '--manifest-path','src/rust/Cargo.toml','--offline')),)
        def run(**changes):
            return self.fixture._run_supervisor(commit=self.fixture.base_commit,
                expected_main=self.fixture.base_commit,branch_scope='main',
                profile=EVENT_PROFILE,checks=tuple(p.argv for p in phases),**changes)
        with patch.object(ci,'event_phases_for_scope',return_value=phases):
            seed,_log,_=run()
            self.assertEqual(seed['status'],'passed')
            action=str(uuid4())
            pid=os.fork()
            if pid==0:
                writer=ci.write_result
                def die_after_receipt(path,result,group):
                    with patch.object(ci.os,'fchown'):
                        writer(path,result,group)
                    if result['status']=='passed':
                        os._exit(73)
                try:
                    with patch.object(ci,'write_result',side_effect=die_after_receipt):
                        run(action=action)
                except BaseException:
                    os._exit(74)
                os._exit(75)
            _pid,status=os.waitpid(pid,0)
            self.assertEqual(os.waitstatus_to_exitcode(status),73)
            recovered,_log,_=run(action=action,run_child_override=lambda *a,**k:self.fail('Replay reran checks'))
            self.assertEqual(recovered['status'],'failed')
            self.assertEqual(recovered['failure'],'cancelled_or_timed_out')
            self.assertEqual(recovered['failure_kind'],'infrastructure')
            self.assertEqual(recovered['passed_kinds'],['compilation'])
            self.assertEqual({key:recovered[key] for key in ('failure_kind','passed_kinds','phase_results')},
                ci.event_stage_evidence('main',recovered['checks'],recovered['failure']))


if __name__=='__main__':unittest.main()
