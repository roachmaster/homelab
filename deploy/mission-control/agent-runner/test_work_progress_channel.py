"""Owned public-channel/helper tests. No model, database or privileged effects."""

import json
import os
from pathlib import Path
import subprocess
from tempfile import TemporaryDirectory
import threading
import unittest

from work_progress import (MAX_REPORTS, ProgressError, ProgressReader, ProgressSinkError,
                           provision, validate_report, write_report)


class PublicProgressTest(unittest.TestCase):
    def test_strict_version_sequence_first_plan_and_utf8_summary_bounds(self):
        valid = {"version": 1, "sequence": 1, "kind": "plan", "summary": "## Plan\n\tRead and test 🛠"}
        self.assertEqual(validate_report(valid), valid)
        for changes in ({"version": True}, {"sequence": True}, {"sequence": 0}, {"sequence": 129},
                        {"sequence": 2}, {"kind": "progress"}, {"summary": " "},
                        {"summary": "x\0secret"}, {"summary": "x\x1b"}, {"summary": "x\x7f"},
                        {"summary": "🛠" * 1025}, {"summary": "\ud800"}, {"private_reasoning": "reject"}):
            with self.subTest(changes=repr(changes)), self.assertRaises(ProgressError):
                validate_report({**valid, **changes})

    def test_provisioned_self_contained_helper_publishes_plan_then_progress_and_no_replay(self):
        with TemporaryDirectory(prefix="forge-progress-helper-") as root:
            path = provision(root)
            reports = []
            reader = ProgressReader(path, lambda *record: reports.append(record))
            try:
                helper = Path(root) / "report_progress.py"
                for kind, summary in (("plan", "Read source\nRun tests"), ("progress", "Tests passed 🛠")):
                    result = subprocess.run(["/usr/bin/python3", "-B", str(helper), kind, summary], capture_output=True)
                    self.assertEqual(result.returncode, 0, result.stderr)
                reader.poll()
                reader.poll()
                self.assertEqual(reports, [(1, "plan", "Read source\nRun tests"), (2, "progress", "Tests passed 🛠")])
                self.assertEqual(len(reader.accepted), 2)
                self.assertEqual((path / "000001.json").stat().st_nlink, 1)
                self.assertFalse(any(item.name.endswith(".tmp") for item in path.iterdir()))
            finally:
                reader.close()

    def test_concurrent_cooperative_writers_and_reader_preserve_immutable_sequence(self):
        with TemporaryDirectory(prefix="forge-progress-concurrent-") as root:
            path = provision(root)
            write_report(path, "plan", "Public plan")
            reports = []
            reader = ProgressReader(path, lambda *record: reports.append(record))
            errors = []
            def writer():
                try:
                    for _ in range(15):
                        write_report(path, "progress", "Public milestone")
                except Exception as error:
                    errors.append(error)
            children = [threading.Thread(target=writer) for _ in range(3)]
            try:
                for child in children:
                    child.start()
                while any(child.is_alive() for child in children):
                    reader.poll()
                for child in children:
                    child.join()
                reader.poll()
                self.assertEqual(errors, [])
                self.assertEqual([item[0] for item in reports], list(range(1, 47)))
            finally:
                for child in children:
                    child.join()
                reader.close()

    def test_rejects_symlink_fifo_hardlink_gap_oversize_and_duplicate_json_without_reading_target(self):
        for invalid in ("symlink", "fifo", "hardlink", "gap", "oversize", "duplicate", "filename"):
            with self.subTest(invalid=invalid), TemporaryDirectory(prefix="forge-progress-invalid-") as root:
                path = provision(root)
                external = Path(root) / "outside.json"
                external.write_text('{"secret":"do not consume"}')
                report = path / "000001.json"
                if invalid == "symlink":
                    report.symlink_to(external)
                elif invalid == "fifo":
                    os.mkfifo(report)
                elif invalid == "hardlink":
                    os.link(external, report)
                elif invalid == "gap":
                    (path / "000002.json").write_text('{}')
                elif invalid == "oversize":
                    report.write_bytes(b"x" * 16_385)
                elif invalid == "duplicate":
                    report.write_text('{"version":1,"version":1,"sequence":1,"kind":"plan","summary":"x"}')
                else:
                    (path / "arbitrary.txt").write_text("reject")
                reader = ProgressReader(path, lambda *_: self.fail("invalid report reached callback"))
                try:
                    with self.assertRaises(ProgressError) as failure:
                        reader.poll()
                    self.assertNotIn("secret", str(failure.exception))
                finally:
                    reader.close()

    def test_accepted_report_or_directory_replacement_and_deletion_are_rejected(self):
        for mutation in ("rewrite", "delete", "directory"):
            with self.subTest(mutation=mutation), TemporaryDirectory(prefix="forge-progress-mutation-") as root:
                path = provision(root)
                write_report(path, "plan", "Original public plan")
                reader = ProgressReader(path, lambda *_: None)
                try:
                    reader.poll()
                    if mutation == "rewrite":
                        value = json.loads((path / "000001.json").read_text())
                        value["summary"] = "Changed claim"
                        (path / "000001.json").write_text(json.dumps(value))
                    elif mutation == "delete":
                        (path / "000001.json").unlink()
                    else:
                        path.rename(path.with_name("old-progress"))
                        path.mkdir()
                    with self.assertRaises(ProgressError):
                        reader.poll()
                finally:
                    reader.close()

    def test_sink_failure_is_distinct_and_does_not_acknowledge_report(self):
        with TemporaryDirectory(prefix="forge-progress-sink-") as root:
            path = provision(root)
            write_report(path, "plan", "Public plan")
            def unavailable(*_):
                raise OSError("private database error must not escape")
            reader = ProgressReader(path, unavailable)
            try:
                with self.assertRaises(ProgressSinkError) as failure:
                    reader.poll()
                self.assertNotIn("private", str(failure.exception))
                self.assertEqual(reader.accepted, {})
            finally:
                reader.close()

    def test_report_count_is_bounded_and_existing_records_are_not_overwritten(self):
        with TemporaryDirectory(prefix="forge-progress-limit-") as root:
            path = provision(root)
            write_report(path, "plan", "Public plan")
            original = (path / "000001.json").read_bytes()
            for _ in range(MAX_REPORTS - 1):
                write_report(path, "progress", "Public milestone")
            with self.assertRaises(ProgressError):
                write_report(path, "progress", "Too many")
            self.assertEqual((path / "000001.json").read_bytes(), original)
            self.assertFalse((path / "000129.json").exists())


if __name__ == "__main__":
    unittest.main()
