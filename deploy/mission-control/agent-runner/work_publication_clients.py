"""Narrow parent-only snapshot, Git and GitHub publication providers.

No credentials, arbitrary commands or remote URLs originate in an agent result.
GitHub authentication remains in the trusted publisher's existing gh account.
"""

from dataclasses import dataclass
from datetime import datetime, timezone
import fcntl
import json
import hashlib
import os
from pathlib import Path, PurePosixPath
import re
import selectors
import shutil
import stat
import subprocess
import time
from urllib.parse import urlencode
from uuid import UUID

from work_agent_branch import (
    EPHEMERAL_BRANCH_POLICY,
    PERSISTENT_BRANCH_POLICY,
    canonical_profile_slug,
    validate_git_branch_name,
)
from work_executor import MAX_FILES, MAX_WORKSPACE_BYTES
from work_process import terminate_and_reap

SHA = re.compile(r"[0-9a-f]{40}")
DIGEST = re.compile(r"[0-9a-f]{64}")
PRIVATE_KEY = re.compile(rb"-----BEGIN (?:RSA |EC |DSA |OPENSSH |ENCRYPTED )?PRIVATE KEY-----(?:\\n|\r?\n)[A-Za-z0-9+/=]{32,}(?:\\n|\r?\n)")
ACCESS_TOKEN = re.compile(rb"(?:gh[pousr]_[A-Za-z0-9]{36}|github_pat_[A-Za-z0-9_]{70,}|AIza[A-Za-z0-9_-]{35}|ya29\.[A-Za-z0-9_-]{40,})")
PRIVATE_PARTS = frozenset({".ssh", ".gemini", ".codex", ".pgpass", ".netrc"})
CACHE_PARTS = frozenset({"node_modules", "target", "__pycache__", ".venv", ".cache", ".pytest_cache", ".mypy_cache", ".ruff_cache"})


def artifact_guard(path, data):
    parts = PurePosixPath(path).parts
    lowered = tuple(part.lower() for part in parts)
    if (any(part in PRIVATE_PARTS | CACHE_PARTS for part in lowered)
            or any(lowered[index:index + 2] == (".config", "gh") for index in range(len(lowered)))
            or any((part == ".env" or part.startswith(".env."))
                   and not part.endswith((".example", ".sample", ".template")) for part in lowered)
            or PurePosixPath(path).suffix.lower() in {".gguf", ".safetensors", ".ckpt", ".onnx"}):
        raise PublicationError("private_or_generated_artifact")
    if PRIVATE_KEY.search(data):
        raise PublicationError("private_key_artifact")
    for match in ACCESS_TOKEN.finditer(data):
        value = match.group()
        # Repeated placeholder glyphs in examples are not an actual token.
        prefix = next(prefix for prefix in (b"github_pat_", b"ghp_", b"gho_", b"ghu_", b"ghs_", b"ghr_", b"AIza", b"ya29.")
                      if value.startswith(prefix))
        body = value[len(prefix):]
        if len(set(body.lower())) >= 4:
            raise PublicationError("access_token_artifact")


class PublicationError(RuntimeError):
    def __init__(self, code="publication_failed"):
        self.code = code
        super().__init__(code)



class SourceIntegrationConflict(PublicationError):
    """Actual private three-way conflict, committed only as unverified evidence."""
    def __init__(self,repository,candidate,authored,main,paths):
        super().__init__('main_merge_conflict')
        self.repository,self.candidate_commit=repository,candidate
        self.authored_commit,self.integration_base=authored,main
        self.conflict_paths=tuple(paths)


def uuid(value):
    try:
        if not isinstance(value, str) or str(UUID(value)) != value:
            raise ValueError()
    except (ValueError, TypeError, AttributeError):
        raise PublicationError("invalid_publication_identity") from None
    return value


