"""Verified source attachments accepted over the private completion relay."""

from dataclasses import dataclass
import re
import unicodedata


@dataclass(frozen=True)
class FileAttachment:
    reference: dict
    excerpt: str

    def expand(self, prompt):
        ref = self.reference
        return (
            f"{prompt}\n\nVerified file excerpt (reference data)\n"
            f"Project: {ref['project']!r}\nFile: {ref['path']!r}\nSHA-256: {ref['sha256']}\n"
            f"Lines: {ref['first_line']}–{ref['last_line']}; starting column: {ref['first_column']}\n\n"
            f"{self.excerpt}"
        )


def parse_attachment(value):
    if not isinstance(value, dict) or value.keys() != {"reference", "excerpt"}:
        raise ValueError("Malformed file attachment")
    ref, excerpt = value["reference"], value["excerpt"]
    fields = {"node", "project", "path", "sha256", "first_line", "last_line", "first_column"}
    if not isinstance(ref, dict) or ref.keys() != fields:
        raise ValueError("Malformed file reference")
    if any(not isinstance(ref[key], str) or not 1 <= len(ref[key].encode()) <= 4096
           or any(unicodedata.category(char) == "Cc" for char in ref[key])
           for key in ("node", "project", "path")):
        raise ValueError("Malformed file name")
    if (not isinstance(ref["sha256"], str) or not re.fullmatch(r"[0-9a-f]{64}", ref["sha256"])
            or any(type(ref[key]) is not int for key in ("first_line", "last_line", "first_column"))
            or not 1 <= ref["first_line"] <= ref["last_line"] <= 1048577
            or not 0 <= ref["first_column"] <= 1048576
            or not isinstance(excerpt, str) or len(excerpt.encode()) > 4096 or "\0" in excerpt):
        raise ValueError("Malformed file digest, range or excerpt")
    return FileAttachment(ref, excerpt)
