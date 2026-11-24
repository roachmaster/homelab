"""Pure JSONL decoding at native CLI transport boundaries.

Codex0.160 flattens WebSearchItem into ThreadItem, serializing both of their
``id`` fields. Keep the outer item identity only for that exact wire shape.
Review artifacts and every other transport field retain unique-key parsing.
"""
import json
import re
from uuid import UUID

CODEX_ITEM_EVENTS = frozenset({'item.started', 'item.updated', 'item.completed'})
OUTER_ITEM_ID = re.compile(r'item_[0-9]+')
SAFE_FIELDS = frozenset({'type', 'item', 'id', 'thread_id', 'text', 'usage',
    'input_tokens', 'output_tokens', 'cached_input_tokens', 'query', 'action',
    'results', 'command', 'aggregated_output', 'exit_code', 'status', 'error',
    'message', 'code', 'result', 'structured_content', 'content'})
REASONS = frozenset({'invalid_utf8', 'nonfinite_number', 'nonobject_record',
    'invalid_json', 'nesting_bound', 'duplicate_field'})
DIAGNOSTIC_FIELDS = frozenset({'version', 'backend', 'reason', 'path',
    'record_ordinal', 'record_byte_count', 'record_sha256'})
SAFE_PATH = re.compile(r'\$(?:(?:\.(?:' + '|'.join(sorted(SAFE_FIELDS)) + r'|\*))|\[\]){0,8}')


class CliTransportError(ValueError):
    """Fixed structural diagnostics; never retains raw record content."""
    def __init__(self, reason, path=()):
        self.reason = reason
        self.path = '$' + ''.join('[]' if type(part) is int else
            '.' + (part if part in SAFE_FIELDS else '*') for part in path[:8])
        super().__init__('Invalid CLI transport record')


class _ObjectPairs(tuple):
    """Distinguishes object pairs from JSON arrays until path-aware validation."""


def _discriminator(pairs):
    values = [value for key, value in pairs if key == 'type']
    return values[0] if len(values) == 1 and isinstance(values[0], str) else None


def _value(value, *, backend, event_type, path=()):
    if isinstance(value, _ObjectPairs):
        ids = [item for key, item in value if key == 'id']
        web_alias = (backend == 'codex' and event_type in CODEX_ITEM_EVENTS
            and path == ('item',) and _discriminator(value) == 'web_search'
            and len(ids) == 2 and all(isinstance(item, str) for item in ids)
            and OUTER_ITEM_ID.fullmatch(ids[0]) is not None)
        result = {}
        for key, item in value:
            if key in result:
                if web_alias and key == 'id':
                    # The provider's second opaque search ID has no authority
                    # over sessions, task identity or the final model response.
                    continue
                raise CliTransportError('duplicate_field', (*path, key))
            result[key] = _value(item, backend=backend, event_type=event_type, path=(*path, key))
        return result
    if isinstance(value, list):
        return [_value(item, backend=backend, event_type=event_type, path=(*path, index))
            for index, item in enumerate(value)]
    return value


def parse_record(record, backend):
    """Accept one complete UTF-8 record with a tightly scoped Codex wire alias."""
    try:
        text = record.decode('utf-8')
    except UnicodeError:
        raise CliTransportError('invalid_utf8') from None
    def nonfinite(_value):
        raise CliTransportError('nonfinite_number')
    try:
        value = json.loads(text, object_pairs_hook=_ObjectPairs, parse_constant=nonfinite)
        if not isinstance(value, _ObjectPairs):
            raise CliTransportError('nonobject_record')
        return _value(value, backend=backend, event_type=_discriminator(value))
    except CliTransportError:
        raise
    except ValueError:
        raise CliTransportError('invalid_json') from None
    except RecursionError:
        raise CliTransportError('nesting_bound') from None


class CliDiagnosticReporter:
    """Ready private logging port selected by the coding-worker factory."""
    def __init__(self, logger):
        if not callable(getattr(logger, 'warning', None)):
            raise TypeError('CLI diagnostics require a ready logger')
        self.logger = logger

    def report(self, lease, diagnostic):
        if (not isinstance(diagnostic, dict) or set(diagnostic) != DIAGNOSTIC_FIELDS
                or type(diagnostic['version']) is not int or diagnostic['version'] != 1
                or diagnostic['backend'] not in {'codex', 'antigravity', 'grok', 'muse'}
                or diagnostic['reason'] not in REASONS
                or not isinstance(diagnostic['path'], str) or not SAFE_PATH.fullmatch(diagnostic['path'])
                or not isinstance(diagnostic['record_sha256'], str)
                or not re.fullmatch('[0-9a-f]{64}', diagnostic['record_sha256'])
                or any(type(diagnostic[name]) is not int or not 1 <= diagnostic[name] <= 67108864
                    for name in ('record_ordinal', 'record_byte_count'))):
            raise ValueError('Invalid structural CLI diagnostic')
        identity = {key: lease[key] for key in ('task_id', 'attempt_id')}
        if any(not isinstance(value, str) or str(UUID(value)) != value for value in identity.values()):
            raise ValueError('Invalid diagnostic attempt identity')
        self.logger.warning('cli_transport_failure %s',
            json.dumps({**diagnostic, **identity}, sort_keys=True, separators=(',', ':')))
