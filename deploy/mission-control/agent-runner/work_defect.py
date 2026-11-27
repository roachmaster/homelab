"""Pure defect filing contract; no admission or workflow authority."""
import json
import re
from uuid import UUID

from queue_store import Rejected

FAILURE_REASONS = frozenset('defect.' + field for field in (
    'task_kind', 'task_kind_filter', 'unexpected_payload', 'fields', 'summary', 'location',
    'location.repo_path', 'location.function', 'location.commit', 'observed_behavior',
    'expected_behavior', 'severity', 'evidence', 'discovering_task_id', 'discovering_workflow_id'))

TRIAGE = ('Defect Plan triage: reproduce the malfunction, identify the root cause or remaining uncertainty, '
          'and recommend fix or defer with reasons. Use the normal review and decision process; '
          'a defer recommendation does not cancel or bypass implementation authority.')


def validate_defect(task_kind='feature', defect=None):
    def invalid(field):
        raise Rejected(400, 'invalid_request', 'defect.' + field)
    if task_kind not in ('feature', 'defect'):
        invalid('task_kind')
    if task_kind == 'feature':
        if defect is not None:
            invalid('unexpected_payload')
        return None
    required = {'summary', 'location', 'observed_behavior', 'expected_behavior', 'severity', 'evidence'}
    optional = {'discovering_task_id', 'discovering_workflow_id'}
    if not isinstance(defect, dict):
        invalid('fields')
    for field in sorted(required - defect.keys()):
        invalid(field)
    if defect.keys() - required - optional:
        invalid('fields')
    def text(value, field, maximum):
        if (not isinstance(value, str) or not value.strip() or len(value.encode('utf-8', errors='surrogatepass')) > maximum
                or any((ord(c) < 32 and c not in '\t\n') or 127 <= ord(c) <= 159 or 0xD800 <= ord(c) <= 0xDFFF for c in value)):
            invalid(field)
    for key, maximum in [('summary',512), ('observed_behavior',4096), ('expected_behavior',4096), ('evidence',8192)]:
        text(defect[key], key, maximum)
    location = defect['location']
    if not isinstance(location, dict):
        invalid('location')
    for field in sorted({'repo_path','function','commit'} - location.keys()):
        invalid('location.' + field)
    if set(location) != {'repo_path','function','commit'}:
        invalid('location')
    text(location['repo_path'], 'location.repo_path', 1024)
    text(location['function'], 'location.function', 512)
    if any(ord(c) < 32 or 127 <= ord(c) <= 159 for key in ('repo_path','function') for c in location[key]):
        invalid('location')
    if location['repo_path'].startswith('/') or '\\' in location['repo_path'] or any(part in ('', '.', '..') for part in location['repo_path'].split('/')):
        invalid('location.repo_path')
    if not isinstance(location['commit'],str) or re.fullmatch('[0-9a-f]{40}', location['commit']) is None:
        invalid('location.commit')
    if not isinstance(defect['severity'], str) or defect['severity'] not in {'low','medium','high','critical'}:
        invalid('severity')
    for key in optional & defect.keys():
        try:
            if not isinstance(defect[key], str) or str(UUID(defect[key])) != defect[key]:
                invalid(key)
        except (ValueError, TypeError, AttributeError):
            invalid(key)
    return json.loads(json.dumps(defect))


def defect_context(task):
    if task.get('task_kind', 'feature') != 'defect':
        return {}
    return {'task_kind':'defect', 'defect':task['defect'], 'triage_instructions':TRIAGE}


def validate_defect_context(value):
    """Validate an optional, completely bound extension on immutable targets."""
    fields = {'task_kind', 'defect', 'triage_instructions'}
    present = fields & value.keys()
    if not present:
        return
    if present != fields or value['task_kind'] != 'defect' or value['triage_instructions'] != TRIAGE:
        raise ValueError('Invalid defect review context')
    try:
        validate_defect(value['task_kind'],value['defect'])
    except Rejected as error:
        raise ValueError('Invalid defect review context') from error
