"""0d: native Jenkins action ids come from typed fields, not a hash of canonical JSON."""
import hashlib
import unittest
from uuid import UUID, uuid4, uuid5

from work_external_delivery import (NATIVE_ACTION_FIELDS, canonical, legacy_native_action_id, native_action_id,
                                    native_action_ids)

VERSION = 'forge-native-jenkins-v3'


def identity(**changes):
    value = {'owner_id': 'mission-control', 'project_alias': 'homelab', 'task_id': str(uuid4()), 'attempt_id': str(uuid4()),
             'review_id': str(uuid4()), 'workflow_id': str(uuid4()), 'correlation_id': str(uuid4()),
             'candidate_commit': 'a' * 40, 'candidate_tree': 'b' * 40, 'native_gate_manifest_sha256': 'c' * 64}
    value.update(changes)
    return value


class NativeActionIdTest(unittest.TestCase):
    def test_typed_tuple_id_is_stable_and_independent_of_json_formatting(self):
        value = identity()
        expected = str(uuid5(UUID(value['workflow_id']), VERSION + ':' + '|'.join(value[k] for k in NATIVE_ACTION_FIELDS)))
        self.assertEqual(native_action_id(VERSION, value), expected)
        reordered = dict(reversed(list(value.items())))
        self.assertEqual(native_action_id(VERSION, reordered), expected)
        self.assertNotEqual(native_action_id('forge-native-jenkins-v2', value), expected)
        self.assertNotEqual(native_action_id(VERSION, identity(candidate_commit='d' * 40)), native_action_id(VERSION, value))

    def test_in_flight_legacy_ids_stay_valid(self):
        value = identity()
        legacy = str(uuid5(UUID(value['workflow_id']), VERSION + ':' + hashlib.sha256(canonical(value)).hexdigest()))
        self.assertEqual(legacy_native_action_id(VERSION, value), legacy)
        self.assertEqual(native_action_ids(VERSION, value), {legacy, native_action_id(VERSION, value)})
        self.assertNotIn(str(uuid4()), native_action_ids(VERSION, value))

    def test_ambiguous_or_missing_fields_are_rejected(self):
        with self.assertRaises(ValueError):
            native_action_id(VERSION, identity(project_alias='home|lab'))
        with self.assertRaises(ValueError):
            native_action_id(VERSION, identity(candidate_tree=''))
        broken = identity()
        del broken['candidate_tree']
        with self.assertRaises(KeyError):
            native_action_id(VERSION, broken)


if __name__ == '__main__':
    unittest.main()
