"""Pure, versioned source-content and verification identities.

An artifact names exact relative paths, executable flags and file bytes. Source
locations, SCM object names and presentation patches never enter this contract.
"""
from dataclasses import dataclass, field
import hashlib
import json
import math
import re
from uuid import UUID, uuid5

ARTIFACT_DOMAIN = 'forge.source-artifact'
POLICY_DOMAIN = 'forge.verification-policy'
VERIFICATION_DOMAIN = 'forge.verification'
VERSION = 1
MAX_FILES = 20000
MAX_PATH_BYTES = 1024
MAX_FILE_BYTES = 64 * 1024 * 1024
MAX_TOTAL_BYTES = 512 * 1024 * 1024
MAX_MANIFEST_BYTES = 32 * 1024 * 1024
MAX_POLICY_BYTES = 4 * 1024 * 1024
MODES = frozenset({'100644', '100755'})
_SHA256 = re.compile(r'[0-9a-f]{64}')


def canonical_json(value):
    """Version-one UTF-8 JSON: sorted keys, no whitespace or nonfinite numbers."""
    return json.dumps(value, sort_keys=True, ensure_ascii=False, allow_nan=False,
                      separators=(',', ':')).encode('utf-8')


def _json_bytes(raw, maximum):
    if not isinstance(raw, bytes) or len(raw) > maximum:
        raise ValueError('Invalid identity JSON size')
    def pairs(items):
        result = {}
        for key, value in items:
            if key in result:
                raise ValueError('Duplicate identity JSON field')
            result[key] = value
        return result
    def invalid(_value):
        raise ValueError('Nonfinite identity JSON number')
    try:
        return json.loads(raw.decode('utf-8'), object_pairs_hook=pairs, parse_constant=invalid)
    except (UnicodeError, RecursionError) as error:
        raise ValueError('Invalid identity JSON') from error


def validate_path(path):
    if (not isinstance(path, str) or not path or path.startswith('/') or '\\' in path
            or any(part in {'', '.', '..'} for part in path.split('/'))
            or any(ord(char) < 32 or 127 <= ord(char) <= 159 for char in path)):
        raise ValueError('Invalid artifact relative path')
    try:
        if len(path.encode('utf-8')) > MAX_PATH_BYTES:
            raise ValueError('Artifact path is too long')
    except UnicodeError as error:
        raise ValueError('Invalid artifact path encoding') from error
    return path


def _digest(value):
    if not isinstance(value, str) or not _SHA256.fullmatch(value):
        raise ValueError('Invalid content SHA-256')
    return value


@dataclass(frozen=True)
class ArtifactEntry:
    path: str
    mode: str
    size: int
    sha256: str

    def __post_init__(self):
        validate_path(self.path)
        if (not isinstance(self.mode, str) or self.mode not in MODES or type(self.size) is not int
                or not 0 <= self.size <= MAX_FILE_BYTES):
            raise ValueError('Unsupported artifact entry')
        _digest(self.sha256)

    def to_dict(self):
        return {'path': self.path, 'mode': self.mode, 'size': self.size, 'sha256': self.sha256}

    @classmethod
    def from_dict(cls, value):
        if not isinstance(value, dict) or set(value) != {'path', 'mode', 'size', 'sha256'}:
            raise ValueError('Invalid artifact entry fields')
        return cls(**value)


@dataclass(frozen=True)
class ArtifactManifest:
    entries: tuple

    def __post_init__(self):
        if not isinstance(self.entries, (tuple, list)) or len(self.entries) > MAX_FILES:
            raise ValueError('Invalid artifact entry count')
        if any(not isinstance(entry, ArtifactEntry) for entry in self.entries):
            raise ValueError('Expected typed artifact entries')
        entries = tuple(sorted(self.entries, key=lambda item: item.path.encode('utf-8')))
        names = set()
        total = 0
        for entry in entries:
            if entry.path in names or any(entry.path[:i] in names
                                          for i, char in enumerate(entry.path) if char == '/'):
                raise ValueError('Duplicate or conflicting artifact paths')
            names.add(entry.path)
            total += entry.size
            if total > MAX_TOTAL_BYTES:
                raise ValueError('Artifact exceeds total byte limit')
        object.__setattr__(self, 'entries', entries)

    def to_dict(self):
        return {'domain': ARTIFACT_DOMAIN, 'version': VERSION,
                'files': [entry.to_dict() for entry in self.entries]}

    def canonical_bytes(self):
        return canonical_json(self.to_dict())

    @property
    def artifact_id(self):
        return hashlib.sha256(self.canonical_bytes()).hexdigest()

    @classmethod
    def from_dict(cls, value):
        if (not isinstance(value, dict) or set(value) != {'domain', 'version', 'files'}
                or value['domain'] != ARTIFACT_DOMAIN or type(value['version']) is not int
                or value['version'] != VERSION or not isinstance(value['files'], list)
                or len(value['files']) > MAX_FILES):
            raise ValueError('Invalid artifact manifest')
        return cls(tuple(ArtifactEntry.from_dict(entry) for entry in value['files']))

    @classmethod
    def from_bytes(cls, raw):
        return cls.from_dict(_json_bytes(raw, MAX_MANIFEST_BYTES))


