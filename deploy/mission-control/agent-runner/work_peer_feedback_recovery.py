"""Receipt-bound recovery of valid same-task peer feedback on a linked child.

The build-feedback consumer retains its own classifier. This selected consumer
reuses its common stop/receipt/packet/lease/readiness/budget/Resume engine and
accepts only the independently verified linked-child binding defect.
"""
from dataclasses import dataclass
import hashlib

from queue_store import Rejected
from work_context_recovery import FeedbackRecovery
from work_peer_context import validate_workflow_feedback
from work_peer_review import parse_review_target, validate_live_review_verdict
from work_review_context import ReviewContext


POLICY = 'forge-native-peer-feedback-recovery-v1'
FAILURE_SUMMARY = 'The complete workflow feedback could not be verified.'


class PeerFeedbackRecovery(FeedbackRecovery):
    POLICY_ID = POLICY
    FAILURE_SUMMARIES = (FAILURE_SUMMARY,)
    ACTION_PREFIX = 'peer-feedback-delivery-retry:'
    IDEMPOTENCY_PREFIX = 'peer-feedback-delivery-'
    AUDIT_STAGE = 'peer_feedback_delivery_recovered'
    RECOVERY_DESCRIPTION = 'Verified current peer feedback resumed the same approved linked task.'

    def retry(self, **request):
        # New automatic admissions always use the selected canonical event
        # provider. Historical build-feedback recovery retains its old behavior.
        if self.flow.retry_events is None:
            raise Rejected(503, 'unavailable')
        return super().retry(**request)

    def _validate_failure_context(self, cursor, w, task, attempt, rec, feedback, packet, review, source):
        from work_workflow import recipe_from_snapshot
        if (review['verification'].get('checks_passed') is not True
                or task['parent_task_id'] is None or attempt['repair_source_review_id'] is None
                or task['repair_source_review_id'] != attempt['repair_source_review_id']
                or attempt['workflow_feedback_review_id'] is None
                or attempt['workflow_feedback_review_id'] != task['workflow_feedback_review_id']
                or attempt['workflow_feedback_sha256'] != task['workflow_feedback_sha256']):
            return False
        cursor.execute('SELECT * FROM forge_agent.coding_workflow_reviews WHERE '
            '(owner_id,project_alias,workflow_id,workflow_review_id)=(%s,%s,%s,%s)',
            (self.store.owner, task['project_alias'], w['workflow_id'], attempt['workflow_feedback_review_id']))
        peer = cursor.fetchone()
        if (not peer or peer['status'] != 'decided' or peer['target_kind'] != 'code'
                or peer['source_task_id'] != task['id'] or peer['source_attempt_id'] != packet['parent_attempt_id']
                or peer['source_result_sha256'] != source['result_sha256']
                or peer['source_revision'] != review['source_revision']
                or peer['source_review_id'] != review['review_id']
                or peer['candidate_commit'] != review['candidate_commit']
                or peer['gate_manifest_sha256'] != review['gate_manifest_sha256']
                or peer['verdict_sha256'] != attempt['workflow_feedback_sha256']
                or peer['reviewer_result_sha256'] != peer['verdict_sha256']
                or peer['reviewer_attempt_id'] is None):
            return False
        cursor.execute('SELECT * FROM forge_agent.coding_tasks WHERE(owner_id,project_alias,id)=(%s,%s,%s)',
            (self.store.owner, task['project_alias'], peer['reviewer_task_id']))
        critic = cursor.fetchone()
        recipe = recipe_from_snapshot(w['snapshot'], 'reviewer_review')
        if (not critic or critic['mode'] != 'review' or critic['status'] != 'completed'
                or critic['workflow_id'] != w['workflow_id']
                or critic['workflow_review_id'] != peer['workflow_review_id']
                or critic['latest_attempt_id'] != peer['reviewer_attempt_id']
                or critic['review_target_sha256'] != peer['target_sha256']
                or critic['agent_profile'] != w['snapshot']['reviewer_profile']
                or critic['agent_profile'] == task['agent_profile']
                or critic['recipe_digest'] != recipe.digest or critic['pinned_recipe'] != recipe.to_dict()):
            return False
        try:
            result = self.flow._artifact(cursor, critic['id'], peer['reviewer_attempt_id'])
            if result['content'] != peer['verdict_json'] or result['result_sha256'] != peer['verdict_sha256']:
                return False
            target = parse_review_target(peer['target_json'], peer['target_sha256'])
            verdict = validate_live_review_verdict(peer['verdict_json'], target)
            if (verdict['decision'] != 'revise' or target['source_task_id'] != str(task['id'])
                    or target['review_id'] != str(review['review_id'])
                    or target['integration_base'] != review['verification'].get('integration_base')
                    or target['source_attempt_id'] != str(packet['parent_attempt_id'])
                    or target['source_result_sha256'] != source['result_sha256']
                    or target['review_id'] == str(attempt['repair_source_review_id'])):
                return False
            cursor.execute('SELECT * FROM forge_agent.coding_plan_decisions WHERE '
                '(owner_id,project_alias,decision_id)=(%s,%s,%s)',
                (self.store.owner, task['project_alias'], task['plan_decision_id'] or task['scope_plan_decision_id']))
            plan = cursor.fetchone()
            if (not plan or plan['decision'] != 'resume' or target['plan_decision_id'] != str(plan['decision_id'])
                    or target['approved_plan']['attempt_id'] != str(plan['plan_attempt_id'])
                    or target['approved_plan']['sha256'] != plan['plan_sha256']
                    or self.flow._artifact(cursor, plan['task_id'], plan['plan_attempt_id'])['content']
                        != target['approved_plan']['content']):
                return False
            lease = {key: str(attempt[key]) if attempt[key] is not None else None for key in
                ('task_id', 'workflow_id', 'workflow_feedback_review_id', 'workflow_feedback_sha256',
                 'repair_source_review_id')}
            lease.update(mode=task['mode'], agent_profile=task['agent_profile'],
                parent_task_id=str(task['parent_task_id']), base_revision=task['base_revision'])
            context = ReviewContext(packet['feedback_json'].encode(), packet['feedback_sha256'],
                str(packet['feedback_id']), tuple(item['finding_id'] for item in feedback['findings']))
            envelope = {key: peer[key] for key in
                ('target_json', 'target_sha256', 'verdict_json', 'verdict_sha256')}
            validated = validate_workflow_feedback(lease, envelope,
                plan_context=dict(plan_attempt_id=str(plan['plan_attempt_id']), plan_sha256=plan['plan_sha256']),
                feedback_context=context)
            return (validated.sha256 == peer['verdict_sha256']
                and hashlib.sha256(validated.content).hexdigest() == peer['verdict_sha256'])
        except (ValueError, KeyError, TypeError, AttributeError, UnicodeError):
            return False

    def run_once(self):
        if self.flow.retry_events is None:
            return False
        return super().run_once()


@dataclass(frozen=True)
class PeerFeedbackRecoveryFactory:
    flow: object

    def create(self):
        config = self.flow.config
        return PeerFeedbackRecovery(self.flow) if config is not None and config.project_workflows else None
