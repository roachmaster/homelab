"""Pipeline stage projection fixtures: one per state-machine case, no DB."""
import importlib.util
from datetime import datetime, timedelta, timezone
from pathlib import Path
import unittest

from work_ci_contract import LEGACY_PROFILE, SCOPED_PROFILE
from work_pipeline_projection import (
    PIPELINE_STALENESS_TIMEOUT_SECONDS, identify_plan, project_pipeline_stages,
    select_latest_build_events, project_correlation_cicd)

HERE = Path(__file__).resolve().parent
SETUP_SPEC = importlib.util.spec_from_file_location('forge_ci_provision', HERE.parent / 'jenkins/setup-ci.py')
SETUP = importlib.util.module_from_spec(SETUP_SPEC)
SETUP_SPEC.loader.exec_module(SETUP)

COMMIT = 'a' * 40
STARTED_AT = datetime(2026, 11, 24, 1, 0, tzinfo=timezone.utc)
NOW = STARTED_AT + timedelta(seconds=60)


def event(kind, sequence, **fields):
    row = {'event_kind': kind, 'sequence': sequence, 'build_number': 7, 'queue_id': 3,
           'commit_sha': COMMIT, 'build_result': None, 'stage_name': None,
           'stage_status': None, 'occurred_at': STARTED_AT}
    row.update(fields)
    return row


def branch_plan():
    return identify_plan(['source-preflight', 'check', 'in-memory-backend'])


def observation(*statuses, names=('source-preflight', 'check', 'in-memory-backend')):
    return [{'name': name, 'status': status, 'head_sha': COMMIT}
            for name, status in zip(names, statuses)]


def project(events, obs=None, **kwargs):
    kwargs.setdefault('now', NOW)
    return project_pipeline_stages(plan=branch_plan(), events=events, observation=obs, **kwargs)


def statuses(result):
    return [stage['status'] for stage in result['pipeline_stages']]


class StageOnlyProgressTest(unittest.TestCase):
    def test_real_stage_start_projects_running_without_inventing_build_events(self):
        events=[event('stage_started',1,stage_name='source-preflight')]
        result=project(events)
        self.assertEqual(statuses(result),['running','waiting','waiting'])
        self.assertEqual(result['pipeline_phase'],'running')
        self.assertEqual([e['event_kind'] for e in events],['stage_started'])

    def test_stage_only_native_sequence_tracks_actual_next_gate(self):
        result=project([event('stage_started',1,stage_name='source-preflight'),
            event('stage_completed',2,stage_name='source-preflight',stage_status='success'),
            event('stage_started',3,stage_name='check')])
        self.assertEqual(statuses(result),['passed','running','waiting'])
        self.assertEqual(result['pipeline_availability'],'available')

    def test_later_progress_does_not_reset_earliest_stage_staleness_bound(self):
        late=STARTED_AT+timedelta(seconds=PIPELINE_STALENESS_TIMEOUT_SECONDS+60)
        result=project([event('stage_started',1,stage_name='source-preflight'),
            event('stage_completed',2,stage_name='source-preflight',stage_status='success'),
            event('stage_started',3,stage_name='check',occurred_at=late)],now=late)
        self.assertEqual(result['pipeline_phase'],'unknown')
        self.assertEqual(statuses(result),['passed','unknown','unknown'])

    def test_unknown_stage_is_not_execution_authority(self):
        result=project([event('stage_started',1,stage_name='unrecognized')])
        self.assertEqual(result['pipeline_phase'],'unknown')
        self.assertEqual(statuses(result),['unknown']*3)

    def test_missing_stage_timestamp_cannot_establish_unbounded_running(self):
        result=project([event('stage_started',1,stage_name='check',occurred_at=None)])
        self.assertEqual(result['pipeline_phase'],'unknown')

    def test_explicit_build_start_retains_its_earlier_staleness_bound(self):
        late=STARTED_AT+timedelta(seconds=PIPELINE_STALENESS_TIMEOUT_SECONDS+60)
        result=project([event('build_started',1),
            event('stage_started',2,stage_name='check',occurred_at=late)],now=late)
        self.assertEqual(result['pipeline_phase'],'unknown')