def _policy_value(value, depth=0, remaining=None):
    if remaining is None:
        remaining = [MAX_FILES]
    remaining[0] -= 1
    if depth > 32 or remaining[0] < 0:
        raise ValueError('Verification policy is too complex')
    if value is None or type(value) in (str, bool, int):
        return value
    if type(value) is float and math.isfinite(value):
        return value
    if isinstance(value, (tuple, list)):
        return [_policy_value(item, depth + 1, remaining) for item in value]
    if isinstance(value, dict) and all(isinstance(key, str) for key in value):
        return {key: _policy_value(item, depth + 1, remaining) for key, item in value.items()}
    raise ValueError('Unsupported verification policy value')


@dataclass(frozen=True)
class VerificationPolicySnapshot:
    _content: bytes = field(repr=False)

    def __post_init__(self):
        value = _json_bytes(self._content, MAX_POLICY_BYTES)
        if (not isinstance(value, dict) or set(value) != {'domain', 'version', 'policy'}
                or value['domain'] != POLICY_DOMAIN or type(value['version']) is not int
                or value['version'] != VERSION or not isinstance(value['policy'], dict)):
            raise ValueError('Invalid verification policy snapshot')
        content = canonical_json({**value, 'policy': _policy_value(value['policy'])})
        if len(content) > MAX_POLICY_BYTES:
            raise ValueError('Verification policy exceeds byte limit')
        object.__setattr__(self, '_content', content)

    @classmethod
    def from_dict(cls, policy):
        """Capture a consumer-owned policy body, not an already wrapped envelope."""
        if not isinstance(policy, dict):
            raise ValueError('Expected verification policy object')
        return cls(canonical_json({'domain': POLICY_DOMAIN, 'version': VERSION,
                                  'policy': _policy_value(policy)}))

    from_policy = from_dict

    @classmethod
    def parse_dict(cls, envelope):
        return cls(canonical_json(envelope))

    @classmethod
    def from_bytes(cls, raw):
        return cls(raw)

    def to_dict(self):
        return _json_bytes(self._content, MAX_POLICY_BYTES)

    def canonical_bytes(self):
        return self._content

    @property
    def policy_sha256(self):
        return hashlib.sha256(self._content).hexdigest()


@dataclass(frozen=True)
class VerificationIdentity:
    artifact_id: str
    policy_sha256: str
    generation: UUID

    def __post_init__(self):
        _digest(self.artifact_id)
        _digest(self.policy_sha256)
        if not isinstance(self.generation, UUID) or self.generation.int == 0:
            raise ValueError('Invalid verification generation')

    def to_dict(self):
        return {'domain': VERIFICATION_DOMAIN, 'version': VERSION,
                'artifact_id': self.artifact_id, 'policy_sha256': self.policy_sha256,
                'generation': str(self.generation)}

    def canonical_bytes(self):
        return canonical_json(self.to_dict())

    @property
    def verification_id(self):
        return hashlib.sha256(self.canonical_bytes()).hexdigest()

    @property
    def verification_uuid(self):
        """Typed identity: the generation UUID namespaces the artifact/policy pair.

        No JSON is re-encoded or hashed; the two inputs are content hashes of
        file bytes and the stored policy bytes."""
        return uuid5(self.generation, VERIFICATION_DOMAIN + ':' + str(VERSION) + ':'
                     + self.artifact_id + ':' + self.policy_sha256)

    def matches_id(self, value):
        """Accept the typed UUID form or the legacy hex form of this identity."""
        if not isinstance(value, str):
            return False
        if _SHA256.fullmatch(value):
            return value == self.verification_id
        try:
            return str(UUID(value)) == value and UUID(value) == self.verification_uuid
        except ValueError:
            return False

    @classmethod
    def from_dict(cls, value):
        if (not isinstance(value, dict) or set(value) != {'domain', 'version', 'artifact_id', 'policy_sha256', 'generation'}
                or value['domain'] != VERIFICATION_DOMAIN or type(value['version']) is not int
                or value['version'] != VERSION or not isinstance(value['generation'], str)):
            raise ValueError('Invalid verification identity')
        generation = UUID(value['generation'])
        if str(generation) != value['generation']:
            raise ValueError('Noncanonical verification generation')
        return cls(value['artifact_id'], value['policy_sha256'], generation)

    @classmethod
    def from_bytes(cls, raw):
        return cls.from_dict(_json_bytes(raw, 2048))
