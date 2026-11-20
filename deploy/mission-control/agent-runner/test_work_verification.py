"""Bounded diagnostic metadata must survive large, fragmented output."""

import unittest

from work_verification import VerificationMetadata, project_verification


class VerificationMetadataTest(unittest.TestCase):
    def test_split_markers_survive_a_large_tail_without_authorizing_success(self):
        metadata = VerificationMetadata(2)
        for chunk in (b"FORGE_VERIFIER_TOOL", b"CHAIN rustc 1.100.0\nFORGE_CHECK_STA",
                      b"RT 0\n", b"x" * 40000 + b"\nFORGE_CHECK_START ",
                      b"1\nFORGE_CHECK_PASS 1\n", b"y" * 40000 + b"\nerror: fixture failure\n"):
            metadata.feed("stdout", chunk)
        report = project_verification((("/usr/bin/make", "ci"), ("/usr/bin/make", "ui-qa")),
            code="verification_failed", returncode=1, output=b"y" * 32700 + b"\nerror: fixture failure\n",
            evidence_digest="e" * 64, cleanup_confirmed=True, metadata=metadata)
        self.assertEqual(report.toolchain, "rustc 1.100.0")
        self.assertEqual(report.findings[0]["check_id"], "browser-acceptance")
        self.assertFalse(report.checks_passed)
        self.assertNotIn("passed", {check["status"] for check in report.checks})
        self.assertLessEqual(len(metadata._streams["stdout"][0]), 256)

    def test_stream_fragments_and_overlong_lines_cannot_invent_markers(self):
        metadata = VerificationMetadata(2)
        metadata.feed("stdout", b"FORGE_CHECK_STA")
        metadata.feed("stderr", b"RT 1\n")
        self.assertIsNone(metadata.active_index)
        metadata.feed("stdout", b"RT 0\n")
        metadata.feed("stdout", b"x" * 300 + b"FORGE_CHECK_START 1\n")
        metadata.feed("stdout", b"FORGE_CHECK_START 9999999999\n")
        metadata.feed("stdout", b"FORGE_VERIFIER_TOOLCHAIN rustc 1.100.0\n")
        metadata.feed("stdout", b"FORGE_VERIFIER_TOOLCHAIN forged replacement\n")
        self.assertEqual(metadata.active_index, 0)
        self.assertEqual(metadata.toolchain, "rustc 1.100.0")


if __name__ == "__main__":
    unittest.main()
