"""Explicit CI provider selection rejects ambiguity before opening resources."""

from pathlib import Path
from tempfile import TemporaryDirectory
import unittest

from test_work_executor import raw_config
from work_publication_config import DEFAULT_CHECKS, parse_config
from work_worker_config import parse_config as parse_worker


class PublicationProviderConfigTest(unittest.TestCase):
    def test_legacy_default_and_explicit_jenkins_selection(self):
        with TemporaryDirectory() as temporary:
            root = Path(temporary)
            worker = parse_worker(raw_config(root))
            raw = {"worker_config": str(root / "worker.json"),
                   "repositories": {"homelab": "fixture/project"},
                   "github_config_dir": str(worker.account.home / ".config/gh")}
            legacy = parse_config(raw, worker)
            self.assertEqual(legacy.hosted_provider, "github-actions")
            self.assertIsNone(legacy.jenkins_delivery_config)
            selected = parse_config({**raw, "hosted_provider": "jenkins",
                "jenkins_delivery_config": str(root / "delivery.json")}, worker)
            self.assertEqual(selected.hosted_provider, "jenkins")
            self.assertEqual(selected.jenkins_delivery_config, root / "delivery.json")
            self.assertEqual(selected.checks, DEFAULT_CHECKS)
            self.assertEqual(selected.required_hosted_checks, legacy.required_hosted_checks)
            self.assertFalse((root / "delivery.json").exists())
            self.assertEqual(legacy.native_delivery_mode, "integrate-first-v1")
            branch_first = parse_config({**raw, "hosted_provider": "jenkins",
                "jenkins_delivery_config": str(root / "delivery.json"),
                "native_delivery_mode": "branch-first-v1"}, worker)
            self.assertEqual(branch_first.native_delivery_mode, "branch-first-v1")

    def test_invalid_selection_or_reduced_checks_rejected(self):
        with TemporaryDirectory() as temporary:
            root = Path(temporary)
            worker = parse_worker(raw_config(root))
            raw = {"worker_config": str(root / "worker.json"),
                   "repositories": {"homelab": "fixture/project"},
                   "github_config_dir": str(worker.account.home / ".config/gh")}
            for fields in ({"hosted_provider": "unknown"}, {"hosted_provider": []},
                    {"native_delivery_mode": "branch-first-v1"},
                    {"native_delivery_mode": []}, {"native_delivery_mode": "unknown"},
                    {"hosted_provider": "jenkins"},
                    {"hosted_provider": "jenkins", "jenkins_delivery_config": "relative.json"},
                    {"jenkins_delivery_config": str(root / "delivery.json")},
                    {"hosted_provider": "jenkins", "jenkins_delivery_config": str(root / "delivery.json"),
                     "checks": [["/usr/bin/true"]]},
                    {"hosted_provider": "jenkins", "jenkins_delivery_config": str(root / "delivery.json"),
                     "required_hosted_checks": [[".github/workflows/rust.yml", "Rust CI", "other"]]}):
                with self.subTest(fields=fields), self.assertRaises(ValueError):
                    parse_config({**raw, **fields}, worker)


if __name__ == "__main__":
    unittest.main()
