"""Attempt-scoped immutable Jenkins inspection; bootstrap injects all artifact roots."""
from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path
import re
import stat
from uuid import UUID

from work_verification import diagnostic_text

MAX_LOG = 64 * 1024 * 1024


def read_regular(path, *, owner, maximum=65536):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, 'rb') as stream:
        info = os.fstat(stream.fileno())
        if (not stat.S_ISREG(info.st_mode) or info.st_nlink != 1
                or info.st_uid != owner or info.st_mode & 0o022 or info.st_size > maximum):
            raise ValueError('Unsafe build inspection artifact')
        raw = stream.read(maximum + 1)
    if len(raw) > maximum:
        raise ValueError('Oversized build inspection artifact')
    return raw


def _uuid(value):
    if not isinstance(value, str) or str(UUID(value)) != value:
        raise ValueError('Invalid build identity')
    return value


def _sha(value, length=64):
    if not isinstance(value, str) or re.fullmatch('[0-9a-f]{' + str(length) + '}', value) is None:
        raise ValueError('Invalid build source digest')
    return value


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(',', ':'), ensure_ascii=False).encode()


BUILD_INFO_SCRIPT = b'''"""Read the current attempt's immutable Jenkins evidence without credentials."""
import argparse,hashlib,json,os,stat
from pathlib import Path
root=Path(__file__).parent
def read(name,limit):
 fd=os.open(root/name,os.O_RDONLY|os.O_NOFOLLOW|os.O_NONBLOCK)
 with os.fdopen(fd,'rb') as f:
  s=os.fstat(f.fileno())
  if not stat.S_ISREG(s.st_mode) or s.st_nlink!=1 or s.st_size>limit:
   raise SystemExit('Invalid immutable build artifact')
  data=f.read(limit+1)
 if len(data)>limit:raise SystemExit('Oversized build artifact')
 return data
parser=argparse.ArgumentParser(description=__doc__)
sub=parser.add_subparsers(dest='command',required=True)
sub.add_parser('status')
logs=sub.add_parser('logs')
logs.add_argument('--offset',type=int,default=0)
logs.add_argument('--limit',type=int,default=16384)
args=parser.parse_args()
metadata=json.loads(read('build-info.json',65536))
if args.command=='status':
 print(json.dumps(metadata,sort_keys=True))
else:
 if not 0<=args.offset<=67108864 or not 1<=args.limit<=65536:
  parser.error('offset must be 0..67108864; limit must be 1..65536 bytes')
 data=read('build.log',67108864)
 if hashlib.sha256(data).hexdigest()!=metadata['sanitized_log_sha256']:
  raise SystemExit('Immutable build log digest mismatch')
 end=min(args.offset+args.limit,len(data))
 if args.offset>len(data):parser.error('offset exceeds log size')
 print(json.dumps({'offset':args.offset,'next_offset':end,'eof':end==len(data),
  'text':data[args.offset:end].decode('utf-8','replace')},ensure_ascii=False))
'''


@dataclass(frozen=True)
class BuildContext:
    metadata: bytes
    log: bytes
    feedback_id: str
    feedback_sha256: str

    def validate(self, feedback_id, feedback_sha256):
        if (self.feedback_id != feedback_id or self.feedback_sha256 != feedback_sha256
                or not isinstance(self.metadata, bytes) or not 1 <= len(self.metadata) <= 65536
                or not isinstance(self.log, bytes) or len(self.log) > MAX_LOG):
            raise ValueError('Build context is not bound to this repair')
        value = json.loads(self.metadata)
        if (value['feedback_id'] != feedback_id or value['feedback_sha256'] != feedback_sha256
                or hashlib.sha256(self.log).hexdigest() != value['sanitized_log_sha256']):
            raise ValueError('Build context changed before staging')
        return self