class PlanIdentificationTest(unittest.TestCase):
    def test_disjoint_check_sets_identify_each_frozen_plan(self):
        branch = identify_plan(['source-preflight', 'check', 'in-memory-backend'])
        self.assertEqual(branch.profile, SCOPED_PROFILE)
        self.assertEqual(branch.logical_checks, ('source-preflight', 'check', 'in-memory-backend'))
        main = identify_plan(['check', 'real-browser', 'backend-suites'])
        self.assertEqual(main.profile, SCOPED_PROFILE)
        self.assertEqual(main.logical_checks, ('check', 'real-browser', 'backend-suites'))
        legacy = identify_plan(['check', 'real-browser', 'pr-fast-gate'])
        self.assertEqual(legacy.profile, LEGACY_PROFILE)
        self.assertEqual(legacy.logical_checks, ('check', 'real-browser', 'pr-fast-gate'))

    def test_unknown_check_set_identifies_no_plan(self):
        self.assertIsNone(identify_plan(['check', 'unknown-stage']))
        self.assertIsNone(identify_plan([]))


class LatestBuildSelectionTest(unittest.TestCase):
    def test_retry_build_supersedes_earlier_build(self):
        events = [
            event('build_started', 1, build_number=7),
            event('stage_started', 2, build_number=7, stage_name='check'),
            event('build_started', 3, build_number=8),
            event('stage_started', 4, build_number=8, stage_name='check'),
        ]
        selected = select_latest_build_events(events)
        self.assertEqual([row['sequence'] for row in selected], [3, 4])

    def test_queued_event_correlates_by_queue_id(self):
        events = [
            event('build_queued', 1, build_number=None, queue_id=3),
            event('build_started', 2, build_number=7, queue_id=3),
            event('build_queued', 3, build_number=None, queue_id=9),
        ]
        selected = select_latest_build_events(events)
        self.assertEqual([row['sequence'] for row in selected], [1, 2])

    def test_queued_only_keeps_latest_queue_entry(self):
        events = [
            event('build_queued', 1, build_number=None, queue_id=3),
            event('build_queued', 2, build_number=None, queue_id=9),
        ]
        selected = select_latest_build_events(events)
        self.assertEqual([row['sequence'] for row in selected], [2])


