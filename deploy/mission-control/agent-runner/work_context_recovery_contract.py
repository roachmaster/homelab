"""Pure bindings for a server-issued native feedback-delivery continuation."""
import json
import re
from uuid import UUID

from work_policy import native_policy_matches

POLICY = 'forge-native-feedback-recovery-v1'
PREFIX = 'FORGE_FEEDBACK_RECOVERY_V1:'
RECOVERY_SUMMARY = 'Trusted build feedback is temporarily unavailable.'
LEGACY_SUMMARY = 'The current review feedback could not be verified.'
UUID_FIELDS = frozenset({'workflow_id', 'task_id', 'stopped_attempt_id', 'feedback_id',
                         'feedback_parent_attempt_id', 'reconciliation_receipt_id'})
DIGEST_FIELDS = frozenset({'feedback_sha256', 'workspace_digest'})
FIELDS = UUID_FIELDS | DIGEST_FIELDS | {'policy', 'base_revision'}


def _validate(value):
    if not isinstance(value, dict) or set(value) != FIELDS or value['policy'] != POLICY:
        raise ValueError('Invalid feedback recovery identity')
    for key in UUID_FIELDS:
        if not isinstance(value[key], str) or str(UUID(value[key])) != value[key]:
            raise ValueError('Invalid feedback recovery UUID')
    for key in DIGEST_FIELDS:
        if not isinstance(value[key], str) or not re.fullmatch('[0-9a-f]{64}', value[key]):
            raise ValueError('Invalid feedback recovery digest')
    if not isinstance(value['base_revision'], str) or not re.fullmatch('[0-9a-f]{40}', value['base_revision']):
        raise ValueError('Invalid feedback recovery source')
    return value


def make_recovery_note(*, workflow_id, task_id, stopped_attempt_id, feedback_id,
                       feedback_sha256, feedback_parent_attempt_id, workspace_digest,
                       reconciliation_receipt_id, base_revision):
    value = _validate(dict(policy=POLICY, workflow_id=str(workflow_id), task_id=str(task_id),
        stopped_attempt_id=str(stopped_attempt_id), feedback_id=str(feedback_id),
        feedback_sha256=feedback_sha256, feedback_parent_attempt_id=str(feedback_parent_attempt_id),
        workspace_digest=workspace_digest, reconciliation_receipt_id=str(reconciliation_receipt_id),
        base_revision=base_revision))
    note = PREFIX + json.dumps(value, sort_keys=True, separators=(',', ':'), allow_nan=False)
    if len(note.encode()) > 2048:
        raise ValueError('Oversized feedback recovery identity')
    return note


def validate_recovery_note(note, lease, feedback):
    """Accept only authenticated native resume metadata, never a model artifact.

    The caller must already have authenticated the worker feedback envelope,
    verified its exact immutable bytes/hash and compared the actual workspace.
    Native owner/client Resume cannot issue this marker: its separate ingress
    always constructs a different fixed server prefix.
    """
    try:
        if (not isinstance(note, str) or not note.startswith(PREFIX)
                or len(note.encode()) > 2048 or not isinstance(lease, dict)
                or not isinstance(feedback, dict) or lease.get('mode') != 'implement'
                or lease.get('workflow_id') is None or lease.get('workflow_review_id') is not None
                or feedback.get('workspace_digest_kind') != 'native_receipt'):
            return False
        recipe = lease.get('recipe', {})
        plan = lease.get('plan_decision_id') or lease.get('scope_plan_decision_id')
        if (recipe.get('version') != 2 or type(recipe.get('version')) is not int
                or not native_policy_matches(recipe.get('resource_profile'), recipe.get('backend'), recipe.get('policy_digest'))
                or plan is None or feedback.get('plan_decision_id') != plan):
            return False
        value = _validate(json.loads(note[len(PREFIX):]))
        if note != PREFIX + json.dumps(value, sort_keys=True, separators=(',', ':'), allow_nan=False):
            return False
        expected = {
            'workflow_id': lease['workflow_id'], 'task_id': lease['task_id'],
            'stopped_attempt_id': lease['resume_attempt_id'], 'feedback_id': lease['feedback_id'],
            'feedback_sha256': lease['feedback_sha256'],
            'feedback_parent_attempt_id': feedback['parent_attempt_id'],
            'workspace_digest': lease['resume_workspace_digest'], 'base_revision': lease['base_revision'],
        }
        return (all(value[k] == v for k, v in expected.items())
            and value['feedback_id'] == feedback['feedback_id']
            and value['task_id'] == feedback['task_id']
            and value['workspace_digest'] == feedback['workspace_digest'])
    except (ValueError, TypeError, KeyError, AttributeError, UnicodeError, RecursionError):
        return False