class BuildContextProvider:
    """One worker-owned read port; model receives only validated bytes and a CLI."""
    def __init__(self, store, state_root, results_root, *, reader=read_regular):
        self.store = store
        self.publication = Path(state_root) / 'publication'
        self.results = Path(results_root)
        self.read = reader

    def acquire(self, lease, feedback):
        envelope = self.store.read_native_delivery_context(lease)
        if envelope is None:
            return None
        if (not isinstance(envelope, dict) or set(envelope) != {
                'feedback_id', 'feedback_sha256', 'review_id', 'source_task_id',
                'source_attempt_id', 'candidate_commit', 'gate_manifest_sha256', 'delivery_failure'}
                or envelope['feedback_id'] != feedback.feedback_id
                or envelope['feedback_sha256'] != feedback.sha256):
            raise ValueError('Build inspection source does not match repair feedback')
        parsed = json.loads(feedback.content)
        if (envelope['source_attempt_id'] != parsed['parent_attempt_id']
                or envelope['candidate_commit'] != parsed['candidate_commit']
                or envelope['gate_manifest_sha256'] != parsed['gate_manifest_sha256']):
            raise ValueError('Build inspection source changed')
        review = _uuid(envelope['review_id'])
        manifest = json.loads(self.read(self.publication / review / 'review.json', owner=os.geteuid()))
        expected = {'review_id': review, 'task_id': _uuid(envelope['source_task_id']),
            'attempt_id': _uuid(envelope['source_attempt_id']),
            'candidate_commit': _sha(envelope['candidate_commit'], 40),
            'gate_manifest_sha256': _sha(envelope['gate_manifest_sha256']),
            'delivery_failure': envelope['delivery_failure']}
        if any(manifest.get(key) != value for key, value in expected.items()):
            raise ValueError('Build inspection manifest changed')
        failure = envelope['delivery_failure']
        if (not isinstance(failure, dict) or set(failure) != {
                'version', 'tested_candidate', 'current_main', 'build', 'conflict_paths'}
                or type(failure['version']) is not int or failure['version'] != 1):
            raise ValueError('Invalid delivery failure reference')
        _sha(failure['tested_candidate'], 40); _sha(failure['current_main'], 40)
        build = failure['build']
        if (not isinstance(build, dict) or set(build) != {
                'provider', 'job', 'action_id', 'queue_id', 'build_number'}
                or build['provider'] != 'jenkins' or build['job'] != 'forge-native-candidate'
                or type(build['build_number']) is not int or not 1 <= build['build_number'] < 2**63
                or build['queue_id'] is not None and (
                    type(build['queue_id']) is not int or not 1 <= build['queue_id'] < 2**63)):
            raise ValueError('Invalid Jenkins build reference')
        action = _uuid(build['action_id'])
        action_root = self.publication / 'native-ci/actions' / action
        binding = json.loads(self.read(action_root / 'native-binding.json', owner=os.geteuid()))
        observation = json.loads(self.read(action_root / 'observation.json', owner=os.geteuid()))
        receipt = observation.get('receipt', {})
        request = binding['request']
        scope = binding.get('scope', request)
        if (scope.get('task_id') != envelope['source_task_id']
                or scope.get('attempt_id') != envelope['source_attempt_id']
                or scope.get('project_alias') != lease['project_alias']
                or request['action_id'] != action or request['workflow_id'] != lease['workflow_id']
                or request['candidate_commit'] != failure['tested_candidate']
                or any(receipt.get(k) != v for k, v in {'action_id': action,
                    'job': build['job'], 'build_number': build['build_number'], 'queue_id': build['queue_id']}.items())):
            raise ValueError('Jenkins observation is not this build')
        raw = self.read(self.results / (action + '.json'), owner=0)
        if hashlib.sha256(raw).hexdigest() != receipt.get('check_receipt_sha256'):
            raise ValueError('Trusted build receipt changed')
        result = json.loads(raw)
        for key in ('action_id', 'workflow_id', 'candidate_commit', 'candidate_tree',
                    'gate_manifest_sha256', 'correlation_id'):
            if result.get(key) != request.get(key):
                raise ValueError('Trusted build receipt source mismatch')
        if result.get('expected_main') != request['integration_base']:
            raise ValueError('Trusted build base mismatch')
        log = self.read(self.results / (action + '.log'), owner=0, maximum=MAX_LOG)
        if hashlib.sha256(log).hexdigest() != result.get('log_sha256') or result.get('log_sha256') != receipt.get('log_sha256'):
            raise ValueError('Trusted build log changed')
        safe_log = diagnostic_text(log.decode('utf-8', 'replace'), MAX_LOG).encode()
        metadata = {'version': 1, 'feedback_id': feedback.feedback_id,
            'feedback_sha256': feedback.sha256, 'workflow_id': lease['workflow_id'],
            'task_id': lease['task_id'], 'source_review_id': review,
            'source_task_id': envelope['source_task_id'], 'source_attempt_id': envelope['source_attempt_id'],
            'build': build, 'tested_candidate': result['candidate_commit'],
            'tested_tree': result['candidate_tree'], 'expected_main_at_build': result['expected_main'],
            'current_main': failure['current_main'], 'conflict_paths': failure['conflict_paths'],
            'gate_manifest_sha256': result['gate_manifest_sha256'], 'build_status': result.get('status'),
            'root_receipt_sha256': hashlib.sha256(raw).hexdigest(),
            'trusted_log_sha256': result['log_sha256'], 'sanitized_log_sha256': hashlib.sha256(safe_log).hexdigest(),
            'log_bytes': len(safe_log)}
        return BuildContext(canonical(metadata), safe_log, feedback.feedback_id, feedback.sha256).validate(
            feedback.feedback_id, feedback.sha256)