class PipelineStateMachineTest(unittest.TestCase):
    def test_no_run_without_events_or_observation(self):
        result = project([])
        self.assertEqual(result['pipeline_stages'], [])
        self.assertEqual(result['pipeline_availability'], 'no_run')
        self.assertEqual(result['pipeline_phase'], 'unknown')

    def test_unidentified_plan_reports_stage_data_unavailable(self):
        result = project_pipeline_stages(plan=None, events=[event('build_started', 1)],
                                         observation=None, now=NOW)
        self.assertEqual(result['pipeline_stages'], [])
        self.assertEqual(result['pipeline_availability'], 'stage_data_unavailable')
        self.assertEqual(result['pipeline_phase'], 'running')

    def test_startup_is_preparing_with_all_stages_unknown(self):
        result = project([event('build_queued', 1, build_number=None),
                          event('build_started', 2)])
        self.assertEqual(statuses(result), ['unknown', 'unknown', 'unknown'])
        self.assertEqual(result['pipeline_availability'], 'stage_data_unavailable')
        self.assertEqual(result['pipeline_phase'], 'preparing')

    def test_running_stage_and_waiting_successors(self):
        result = project([
            event('build_started', 1),
            event('stage_started', 2, stage_name='source-preflight'),
            event('stage_completed', 3, stage_name='source-preflight', stage_status='success'),
            event('stage_started', 4, stage_name='check'),
        ])
        self.assertEqual(statuses(result), ['passed', 'running', 'waiting'])
        self.assertEqual(result['pipeline_availability'], 'available')
        self.assertEqual(result['pipeline_phase'], 'running')

    def test_failed_stage_makes_successors_not_run(self):
        result = project([
            event('build_started', 1),
            event('stage_started', 2, stage_name='source-preflight'),
            event('stage_completed', 3, stage_name='source-preflight', stage_status='success'),
            event('stage_started', 4, stage_name='check'),
            event('stage_completed', 5, stage_name='check', stage_status='failure'),
        ])
        self.assertEqual(statuses(result), ['passed', 'failed', 'not_run'])
        self.assertEqual(result['pipeline_availability'], 'available')

    def test_inter_command_gap_is_unknown_and_partial(self):
        result = project([
            event('build_started', 1),
            event('stage_started', 2, stage_name='source-preflight'),
            event('stage_completed', 3, stage_name='source-preflight', stage_status='success'),
        ])
        self.assertEqual(statuses(result), ['passed', 'unknown', 'unknown'])
        self.assertEqual(result['pipeline_availability'], 'partial')
        self.assertEqual(result['pipeline_phase'], 'running')

    def test_all_completed_before_build_completed_is_finalizing(self):
        result = project([
            event('build_started', 1),
            event('stage_completed', 2, stage_name='source-preflight', stage_status='success'),
            event('stage_completed', 3, stage_name='check', stage_status='success'),
            event('stage_completed', 4, stage_name='in-memory-backend', stage_status='success'),
        ])
        self.assertEqual(statuses(result), ['passed', 'passed', 'passed'])
        self.assertEqual(result['pipeline_phase'], 'finalizing')
        self.assertEqual(result['pipeline_availability'], 'available')

    def test_subsumption_repairs_lost_earlier_events(self):
        result = project([
            event('build_started', 1),
            event('stage_started', 2, stage_name='in-memory-backend'),
        ])
        self.assertEqual(statuses(result), ['passed', 'passed', 'running'])

    def test_subsumption_never_overwrites_an_explicit_failure(self):
        result = project([
            event('build_started', 1),
            event('stage_completed', 2, stage_name='source-preflight', stage_status='failure'),
            event('stage_started', 3, stage_name='check'),
        ])
        # Contradictory input (a later stage cannot start after a failure);
        # the explicit failure wins, and the failed rule beats the
        # running-successor rule: a stage after a failed stage is not-run,
        # never waiting.
        self.assertEqual(statuses(result), ['failed', 'running', 'not_run'])

    def test_stale_run_degrades_to_unknown_phase(self):
        result = project([
            event('build_started', 1),
            event('stage_started', 2, stage_name='source-preflight'),
            event('stage_completed', 3, stage_name='source-preflight', stage_status='success'),
            event('stage_started', 4, stage_name='check'),
        ], now=STARTED_AT + timedelta(seconds=PIPELINE_STALENESS_TIMEOUT_SECONDS + 1))
        self.assertEqual(statuses(result), ['passed', 'unknown', 'unknown'])
        self.assertEqual(result['pipeline_availability'], 'partial')
        self.assertEqual(result['pipeline_phase'], 'unknown')

    def test_stale_run_without_stage_detail_is_not_preparing(self):
        result = project([event('build_started', 1)],
                         now=STARTED_AT + timedelta(seconds=PIPELINE_STALENESS_TIMEOUT_SECONDS + 1))
        self.assertEqual(result['pipeline_phase'], 'unknown')

    def test_staleness_bound_matches_verifier_timeout(self):
        self.assertEqual(PIPELINE_STALENESS_TIMEOUT_SECONDS, SETUP.DEFAULT_TIMEOUT_SECONDS)


