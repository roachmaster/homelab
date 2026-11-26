"""Verification identities bind by typed fields; saved snapshots are not re-hashed on read."""
import hashlib
import unittest
from uuid import UUID, uuid4

from work_artifact_identity import ArtifactEntry, ArtifactManifest, VerificationIdentity
from work_external_identity import ExternalCandidateIdentityProvider, validate_content_identity
from work_native_identity import VERSION as NATIVE_VERSION, NativeVerificationIdentityProvider


def entry(path, content):
    return ArtifactEntry(path, '100644', len(content), hashlib.sha256(content).hexdigest())


class Artifacts:
    def __init__(self, manifests):
        self.manifests = manifests

    def acquire(self, repository, commit):
        return self.manifests[commit]


class VerificationIdTest(unittest.TestCase):
    def setUp(self):
        self.identity = VerificationIdentity('a' * 64, 'b' * 64, uuid4())

    def test_typed_uuid_is_deterministic_and_scoped_by_every_field(self):
        same = VerificationIdentity(self.identity.artifact_id, self.identity.policy_sha256, self.identity.generation)
        self.assertEqual(same.verification_uuid, self.identity.verification_uuid)
        for other in (VerificationIdentity('c' * 64, 'b' * 64, self.identity.generation),
                      VerificationIdentity('a' * 64, 'c' * 64, self.identity.generation),
                      VerificationIdentity('a' * 64, 'b' * 64, uuid4())):
            self.assertNotEqual(other.verification_uuid, self.identity.verification_uuid)
        self.assertEqual(self.identity.verification_uuid.version, 5)

    def test_matches_typed_or_legacy_form_only(self):
        self.assertTrue(self.identity.matches_id(self.identity.verification_id))
        self.assertTrue(self.identity.matches_id(str(self.identity.verification_uuid)))
        other = VerificationIdentity('c' * 64, 'b' * 64, self.identity.generation)
        for value in (other.verification_id, str(other.verification_uuid),
                      str(self.identity.verification_uuid).upper(), 'f' * 64, '', None, 'not-a-uuid'):
            self.assertFalse(self.identity.matches_id(value), value)


class ExternalReferenceTest(unittest.TestCase):
    def setUp(self):
        self.artifacts = Artifacts({
            'head': ArtifactManifest((entry('a.txt', b'a'), entry('gate/Makefile', b'm'))),
            'base': ArtifactManifest((entry('a.txt', b'old'), entry('gate/Makefile', b'm')))})
        self.provider = ExternalCandidateIdentityProvider(self.artifacts)
        self.snapshot, self.reference = self.provider.prepare(
            None, 'head', 'base', {'profile': 'forge-sdlc-jenkins-v3'}, str(uuid4()),
            lambda path: path.startswith('gate/'))

    def test_reference_accepts_the_typed_verification_uuid(self):
        verification = VerificationIdentity.from_dict(self.reference['verification'])
        typed = {**self.reference, 'verification_id': str(verification.verification_uuid)}
        self.assertEqual(validate_content_identity(typed), typed)
        self.provider.validate_snapshot(self.snapshot, typed)

    def test_reference_with_another_identity_is_rejected(self):
        forged = {**self.reference, 'verification_id': str(uuid4())}
        with self.assertRaises(ValueError):
            validate_content_identity(forged)

    def test_snapshot_is_compared_structurally_not_by_rehash(self):
        stale_hash = {**self.reference, 'manifest_sha256': '0' * 64}
        self.provider.validate_snapshot(self.snapshot, stale_hash)
        changed = {**self.snapshot, 'base_artifact': ArtifactManifest((entry('a.txt', b'x'),)).to_dict()}
        with self.assertRaises(ValueError):
            self.provider.validate_snapshot(changed, self.reference)


class NativeReferenceTest(unittest.TestCase):
    def setUp(self):
        self.review = str(uuid4())
        identity = VerificationIdentity('a' * 64, 'b' * 64, UUID(self.review))
        self.snapshot = {'scope': {'review_id': self.review}, 'identity': identity.to_dict()}
        self.identity = identity
        self.reference = {'version': NATIVE_VERSION, 'generation': self.review, 'artifact_id': 'a' * 64,
                          'policy_sha256': 'b' * 64, 'verification_id': identity.verification_id,
                          'snapshot_sha256': 'c' * 64}

    def test_binds_typed_fields_and_ignores_snapshot_rehash(self):
        matches = NativeVerificationIdentityProvider.reference_matches
        self.assertTrue(matches(self.reference, self.snapshot))
        self.assertTrue(matches({**self.reference, 'snapshot_sha256': 'd' * 64}, self.snapshot))
        self.assertTrue(matches({**self.reference, 'verification_id': str(self.identity.verification_uuid)}, self.snapshot))
        for key, value in (('artifact_id', 'e' * 64), ('policy_sha256', 'e' * 64), ('generation', str(uuid4())),
                           ('verification_id', 'e' * 64), ('version', 'other')):
            self.assertFalse(matches({**self.reference, key: value}, self.snapshot), key)
        self.assertFalse(matches({k: v for k, v in self.reference.items() if k != 'snapshot_sha256'}, self.snapshot))


if __name__ == '__main__':
    unittest.main()