def branch_name(job, config=None):
    explicit = job.get("branch_name") if isinstance(job, dict) else None
    if isinstance(explicit, str) and explicit:
        try:
            validated = validate_git_branch_name(explicit)
            if validated == "main":
                raise ValueError()
            return validated
        except ValueError:
            raise PublicationError("invalid_publication_identity") from None
    if (config is not None
            and getattr(config, "branch_policy", EPHEMERAL_BRANCH_POLICY) == PERSISTENT_BRANCH_POLICY
            and isinstance(job, dict) and job.get("agent_profile")
            and not str(job.get("branch_publication_id") or "").startswith("legacy:")):
        try:
            return config.resolved_branch_policy().resolve_branch(
                job["project_alias"], job["agent_profile"]
            )
        except (KeyError, ValueError):
            raise PublicationError("invalid_publication_identity") from None
    return "forge/task-" + uuid(job.get("branch_publication_id") or job.get("target_publication_id") or job["publication_id"])


def command(argv, *, env, cwd=None, data=None, timeout=60):
    """Bounded, noninteractive parent CLI; raw diagnostics never leave this port."""
    process = None
    try:
        process = subprocess.Popen(list(argv), stdin=subprocess.PIPE if data is not None else subprocess.DEVNULL,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env, cwd=cwd,
                                   close_fds=True, start_new_session=True)
        deadline, total, stdout = time.monotonic() + timeout, 0, bytearray()
        with selectors.DefaultSelector() as selected:
            for stream, kind in ((process.stdout, "stdout"), (process.stderr, "stderr")):
                os.set_blocking(stream.fileno(), False)
                selected.register(stream, selectors.EVENT_READ, kind)
            pending = memoryview(data or b"")
            if data is not None:
                os.set_blocking(process.stdin.fileno(), False)
                selected.register(process.stdin, selectors.EVENT_WRITE, "stdin")
            while selected.get_map():
                if time.monotonic() >= deadline:
                    raise PublicationError("publication_command_timeout")
                for key, _mask in selected.select(.1):
                    if key.data == "stdin":
                        try:
                            pending = pending[os.write(key.fd, pending):]
                        except BlockingIOError:
                            continue
                        if not pending:
                            selected.unregister(key.fileobj)
                            key.fileobj.close()
                    else:
                        try:
                            chunk = os.read(key.fd, 65536)
                        except BlockingIOError:
                            continue
                        if not chunk:
                            selected.unregister(key.fileobj)
                        else:
                            total += len(chunk)
                            if total > 4 * 1024 * 1024:
                                raise PublicationError("publication_command_output_limit")
                            if key.data == "stdout":
                                stdout.extend(chunk)
        if process.wait(timeout=max(.01, deadline - time.monotonic())):
            raise PublicationError("publication_command_failed")
        return bytes(stdout)
    except (OSError, subprocess.SubprocessError):
        raise PublicationError("publication_command_unavailable") from None
    finally:
        if process is not None:
            stopped = terminate_and_reap(process)
            for stream in (process.stdin, process.stdout, process.stderr):
                if stream is not None:
                    try:
                        stream.close()
                    except OSError:
                        pass
            if not stopped:
                raise PublicationError("publication_launcher_uncertain")


def parent_environment(config):
    return {"PATH": "/usr/bin:/bin", "LANG": "C.UTF-8", "LC_ALL": "C.UTF-8",
            "HOME": str(config.worker.account.home), "GH_CONFIG_DIR": str(config.github_config_dir),
            "GH_HOST": "github.com", "GH_PROMPT_DISABLED": "1", "GIT_TERMINAL_PROMPT": "0",
            "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": "/dev/null",
            "GIT_AUTHOR_NAME": config.commit_name, "GIT_COMMITTER_NAME": config.commit_name,
            "GIT_AUTHOR_EMAIL": config.commit_email, "GIT_COMMITTER_EMAIL": config.commit_email}


@dataclass(frozen=True)
class SnapshotFile:
    path: str
    kind: str
    data: bytes
    executable: bool


@dataclass(frozen=True)
class Snapshot:
    files: tuple
    workspace_digest: str


def safe_relative(value):
    if not isinstance(value, str):
        raise PublicationError("unsafe_artifact_path")
    path = PurePosixPath(value)
    if (not value or path.is_absolute() or path.as_posix() != value
            or any(part in {"", ".", ".."} or part.lower() == ".git" for part in path.parts)
            or len(value.encode("utf-8")) > 1024 or any(ord(c) < 32 for c in value)):
        raise PublicationError("unsafe_artifact_path")
    return path