class TerminalObservationTest(unittest.TestCase):
    def test_terminal_observation_overrides_events_on_disagreement(self):
        result = project([
            event('build_started', 1),
            event('stage_started', 2, stage_name='in-memory-backend'),
            event('build_completed', 3, build_result='failure'),
        ], observation('success', 'failure', 'skipped'))
        # Stage 1 was subsumed passed by the stage-2 start; the receipt
        # proves it failed, so the observation wins.
        self.assertEqual(statuses(result), ['passed', 'failed', 'not_run'])
        self.assertEqual(result['pipeline_phase'], 'complete')
        self.assertEqual(result['pipeline_availability'], 'available')

    def test_nonterminal_observation_does_not_override(self):
        result = project([
            event('build_started', 1),
            event('stage_started', 2, stage_name='source-preflight'),
            event('stage_completed', 3, stage_name='source-preflight', stage_status='success'),
            event('stage_started', 4, stage_name='check'),
        ], observation('pending', 'pending', 'pending'))
        self.assertEqual(statuses(result), ['passed', 'running', 'waiting'])

    def test_interruption_with_receipt_marks_failed_stage_and_not_run(self):
        result = project([
            event('build_started', 1),
            event('stage_started', 2, stage_name='source-preflight'),
            event('stage_completed', 3, stage_name='source-preflight', stage_status='success'),
            event('stage_started', 4, stage_name='check'),
            event('build_completed', 5, build_result='aborted'),
        ], observation('success', 'failure', 'skipped'))
        self.assertEqual(statuses(result), ['passed', 'failed', 'not_run'])
        self.assertEqual(result['pipeline_stages'][1]['detail'], 'interrupted by cancellation')
        self.assertEqual(result['pipeline_phase'], 'complete')

    def test_cancellation_without_receipt_leaves_interrupted_and_later_unknown(self):
        result = project([
            event('build_started', 1),
            event('stage_started', 2, stage_name='source-preflight'),
            event('build_completed', 3, build_result='aborted'),
        ])
        self.assertEqual(statuses(result), ['unknown', 'unknown', 'unknown'])
        self.assertEqual(result['pipeline_availability'], 'stage_data_unavailable')
        self.assertEqual(result['pipeline_phase'], 'complete')

    def test_failed_stage_carries_its_finding_reference(self):
        result = project([
            event('build_started', 1),
            event('stage_completed', 2, stage_name='source-preflight', stage_status='failure'),
        ], findings=[{'finding_id': 'f-1', 'summary': 'preflight failed',
                      'check_id': 'source-preflight'},
                     {'finding_id': 'f-2', 'summary': 'unrelated',
                      'check_id': 'check'}])
        stage = result['pipeline_stages'][0]
        self.assertEqual(stage['finding_id'], 'f-1')
        self.assertEqual(stage['finding_summary'], 'preflight failed')
        self.assertIsNone(result['pipeline_stages'][1]['finding_id'])

    def test_verification_unavailable_hides_last_known_stages(self):
        result = project([
            event('build_started', 1),
            event('stage_completed', 2, stage_name='source-preflight', stage_status='success'),
        ], verification_unavailable=True)
        self.assertEqual(result['pipeline_stages'], [])
        self.assertEqual(result['pipeline_availability'], 'stage_data_unavailable')
        self.assertEqual(result['pipeline_phase'], 'unknown')


class CorrelationObservationIdentityTest(unittest.TestCase):
    def run_projection(self, checks, *, candidate=COMMIT, action='retry'):
        result = project_correlation_cicd(
            correlation_id='fixture-correlation', now=NOW,
            jenkins_events=[
                event('build_started', 1, action_id=action),
                event('stage_started', 2, action_id=action, stage_name='source-preflight'),
            ],
            hosted_observations=[{'candidate_commit': candidate, 'checks': checks}],
        )
        return result['runs'][0]

    def checks(self, action):
        return [dict(check, action_id=action) for check in
                observation('success', 'success', 'success')]

    def test_retry_does_not_inherit_previous_actions_success_for_same_commit(self):
        run = self.run_projection(self.checks('previous'))
        self.assertEqual(run['phase'], 'running')
        self.assertEqual(run['stages'][0]['status'], 'running')

    def test_bound_observation_must_match_candidate_commit(self):
        run = self.run_projection(self.checks('retry'), candidate='b' * 40)
        self.assertEqual(run['phase'], 'running')

    def test_matching_action_and_commit_can_complete_run(self):
        run = self.run_projection(self.checks('retry'))
        self.assertEqual(run['phase'], 'complete')
        self.assertTrue(all(stage['status'] == 'passed' for stage in run['stages']))

    def test_unbound_legacy_observation_cannot_complete_action_bound_retry(self):
        run = self.run_projection(observation('success', 'success', 'success'))
        self.assertEqual(run['phase'], 'running')

    def test_unbound_legacy_run_retains_commit_observation(self):
        run = self.run_projection(observation('success', 'success', 'success'), action=None)
        self.assertEqual(run['phase'], 'complete')

    def test_mixed_actions_cannot_complete_either_run(self):
        checks = self.checks('retry')
        checks[-1]['action_id'] = 'previous'
        self.assertEqual(self.run_projection(checks)['phase'], 'running')

    def test_check_head_must_match_observation_candidate(self):
        checks = self.checks('retry')
        checks[-1]['head_sha'] = 'b' * 40
        self.assertEqual(self.run_projection(checks)['phase'], 'running')

    def test_bound_observation_cannot_complete_unbound_legacy_run(self):
        self.assertEqual(self.run_projection(self.checks('previous'), action=None)['phase'], 'running')


if __name__ == '__main__':
    unittest.main()
