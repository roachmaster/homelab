"""The latest genuine main run must not sort before its completed branch run."""
from datetime import datetime, timezone
import unittest

from work_pipeline_projection import project_correlation_cicd


def stage(action, build, scope, stamp, name='check', sequence=1):
    return dict(action_id=action, build_number=build, job_name='forge-native-candidate',
                branch_scope=scope, branch='main' if scope == 'main' else 'feature/codex',
                commit_sha=('b' if scope == 'main' else 'a') * 40,
                event_kind='stage_started', stage_name=name, sequence=sequence,
                occurred_at=stamp)


class CicdRunOrderTest(unittest.TestCase):
    def test_main_stage_witness_sorts_after_completed_branch(self):
        branch = stage('branch', 132, 'branch/feature-codex', '2026-11-27T21:00:00Z')
        finished = {**branch, 'event_kind': 'build_completed', 'build_result': 'SUCCESS',
                    'occurred_at': '2026-11-27T21:10:00Z', 'sequence': 2}
        main = stage('main', 134, 'main', '2026-11-27T21:11:00Z', sequence=3)
        artifact = dict(action_id='branch', stage_index=0, job_name='forge-native-candidate',
                        build_number=132, pipeline_scope='branch', branch_scope='branch/feature-codex',
                        commit_sha='a' * 40, logical_stage_id='preflight-format', phase_id='source-preflight',
                        phase_kind='verification', label='Preflight', status='passed', returncode=0,
                        created_at='2026-11-27T22:00:00Z')
        for rows in ([], [artifact]):
            with self.subTest(final_branch_artifact=bool(rows)):
                result = project_correlation_cicd(correlation_id='scope', stage_results=rows,
                    jenkins_events=[branch, finished, main], now=datetime(2026, 11, 27, 21, 12, tzinfo=timezone.utc))
                self.assertEqual([r['action_id'] for r in result['runs']], ['branch', 'main'])
                self.assertEqual(result['runs'][-1]['started_at'], main['occurred_at'])
                self.assertEqual(result['runs'][-1]['status'], 'running')
                self.assertEqual(main['event_kind'], 'stage_started')

    def test_explicit_start_wins_over_stage_clock(self):
        first = stage('main', 134, 'main', '2026-11-27T21:11:00Z')
        explicit = {**first, 'event_kind': 'build_started',
                    'occurred_at': '2026-11-27T21:10:00Z', 'sequence': 2}
        later = {**first, 'occurred_at': '2026-11-27T21:12:00Z', 'sequence': 3}
        result = project_correlation_cicd(correlation_id='scope',
            jenkins_events=[first, explicit, later], now=datetime(2026, 11, 27, 21, 13, tzinfo=timezone.utc))
        self.assertEqual(result['runs'][0]['started_at'], explicit['occurred_at'])

    def test_unknown_stage_or_missing_clock_cannot_supply_start(self):
        for item in (stage('main', 134, 'main', '2026-11-27T21:11:00Z', name='unknown'),
                     stage('main', 134, 'main', None)):
            with self.subTest(event=item):
                result = project_correlation_cicd(correlation_id='scope', jenkins_events=[item])
                self.assertIsNone(result['runs'][0]['started_at'])


if __name__ == '__main__':
    unittest.main()
