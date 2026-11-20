"""Explicit source selection from durable merge receipts and a private Git ref."""

import re

VERIFIED_REF = "refs/forge/verified-main"
SHA = re.compile(r"[0-9a-f]{40}")


class SourceRevisionError(ValueError):
    def __init__(self):
        super().__init__("The configured verified project source is unavailable.")


def private_revision(git, repository):
    raw = git(repository, "for-each-ref", "--count=2", "--format=%(refname) %(objectname)", VERIFIED_REF).decode().strip()
    if not raw:
        return None
    values = raw.split()
    if len(values) != 2 or values[0] != VERIFIED_REF or not SHA.fullmatch(values[1]):
        raise SourceRevisionError()
    return values[1]


class SelectedSourceRevisionProvider:
    """Coding-worker-scoped source reader; no registry, model or network access."""
    def __init__(self, config, store, git):
        self.config, self.store, self.git = config, store, git

    def projects(self):
        try:
            known = self.store.project_revisions() if self.config.autonomous_projects else {}
            if (not isinstance(known, dict) or len(known) > 64
                    or any(not isinstance(alias, str) or not isinstance(head, str) or not SHA.fullmatch(head)
                           for alias, head in known.items())):
                raise SourceRevisionError()
            result = []
            for alias, repository in self.config.projects.items():
                head = "HEAD"
                if alias in self.config.autonomous_projects:
                    reference, expected = private_revision(self.git, repository), known.get(alias)
                    if reference != expected:
                        raise SourceRevisionError()
                    head = expected or "HEAD"
                revision = self.git(repository, "rev-parse", "--verify", head + "^{commit}").decode().strip()
                if not SHA.fullmatch(revision) or head != "HEAD" and head != revision:
                    raise SourceRevisionError()
                result.append({"alias": alias, "base_revision": revision})
            return result
        except (OSError, ValueError, KeyError, TypeError, AttributeError):
            raise SourceRevisionError() from None


class VerifiedSourceSyncProvider:
    """Publisher-scoped exact object import and CAS of one non-main ref."""
    def __init__(self, config, git):
        self.config, self.git = config, git

    def synchronize(self, job, intent, require_lease):
        try:
            alias = job["project_alias"]
            if alias not in self.config.worker.autonomous_projects:
                raise SourceRevisionError()
            repository = self.config.worker.projects[alias]
            head, tree, previous = intent["merge_commit"], intent["candidate_tree"], intent["expected_main"]
            if any(not isinstance(value, str) or not SHA.fullmatch(value) for value in (head, tree, previous)):
                raise SourceRevisionError()
            require_lease()
            self.git._git(repository, "fetch", "--quiet", "--no-tags", "--no-recurse-submodules",
                "--no-write-fetch-head", "--", self.git.remote_url_factory(alias), head, network=True, timeout=120)
            descriptor = self.git._git(repository, "show", "-s", "--format=%T%n%P", head).decode().splitlines()
            if descriptor != [tree, previous + " " + intent["candidate_commit"]]:
                raise SourceRevisionError()
            current = private_revision(self.git._git, repository)
            if current == head:
                return
            if current is not None:
                self.git._git(repository, "merge-base", "--is-ancestor", current, previous)
            require_lease()
            self.git._git(repository, "update-ref", VERIFIED_REF, head, current or "0" * 40)
            if private_revision(self.git._git, repository) != head:
                raise SourceRevisionError()
        except Exception:
            raise SourceRevisionError() from None