def safe_link(path, target):
    if not target or target.startswith("/") or "\x00" in target:
        raise PublicationError("unsafe_artifact_symlink")
    depth = len(PurePosixPath(path).parent.parts)
    for part in PurePosixPath(target).parts:
        if part == "..":
            depth -= 1
        elif part not in {"", "."}:
            if part.lower() == ".git":
                raise PublicationError("unsafe_artifact_symlink")
            depth += 1
        if depth < 0:
            raise PublicationError("unsafe_artifact_symlink")


class WorkspaceSnapshotProvider:
    def __init__(self, workspaces, *, execute=command):
        self.workspaces, self.execute = workspaces, execute

    def create(self, job):
        with self.workspaces.inspect(job) as workspace:
            if workspace.digest() != job["workspace_digest"]:
                raise PublicationError("workspace_changed")
            env = {"PATH": "/usr/bin:/bin", "LANG": "C.UTF-8", "GIT_CONFIG_NOSYSTEM": "1",
                   "GIT_CONFIG_GLOBAL": "/dev/null", "GIT_TERMINAL_PROMPT": "0"}
            raw = self.execute(["/usr/bin/git", "-c", "core.hooksPath=/dev/null", "-c", "core.fsmonitor=false",
                                "-C", str(workspace.path), "ls-files", "--cached", "--others", "--exclude-standard", "-z"], env=env)
            names = sorted(set(raw.split(b"\0")) - {b""})
            if len(names) > MAX_FILES:
                raise PublicationError("artifact_resource_limit")
            files, total = [], 0
            for encoded in names:
                try:
                    name = encoded.decode("utf-8")
                    relative = safe_relative(name)
                except UnicodeError:
                    raise PublicationError("unsafe_artifact_path") from None
                source = workspace.path.joinpath(*relative.parts)
                for parent in relative.parents:
                    if parent != PurePosixPath(".") and workspace.path.joinpath(*parent.parts).is_symlink():
                        raise PublicationError("unsafe_artifact_path")
                try:
                    metadata = source.lstat()
                except FileNotFoundError:
                    continue  # A tracked deletion is represented by absence.
                if stat.S_ISLNK(metadata.st_mode):
                    link = os.readlink(source)
                    safe_link(name, link)
                    data, kind = os.fsencode(link), "link"
                elif stat.S_ISREG(metadata.st_mode):
                    descriptor = os.open(source, os.O_RDONLY | os.O_NOFOLLOW)
                    with os.fdopen(descriptor, "rb") as stream:
                        data = stream.read(MAX_WORKSPACE_BYTES - total + 1)
                    kind = "file"
                else:
                    raise PublicationError("unsafe_artifact_type")
                total += len(data)
                if total > MAX_WORKSPACE_BYTES:
                    raise PublicationError("artifact_resource_limit")
                artifact_guard(name, data)
                files.append(SnapshotFile(name, kind, data, bool(metadata.st_mode & 0o111)))
            if workspace.digest() != job["workspace_digest"]:
                raise PublicationError("workspace_changed")
            return Snapshot(tuple(files), job["workspace_digest"])


