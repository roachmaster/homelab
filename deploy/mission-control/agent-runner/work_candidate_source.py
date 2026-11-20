"""Trusted import of an exact reviewed candidate for a linked repair workspace."""

import json
import os
from pathlib import Path
import re
import stat
from uuid import UUID


class CandidateSourceError(ValueError):
    pass


class ReviewCandidateSourceProvider:
    """Bootstrap supplies the fixed Git reader; source paths never come from agents."""
    def __init__(self, config, git):
        self.config, self.git = config, git

    def ensure(self, lease, repository):
        review = lease.get("repair_source_review_id")
        candidate = lease.get("repair_candidate_commit")
        if review is None and candidate is None:
            return
        try:
            if (not isinstance(review, str) or str(UUID(review)) != review
                    or not isinstance(candidate, str) or not re.fullmatch(r"[0-9a-f]{40}", candidate)
                    or candidate != lease["base_revision"] or not lease.get("feedback_id")
                    or not lease.get("parent_task_id")):
                raise CandidateSourceError()
            directory = self.config.state_root / "publication" / review
            source = directory / "repository"
            if (directory.is_symlink() or source.is_symlink() or (source / ".git").is_symlink()
                    or not (source / ".git").is_dir()
                    or not directory.resolve().is_relative_to(self.config.state_root.resolve())):
                raise CandidateSourceError()
            descriptor = os.open(directory / "review.json", os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
            with os.fdopen(descriptor, "rb") as stream:
                metadata = os.fstat(stream.fileno())
                if (not stat.S_ISREG(metadata.st_mode) or metadata.st_uid != os.geteuid()
                        or metadata.st_nlink != 1 or metadata.st_mode & 0o077):
                    raise CandidateSourceError()
                content = stream.read(65537)
            if len(content) > 65536:
                raise CandidateSourceError()
            manifest = json.loads(content)
            if (manifest.get("review_id") != review or manifest.get("candidate_commit") != candidate
                    or manifest.get("task_id") != lease["parent_task_id"]):
                raise CandidateSourceError()
            self._verify(source, candidate)
            # Only immutable objects are imported. No ref, working tree, main,
            # remote configuration or FETCH_HEAD is selected or updated.
            self.git(repository, "fetch", "--quiet", "--no-tags", "--no-recurse-submodules",
                     "--no-write-fetch-head", "--", str(source), candidate)
            self._verify(source, candidate)
            if self.git(repository, "rev-parse", "--verify", candidate + "^{commit}").decode().strip() != candidate:
                raise CandidateSourceError()
        except (OSError, ValueError, KeyError, TypeError, AttributeError):
            raise CandidateSourceError("The reviewed repair candidate is unavailable or changed.") from None

    def _verify(self, source, candidate):
        if (self.git(source, "rev-parse", "--verify", "HEAD^{commit}").decode().strip() != candidate
                or self.git(source, "status", "--porcelain", "--untracked-files=all")):
            raise CandidateSourceError()