class GitRepositoryProvider:
    def __init__(self, config, *, execute=command, remote_url_factory=None, branch_provider=None):
        self.config, self.execute = config, execute
        self.remote_url_factory = remote_url_factory or (lambda alias: "https://github.com/" + config.repositories[alias] + ".git")
        self.branch_provider = branch_provider

    def _git(self, repository, *args, network=False, timeout=60):
        argv = ["/usr/bin/git", "-c", "core.hooksPath=/dev/null", "-c", "core.fsmonitor=false",
                "-c", "commit.gpgSign=false", "-c", "credential.helper="]
        if network:
            argv += ["-c", "credential.helper=/usr/bin/gh auth git-credential"]
        if repository is not None:
            argv += ["-C", str(repository)]
        return self.execute([*argv, *args], env=parent_environment(self.config), timeout=timeout)

    def _agent_cache_dir(self, project_alias, agent_profile):
        root = self.config.state_root / "agent-branches" / project_alias / canonical_profile_slug(agent_profile)
        root.mkdir(mode=0o700, parents=True, exist_ok=True)
        if root.is_symlink() or root.parent.is_symlink():
            raise PublicationError("publication_state_changed")
        return root

    def _recover_dirty_git_state(self, clone_dir):
        """Abort any crashed in-progress rebase or merge inside an owned cached clone."""
        git_dir = clone_dir / ".git"
        if not git_dir.is_dir():
            return
        if (git_dir / "rebase-merge").exists() or (git_dir / "rebase-apply").exists():
            try:
                self._git(clone_dir, "rebase", "--abort")
            except PublicationError:
                shutil.rmtree(git_dir / "rebase-merge", ignore_errors=True)
                shutil.rmtree(git_dir / "rebase-apply", ignore_errors=True)
        if (git_dir / "MERGE_HEAD").exists():
            try:
                self._git(clone_dir, "merge", "--abort")
            except PublicationError:
                (git_dir / "MERGE_HEAD").unlink(missing_ok=True)
        try:
            self._git(clone_dir, "reset", "--hard", "HEAD")
            self._git(clone_dir, "clean", "-fd")
        except PublicationError:
            pass

    def sync_agent_cache_clone(self, project_alias, agent_profile, branch, remote, *, candidate_commit=None, candidate_repo=None):
        """Maintain a file-locked per-agent cached clone tracking remote main and branch ref."""
        cache_root = self._agent_cache_dir(project_alias, agent_profile)
        lock_path = cache_root / "clone.lock"
        clone_dir = cache_root / "repo"
        descriptor = os.open(lock_path, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
        with os.fdopen(descriptor, "r+b") as stream:
            fcntl.flock(stream.fileno(), fcntl.LOCK_EX)
            try:
                if clone_dir.is_symlink():
                    raise PublicationError("publication_state_changed")
                if not (clone_dir / ".git").is_dir():
                    if clone_dir.exists():
                        shutil.rmtree(clone_dir)
                    self._git(None, "clone", "--quiet", "--no-local", "--no-checkout", "--",
                              str(self.config.worker.projects[project_alias]), str(clone_dir))
                else:
                    self._recover_dirty_git_state(clone_dir)
                self._git(clone_dir, "fetch", "--quiet", "--no-tags", "--", remote,
                          "+refs/heads/main:refs/remotes/origin/main", network=True, timeout=120)
                fetched_main = self._git(clone_dir, "rev-parse", "refs/remotes/origin/main").decode().strip()
                if candidate_repo is not None and candidate_commit is not None:
                    self._git(clone_dir, "fetch", "--quiet", "--no-tags", "--no-recurse-submodules",
                              "--", str(candidate_repo), candidate_commit)
                    self._git(clone_dir, "update-ref", "refs/heads/" + branch, candidate_commit)
                return fetched_main
            finally:
                fcntl.flock(stream.fileno(), fcntl.LOCK_UN)

    def _persistent_mode(self, job):
        return (getattr(self.config, "branch_policy", EPHEMERAL_BRANCH_POLICY) == PERSISTENT_BRANCH_POLICY
                and isinstance(job, dict) and bool(job.get("agent_profile"))
                and not str(job.get("branch_publication_id") or "").startswith("legacy:"))

    def prepare(self, job, snapshot):
        return self._prepare(job, snapshot, branch_first=False)

    def prepare_source(self, job, snapshot):
        """Integrate current main privately before the source push/event/build."""
        return self._prepare(job, snapshot, branch_first=True)

    def _prepare(self, job, snapshot, *, branch_first):
        if snapshot.workspace_digest != job["workspace_digest"]:
            raise PublicationError("workspace_changed")
        directory = self.config.state_root / uuid(job["publication_id"])
        directory.mkdir(mode=0o700, parents=True, exist_ok=True)
        identity = {key: job[key] for key in ("publication_id", "task_id", "attempt_id", "base_revision", "workspace_digest")}
        metadata = directory / "identity.json"
        if directory.is_symlink() or metadata.is_symlink():
            raise PublicationError("publication_state_changed")
        if metadata.exists():
            if json.loads(metadata.read_text()) != identity:
                raise PublicationError("publication_state_changed")
        else:
            with metadata.open("x") as stream:
                json.dump(identity, stream, sort_keys=True)
        repository = directory / "repository"
        if repository.is_symlink():
            raise PublicationError("publication_state_changed")
        prepared = directory / 'prepared-source.json'
        if branch_first and prepared.exists():
            from work_external_delivery import read_private
            receipt = json.loads(read_private(prepared, owner=os.geteuid()))
            if receipt.get('identity') != identity:
                raise PublicationError('publication_state_changed')
            head, main = receipt.get('candidate_commit'), receipt.get('expected_main')
            if not isinstance(head, str) or not SHA.fullmatch(head) or not isinstance(main, str) or not SHA.fullmatch(main):
                raise PublicationError('publication_state_changed')
            tested = directory / 'repository.tested'
            selected = tested if (directory / 'local-seed-install.json').exists() and tested.exists() else repository
            if selected.is_symlink() or not selected.is_dir():
                raise PublicationError('publication_state_changed')
            self.verify(selected, head)
            expected = {entry.path: ['120000' if entry.kind == 'link' else '100755' if entry.executable else '100644',
                hashlib.sha1(b'blob '+str(len(entry.data)).encode()+b'\0'+entry.data).hexdigest()] for entry in snapshot.files}
            version = receipt.get('version')
            authored = head if version == 1 else receipt.get('authored_commit')
            if type(version) is not int or version not in (1,2) or not isinstance(authored,str)or not SHA.fullmatch(authored):
                raise PublicationError('publication_state_changed')
            actual = self._tree_entries(selected,authored)
            if actual != expected or self._git(selected,'rev-parse','FETCH_HEAD').decode().strip() != main:
                raise PublicationError('publication_state_changed')
            if version == 2:
                integration = receipt.get('integration')
                if (integration not in {'current-main-three-way-v1', 'trunk-rebase-v1'}
                    or self._git(selected,'rev-parse',authored+'^{tree}').decode().strip()!=receipt.get('authored_tree')
                    or self._git(selected,'rev-parse',head+'^{tree}').decode().strip()!=receipt.get('candidate_tree')):
                    raise PublicationError('publication_state_changed')
                if integration == 'current-main-three-way-v1':
                    self._git(selected,'merge-base','--is-ancestor',authored,head)
                self._git(selected,'merge-base','--is-ancestor',main,head)
                paths=receipt.get('conflict_paths')
                if not isinstance(paths,list)or len(paths)>32 or any(not isinstance(path,str)for path in paths):
                    raise PublicationError('publication_state_changed')
                for path in paths:safe_relative(path)
                if paths:raise SourceIntegrationConflict(selected,head,authored,main,paths)
            # Existing v1/pushed/build receipts retain their exact old source.
            # They are never silently upgraded into another candidate.
            return selected,head
        if repository.exists():
            shutil.rmtree(repository)  # Only this stopped, identity-bound private clone.
        self._git(None, "clone", "--quiet", "--no-local", "--no-checkout", "--",
                  str(self.config.worker.projects[job["project_alias"]]), str(repository))
        canonical = self.config.worker.projects[job["project_alias"]]
        base = job["base_revision"]
        if self._git(canonical, "rev-parse", "--verify", base + "^{commit}").decode().strip() != base:
            raise PublicationError("publication_base_changed")
        self._git(repository, "fetch", "--quiet", "--no-tags", "--no-recurse-submodules",
                  "--no-write-fetch-head", "--", str(canonical), base)
        if self._git(repository, "rev-parse", "--verify", base + "^{commit}").decode().strip() != base:
            raise PublicationError("publication_base_changed")
        branch = branch_name(job, self.config)
        self._git(repository, "checkout", "--quiet", "--detach", job["base_revision"])
        self._git(repository, "checkout", "--quiet", "-b", branch)
        for child in repository.iterdir():
            if child.name != ".git":
                if child.is_dir() and not child.is_symlink():
                    shutil.rmtree(child)
                else:
                    child.unlink()
        for entry in snapshot.files:
            relative = safe_relative(entry.path)
            artifact_guard(entry.path, entry.data)
            destination = repository.joinpath(*relative.parts)
            destination.parent.mkdir(parents=True, exist_ok=True)
            if any(parent.is_symlink() for parent in destination.parents if parent != repository.parent):
                raise PublicationError("unsafe_artifact_path")
            if entry.kind == "link":
                target = os.fsdecode(entry.data)
                safe_link(entry.path, target)
                destination.symlink_to(target)
            elif entry.kind == "file":
                destination.write_bytes(entry.data)
                destination.chmod(0o755 if entry.executable else 0o644)
            else:
                raise PublicationError("unsafe_artifact_type")
        self._git(repository, "add", "--all", "--", ".")
        changed = bool(self._git(repository, "diff", "--cached", "--name-only", "-z"))
        if not changed and (not branch_first or not (job.get('parent_task_id') and job.get('feedback_id'))):
            raise PublicationError("no_source_changes")
        if changed:
            self._git(repository, "commit", "--quiet", "--no-gpg-sign", "-m", "Forge implementation " + uuid(job["task_id"]))
        authored=self._git(repository,'rev-parse','HEAD').decode().strip()
        authored_tree=self._git(repository,'rev-parse','HEAD^{tree}').decode().strip()
        remote = self.remote_url_factory(job["project_alias"])
        persistent = self._persistent_mode(job)
        fetched_at = datetime.now(timezone.utc)
        if persistent:
            self.sync_agent_cache_clone(job["project_alias"], job["agent_profile"], branch, remote)
            if self.branch_provider is not None:
                self.branch_provider.record_sync(
                    job["project_alias"], job["agent_profile"],
                    branch_name=branch, sync_status="rebasing",
                    last_task_id=job.get("task_id"),
                    last_correlation_id=job.get("correlation_id"),
                )
        self._git(repository, "fetch", "--quiet", "--no-tags", "--", remote, "refs/heads/main", network=True, timeout=120)
        main=self._git(repository,'rev-parse','FETCH_HEAD').decode().strip()
        conflicts=[]
        integration_mode = 'current-main-three-way-v1'
        if persistent and changed:
            try:
                self._git(repository, "rebase", "--quiet", "FETCH_HEAD")
                integration_mode = 'trunk-rebase-v1'
            except PublicationError:
                self._recover_dirty_git_state(repository)
                self._git(repository, "checkout", "--quiet", "-B", branch, authored)
                if not branch_first:
                    if self.branch_provider is not None:
                        self.branch_provider.record_sync(
                            job["project_alias"], job["agent_profile"],
                            branch_name=branch, fetched_main_sha=main, fetched_main_at=fetched_at,
                            sync_status="conflict", conflict_paths=[],
                            last_task_id=job.get("task_id"),
                            last_correlation_id=job.get("correlation_id"),
                        )
                    raise PublicationError('main_merge_conflict') from None
                try:
                    self._git(repository, "merge", "--no-edit", "--no-gpg-sign", "FETCH_HEAD")
                except PublicationError:
                    conflicts=[path.decode('utf-8')for path in self._git(repository,'diff','--name-only','--diff-filter=U','-z').split(b'\0')if path]
                    if not conflicts:raise PublicationError('main_merge_unavailable')from None
                    if len(conflicts)>32:raise PublicationError('main_merge_conflicts_out_of_scope')from None
                    for path in conflicts:safe_relative(path)
                    self._git(repository,'add','--all','--','.')
                    self._git(repository,'commit','--quiet','--no-gpg-sign','-m','Unverified Forge current-main integration conflict; resolve before review')
        else:
            try:
                # Three-way integration is owned exclusively by this private clone.
                # The native worktree and its admitted base/receipt stay unchanged.
                self._git(repository, "merge", "--no-edit", "--no-gpg-sign", "FETCH_HEAD")
            except PublicationError:
                if not branch_first:raise PublicationError('main_merge_conflict')from None
                conflicts=[path.decode('utf-8')for path in self._git(repository,'diff','--name-only','--diff-filter=U','-z').split(b'\0')if path]
                if not conflicts:raise PublicationError('main_merge_unavailable')from None
                if len(conflicts)>32:raise PublicationError('main_merge_conflicts_out_of_scope')from None
                for path in conflicts:safe_relative(path)
                # Preserve actual unresolved markers and nonconflicting main files,
                # with both genuine parents. This is never approval or CI evidence.
                self._git(repository,'add','--all','--','.')
                self._git(repository,'commit','--quiet','--no-gpg-sign','-m','Unverified Forge current-main integration conflict; resolve before review')
        head = self._git(repository, "rev-parse", "HEAD").decode().strip()
        if not SHA.fullmatch(head):raise PublicationError("invalid_publication_commit")
        self.verify(repository, head)
        self._git(repository,'merge-base','--is-ancestor',main,head)
        if persistent:
            if not conflicts:
                self.sync_agent_cache_clone(
                    job["project_alias"], job["agent_profile"], branch, remote,
                    candidate_commit=head, candidate_repo=repository,
                )
            if self.branch_provider is not None:
                self.branch_provider.record_sync(
                    job["project_alias"], job["agent_profile"],
                    branch_name=branch,
                    fetched_main_sha=main,
                    fetched_main_at=fetched_at,
                    sync_status="conflict" if conflicts else "synced_with_main",
                    conflict_paths=conflicts,
                    last_task_id=job.get("task_id"),
                    last_correlation_id=job.get("correlation_id"),
                )
        if branch_first:
            from work_external_delivery import durable_json
            durable_json(prepared, {'version':2,'integration':integration_mode,'identity':identity,
                'authored_commit':authored,'authored_tree':authored_tree,'candidate_commit':head,
                'candidate_tree':self._git(repository,'rev-parse','HEAD^{tree}').decode().strip(),
                'expected_main':main,'conflict_paths':conflicts})
        if conflicts:raise SourceIntegrationConflict(repository,head,authored,main,conflicts)
        return repository, head

    def _tree_entries(self,repository,head):
        result={}
        for row in self._git(repository,'ls-tree','-r','-z','--full-tree',head).split(b'\0'):
            if row:
                description,path=row.split(b'\t',1);mode,kind,blob=description.decode().split()
                if kind!='blob':raise PublicationError('publication_state_changed')
                result[path.decode()]=[mode,blob]
        return result

    def verify(self, repository, expected):
        if (self._git(repository, "rev-parse", "HEAD").decode().strip() != expected
                or self._git(repository, "status", "--porcelain", "--untracked-files=all")):
            raise PublicationError("verified_source_changed")

    def remote_head(self, job):
        branch = branch_name(job, self.config)
        raw = self._git(None, "ls-remote", "--heads", "--", self.remote_url_factory(job["project_alias"]),
                        "refs/heads/" + branch, network=True).decode().strip()
        if not raw:
            return None
        values = raw.split()
        if len(values) != 2 or not SHA.fullmatch(values[0]) or values[1] != "refs/heads/" + branch:
            raise PublicationError("remote_branch_identity_changed")
        return values[0]

    def push(self, job, repository, head, *, expected_remote_head=None, reservation=None, pull_requests=None):
        self.verify(repository, head)
        branch = branch_name(job, self.config)
        if reservation is not None:
            if self.branch_provider is not None:
                self.branch_provider.verify_owner(reservation)
            if pull_requests is not None:
                open_prs = pull_requests.list_open({**job, "branch_name": branch})
                if reservation.active_pr_url is None:
                    if open_prs:
                        raise PublicationError("branch_open_pr_conflict")
                else:
                    if len(open_prs) != 1 or open_prs[0].get("html_url") != reservation.active_pr_url:
                        raise PublicationError("branch_open_pr_conflict")
            lease_expected = expected_remote_head if expected_remote_head is not None else ""
            lease_arg = f"--force-with-lease=refs/heads/{branch}:{lease_expected}"
            self._git(repository, "push", "--quiet", lease_arg, "--",
                      self.remote_url_factory(job["project_alias"]),
                      head + ":refs/heads/" + branch, network=True, timeout=120)
            return
        self._git(repository, "push", "--quiet", "--", self.remote_url_factory(job["project_alias"]),
                  head + ":refs/heads/" + branch, network=True, timeout=120)


class GitHubPullRequestClient:
    def __init__(self, config, *, execute=command):
        self.config, self.execute = config, execute

    def _api(self, method, path, payload=None):
        argv = ["/usr/bin/gh", "api", "--hostname", "github.com", "--method", method,
                "--header", "Accept: application/vnd.github+json", path]
        data = None
        if payload is not None:
            argv += ["--input", "-"]
            data = json.dumps(payload).encode()
        raw = self.execute(argv, env=parent_environment(self.config), data=data)
        try:
            return json.loads(raw.decode())
        except (UnicodeError, ValueError, RecursionError):
            raise PublicationError("invalid_pull_request_response") from None

    def _validated(self, job, head, value):
        repository = self.config.repositories[job["project_alias"]]
        branch = branch_name(job, self.config)
        try:
            number = value["number"]
            valid = (type(number) is int and number > 0
                     and value["head"]["ref"] == branch and value["head"]["sha"] == head
                     and value["head"]["repo"]["full_name"] == repository
                     and value["base"]["ref"] == "main"
                     and value["base"]["repo"]["full_name"] == repository
                     and value["html_url"] == f"https://github.com/{repository}/pull/{number}")
        except (KeyError, TypeError):
            valid = False
        if not valid:
            raise PublicationError("pull_request_identity_changed")
        return value["html_url"]

    def list_open(self, job):
        repository = self.config.repositories[job["project_alias"]]
        branch = branch_name(job, self.config)
        query = urlencode({"state": "open", "head": repository.split("/")[0] + ":" + branch,
                           "base": "main", "per_page": 100})
        values = self._api("GET", "repos/" + repository + "/pulls?" + query)
        if not isinstance(values, list):
            raise PublicationError("invalid_pull_request_response")
        return values

    def find(self, job, head, *, expected_pr_url=None):
        repository = self.config.repositories[job["project_alias"]]
        branch = branch_name(job, self.config)
        persistent = (
            getattr(self.config, "branch_policy", EPHEMERAL_BRANCH_POLICY) == PERSISTENT_BRANCH_POLICY
            and (bool(job.get("agent_profile")) or bool(job.get("branch_name")) or expected_pr_url is not None)
            and not branch.startswith("forge/task-")
        )
        if persistent:
            open_prs = self.list_open({**job, "branch_name": branch})
            if len(open_prs) > 1:
                raise PublicationError("pull_request_identity_changed")
            if len(open_prs) == 1:
                if expected_pr_url is not None and open_prs[0].get("html_url") != expected_pr_url:
                    raise PublicationError("pull_request_identity_changed")
                return self._validated(job, head, open_prs[0])
            if expected_pr_url is not None:
                # Lost-ACK reconciliation for a recorded PR that may have already been merged/closed.
                query = urlencode({"state": "all", "head": repository.split("/")[0] + ":" + branch,
                                   "base": "main", "per_page": 100})
                values = self._api("GET", "repos/" + repository + "/pulls?" + query)
                if not isinstance(values, list):
                    raise PublicationError("invalid_pull_request_response")
                matches = [row for row in values
                           if isinstance(row, dict)
                           and row.get("html_url") == expected_pr_url
                           and row.get("head", {}).get("sha") == head]
                if len(matches) == 1:
                    return self._validated(job, head, matches[0])
            return None
        query = urlencode({"state": "all", "head": repository.split("/")[0] + ":" + branch,
                           "base": "main", "per_page": 100})
        values = self._api("GET", "repos/" + repository + "/pulls?" + query)
        if not isinstance(values, list) or len(values) > 1:
            raise PublicationError("pull_request_identity_changed")
        return self._validated(job, head, values[0]) if values else None

    def create(self, job, head):
        repository = self.config.repositories[job["project_alias"]]
        payload = {"title": "Forge implementation " + uuid(job["task_id"]),
                   "head": branch_name(job, self.config), "base": "main", "draft": True, "maintainer_can_modify": False,
                   "body": "Implements the approved Forge plan.\n\nTask: " + uuid(job["task_id"])
                   + "\nWorkspace digest: `" + job["workspace_digest"] + "`\n\n"
                   + "The configured isolated publication checks passed for commit `" + head + "`.\n"
                   + "Review this PR before merging; Forge does not merge or push main."}
        return self._validated(job, head, self._api("POST", "repos/" + repository + "/pulls", payload))
