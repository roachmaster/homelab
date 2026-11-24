#!/usr/bin/python3 -I
"""Root-owned fixed CI supervisor; untrusted source executes as dedicated forge_ci.

Jenkins calls this one helper through its narrow sudo rule. No command, URL,
credential or filesystem destination is accepted from build parameters.
"""
from abc import ABC, abstractmethod
from contextlib import contextmanager
from dataclasses import dataclass
from pathlib import Path
import ctypes
import fcntl
import grp
import hashlib
import json
import os
import platform
import pwd
import re
import selectors
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import time
from uuid import UUID

CONFIG = Path('/etc/forge-ci/pipeline.json')
MAX_SOURCE_BYTES = 128 * 1024 * 1024
MAX_META_BYTES = 16 * 1024 * 1024
COPY_CHUNK_BYTES = 1024 * 1024
CACHE_SCHEMA_VERSION = 1
TRUSTED_CACHE_ROOT_PREFIX = Path('/mnt/data/forge-homelab')
DEFAULT_CACHE_ROOT = Path('/mnt/data/forge-homelab/ci-cache')
DEFAULT_MAX_TOTAL_BYTES = 20 * 1024 * 1024 * 1024
DEFAULT_MAX_ENTRIES = 8
DEFAULT_MAX_AGE_SECONDS = 14 * 86400
EPHEMERAL_TARGET_DIRS = frozenset({'ui-assets', 'ui-qa', 'tmp'})
# Directory names excluded from snapshots at any depth in both the main target
# tree and the package-targets tree. Cargo incremental artifacts only accelerate
# rebuilds of changed crates: unchanged crates reuse deps/ plus fingerprints,
# and changed crates rebuild cleanly without them. Snapshotting incremental/
# costs gigabytes per entry and grows on every warm cycle, because restored
# sessions plus the new build's sessions accumulate. rustdoc output (doc/) and
# generated AST/dead-code reports (ast/) are pure build outputs that Cargo
# never reads back for compilation reuse.
SNAPSHOT_EXCLUDED_DIR_NAMES = frozenset({'incremental', 'doc', 'ast'})
REPO_BUILD_CONFIG_PATHS = (
    '.cargo/config',
    '.cargo/config.toml',
    'src/rust/.cargo/config',
    'src/rust/.cargo/config.toml',
    'rust-toolchain',
    'rust-toolchain.toml',
)
COMPILER_ENV_KEYS = (
    'RUSTFLAGS',
    'CARGO_ENCODED_RUSTFLAGS',
    'RUSTDOCFLAGS',
    'CARGO_INCREMENTAL',
    'CC',
    'CFLAGS',
)
CHECKS = (
    ('/usr/bin/make', 'ci'),
    ('/usr/bin/make', 'ui-qa'),
    ('/usr/bin/python3', '-B', '-m', 'unittest', 'discover', '-s',
     'deploy/mission-control/agent-runner', '-p', 'test_*.py', '-v'),
)
CHECK_LABELS = ('make ci', 'make ui-qa', 'unittest')
LEGACY_PROFILE = 'forge-sdlc-jenkins-v1'
SCOPED_PROFILE = 'forge-sdlc-jenkins-v2'
SCOPED_BRANCH_CHECKS = (
    ('/usr/bin/make', 'ci-preflight'),
    ('/usr/bin/make', 'ci'),
    ('/usr/bin/python3', '-B', 'deploy/mission-control/backend-suites.py', '--scope', 'branch'),
)
SCOPED_MAIN_CHECKS = (
    ('/usr/bin/make', 'ci-verified'),
    ('/usr/bin/make', 'ui-qa'),
    ('/usr/bin/python3', '-B', 'deploy/mission-control/backend-suites.py', '--scope', 'main'),
)
V2_PROTECTED_PATHS = frozenset({
    'deploy/mission-control/agent-runner/work_delivery_ops.py',
    'deploy/mission-control/agent-runner/work_delivery_worker.py',
    'deploy/mission-control/agent-runner/work_delivery_repair.py',
    'deploy/mission-control/portal_release_actor.py',
    'deploy/mission-control/agent-runner/work_api.py',
    'deploy/mission-control/agent-runner/work_config.py',
    'deploy/mission-control/agent-runner/work_store.py',
    'deploy/mission-control/agent-runner/work_workflow.py',
    'deploy/mission-control/sql/migrations/0029_delivery_automation.sql',
    'deploy/mission-control/sql/management/verify-delivery.sql',
    'deploy/mission-control/backend-suites.py',
    'deploy/mission-control/agent-runner/work_ci_contract.py',
    'deploy/mission-control/agent-runner/work_post_merge.py',
    'deploy/mission-control/agent-runner/work_post_merge_provider.py',
    'deploy/mission-control/agent-runner/work_publication.py',
    'deploy/mission-control/agent-runner/work_worker_store.py',
    'deploy/mission-control/sql/migrations/0028_post_merge_verification.sql',
    'deploy/mission-control/sql/management/verify-post-merge.sql',
    'deploy/mission-control/sql/apply.sql',
    'deploy/mission-control/sql/management/grant-runner.sql',
    'deploy/mission-control/sql/management/verify.sql',
    'deploy/mission-control/sql/management/verify-runner-privileges.sql',
    'deploy/mission-control/sql/management/grant-muse-read-access.sql',
    'deploy/mission-control/sql/management/verify-muse-read-access.sql',
    'deploy/mission-control/sql/management/verify-autonomy.sql',
    'deploy/mission-control/sql/management/verify-external-workflows.sql',
    'deploy/mission-control/agent-runner/work_native_jenkins.py',
    'deploy/mission-control/agent-runner/work_publication_config.py',
    'deploy/mission-control/agent-runner/work_native_repair.py',
    'deploy/mission-control/agent-runner/work_review.py',
    'deploy/mission-control/agent-runner/work_external_delivery.py',
    'deploy/mission-control/jenkins/verify-candidate.py',
    'deploy/mission-control/jenkins/setup-ci.py',
})

BRANCH_CHECKS = SCOPED_BRANCH_CHECKS
BRANCH_CHECK_LABELS = ('source and Python preflight', 'make ci', 'in-memory backend')

# Closed v3 phase policy; mirrored from work_ci_phases, never imported from a candidate.
EVENT_PROFILE = 'forge-sdlc-jenkins-v3'
FAILURE_KINDS = frozenset({'compilation','tests','browser','lint','formatting',
    'planning','review','merge','infrastructure','verification','feedback_delivery',
    'reviewer_correction'})
ALL_FEATURES = ('PACKAGE=', 'FEATURES=', 'ALL_FEATURES=1', 'NO_DEFAULT_FEATURES=0')
FEATURE_VARIANTS = (
    ('remote-https', 'forge-remote-https', '', True),
    ('ide-remote', 'forge-ide', 'remote', True),
    ('ide-minimal', 'forge-ide', '', True),
    ('template-minimal', 'forge-template', '', True),
    ('terminal-minimal', 'forge-terminal', '', True),
    ('local-minimal', 'forge-local', '', False),
    ('terminal-jobs', 'forge-terminal', 'jobs', True),
    ('terminal-disk', 'forge-terminal', 'disk', True),
    ('system-node', 'forge-system', 'node', False),
    ('web-minimal', 'forge-ide-web', '', True),
)


@dataclass(frozen=True)
class CiPhase:
    phase_id: str
    kind: str
    argv: tuple[str, ...]
    logical_gate: str = 'check'


def event_phases_for_scope(branch_scope: str) -> tuple[CiPhase, ...]:
    """Preserve the complete v2 feature matrix, with a test-build barrier."""
    phases = [
        CiPhase('source-preflight', 'verification', ('/usr/bin/make','ci-preflight'),
            'check' if branch_scope == 'main' else 'source-preflight'),
        CiPhase('script-tests', 'tests', ('/usr/bin/python3','-B','-m','unittest','discover',
            '-s','src/rust/scripts/tests','-v')),
        CiPhase('formatting', 'formatting', ('/usr/bin/make','fmt-check','PACKAGE=','CHANGED_ONLY=0')),
        CiPhase('imports', 'verification', ('/usr/bin/make','check-imports')),
        CiPhase('dependencies', 'verification', ('/usr/bin/make','check-deps')),
    ]
    for target in ('build','check','test-compile','test-release-compile'):
        phases.append(CiPhase('all-features-'+target, 'compilation', ('/usr/bin/make',target,*ALL_FEATURES)))
    for identity, package, features, _lint in FEATURE_VARIANTS:
        flags = ('PACKAGE='+package,'FEATURES='+features,'ALL_FEATURES=0','NO_DEFAULT_FEATURES=1')
        phases.append(CiPhase(identity+'-test-compile','compilation',('/usr/bin/make','test-compile',*flags)))
    phases.append(CiPhase('ui-compilation','compilation',('/usr/bin/make','ci-ui-compile')))
    for target, kind in (('lint','lint'),('test','tests'),('test-release','tests'),('doc','verification')):
        phases.append(CiPhase('all-features-'+target,kind,('/usr/bin/make',target,*ALL_FEATURES)))
    for identity, package, features, lint in FEATURE_VARIANTS:
        flags = ('PACKAGE='+package,'FEATURES='+features,'ALL_FEATURES=0','NO_DEFAULT_FEATURES=1')
        phases.append(CiPhase(identity+'-tests','tests',('/usr/bin/make','test',*flags)))
        if lint:
            phases.append(CiPhase(identity+'-lint','lint',('/usr/bin/make','lint',*flags)))
    phases.extend((
        CiPhase('ui-contracts','verification',('/usr/bin/make','ui-check')),
        CiPhase('browser-driver-tests','tests',('/usr/bin/make','ui-qa-check')),
    ))
    if branch_scope == 'main':
        phases.extend((
            CiPhase('browser-journeys','browser',('/usr/bin/make','ui-qa'),'real-browser'),
            CiPhase('backend-main','tests',('/usr/bin/python3','-B','deploy/mission-control/backend-suites.py','--scope','main'),'backend-suites'),
        ))
    else:
        phases.append(CiPhase('backend-memory','tests',('/usr/bin/python3','-B',
            'deploy/mission-control/backend-suites.py','--scope','branch'),'in-memory-backend'))
    return tuple(phases)


def event_stage_evidence(branch_scope: str, checks: list, failure: str | None) -> dict:
    """Bind observed exit codes to the closed plan, never to stdout markers."""
    phases = event_phases_for_scope(branch_scope)
    if failure is not None and (not isinstance(failure,str) or not failure):
        raise ValueError('Invalid typed phase failure reason')
    if not isinstance(checks,list) or len(checks)>len(phases):
        raise ValueError('Invalid v3 phase execution count')
    records=[]
    for index,check in enumerate(checks):
        phase=phases[index]
        if (not isinstance(check,dict) or set(check)!={'index','argv','returncode'}
                or type(check['index']) is not int or check['index']!=index
                or check['argv']!=list(phase.argv) or type(check['returncode']) is not int
                or not -255<=check['returncode']<=255 or (index<len(checks)-1 and check['returncode']!=0)):
            raise ValueError('Phase result does not match actual closed execution order')
        records.append(dict(index=index,phase_id=phase.phase_id,kind=phase.kind,
            argv=list(phase.argv),returncode=check['returncode']))
    failed=records[-1] if records and records[-1]['returncode'] else None
    if failure is None and (failed is not None or len(records)!=len(phases)):
        raise ValueError('Incomplete phase execution cannot claim success')
    passed=sorted({phase.kind for phase in phases
        if all(index<len(records) and records[index]['returncode']==0
            for index,item in enumerate(phases) if item.kind==phase.kind)})
    if failure=='source_changed_during_checks':
        return dict(failure_kind='verification',passed_kinds=[],phase_results=records)
    if failure in {'cancelled_or_timed_out','verification_unavailable','deadline_exceeded'}:
        kind='infrastructure'
    elif failed is not None:
        kind='infrastructure' if failed['returncode']==124 or failed['returncode']<0 else failed['kind']
    elif failure is not None:
        kind='verification'
    else:
        kind=None
    return dict(failure_kind=kind,passed_kinds=passed,phase_results=records)


def checks_for_scope(branch_scope, profile=LEGACY_PROFILE):
    """Closed operator-selected execution policy; v1 remains full on every scope."""
    if profile == EVENT_PROFILE:
        phases=event_phases_for_scope(branch_scope)
        return tuple(p.argv for p in phases),tuple(p.phase_id for p in phases)
    if profile != SCOPED_PROFILE:
        # Legacy trusted test profiles retain the same full CHECKS policy;
        # the executable composition root accepts only the two public profiles.
        return CHECKS, CHECK_LABELS
    if branch_scope == 'main':
        return SCOPED_MAIN_CHECKS, ('source preflight and make ci', 'make ui-qa', 'all backend suites')
    return SCOPED_BRANCH_CHECKS, ('source and Python preflight', 'make ci', 'in-memory backend')


def profile_coverage(profile, branch_scope):
    if profile not in {SCOPED_PROFILE,EVENT_PROFILE}:
        return None
    main = branch_scope == 'main'
    return {'scope': 'main' if main else 'branch', 'browser': 'required' if main else 'not_run',
            'database': 'required_zero_skips' if main else 'not_run',
            'backend_suites': 'all_four' if main else 'agent_runner_in_memory'}


def profile_gate_manifest(profile, branch_scope, protected, helper_digest):
    commands, _labels = checks_for_scope(branch_scope, profile)
    policy = {'version': 3 if profile == EVENT_PROFILE else 2 if profile == SCOPED_PROFILE else 1, 'provider': 'jenkins',
        'profile': profile, 'checks': commands if profile in {SCOPED_PROFILE,EVENT_PROFILE} else CHECKS,
        'verifier_sha256': helper_digest,
        'source_gate_sha256': hashlib.sha256(canonical(protected)).hexdigest()}
    if profile in {SCOPED_PROFILE,EVENT_PROFILE}:
        policy['branch_scope'] = branch_scope
    if profile == EVENT_PROFILE:
        policy['phase_plan']=[dict(phase_id=p.phase_id,kind=p.kind,argv=list(p.argv),logical_gate=p.logical_gate)
            for p in event_phases_for_scope(branch_scope)]
    return hashlib.sha256(canonical(policy)).hexdigest()

SHA = re.compile('[0-9a-f]{40}')
DIGEST = re.compile('[0-9a-f]{64}')
BRANCH_NAME_RE = re.compile(r'[A-Za-z0-9][A-Za-z0-9._/-]{0,127}')
BRANCH_SLUG_RE = re.compile(r'[a-z0-9](?:[a-z0-9._-]{0,56}[a-z0-9])?')
BRANCH_SCOPE_RE = re.compile(r'branch/[a-z0-9](?:[a-z0-9._-]{0,56}[a-z0-9])?')
CARGO_COMPILING_RE = re.compile(r'^\s*Compiling\s+\S+')
CARGO_FINISHED_RE = re.compile(
    r'^\s*Finished\s+.*?\s+in\s+((?:\d+h\s*)?(?:\d+m\s*)?\d+(?:\.\d+)?s)\s*$'
)
DURATION_PART_RE = re.compile(r'(?:(\d+)h\s*)?(?:(\d+)m\s*)?(\d+(?:\.\d+)?)s')


class UnsafeArtifactError(ValueError):
    """Raised when build-owned scratch contains an unsafe artifact entry."""


class CorruptSnapshotError(ValueError):
    """Raised when a cached snapshot fails integrity or structural checks."""


class MainAncestorRequired(ValueError):
    """The pushed source does not yet contain its observed main baseline."""


def main_ancestor_preflight(source, request, env):
    """Read only the imported bundle; never fetch or synthesize an untested merge."""
    expected = request['expected_main']
    commit = bounded_output(['/usr/bin/git', '-C', str(source), 'rev-parse', '--verify',
                             expected + '^{commit}'], env=env).decode().strip()
    if commit != expected:
        raise ValueError('Expected main commit is unavailable from the verified bundle')
    result = subprocess.run(['/usr/bin/git', '-C', str(source), 'merge-base', '--is-ancestor',
                             expected, request['candidate_commit']], env=env,
                            stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                            stderr=subprocess.DEVNULL, timeout=30, check=False)
    if result.returncode == 1:
        raise MainAncestorRequired()
    if result.returncode != 0:
        raise ValueError('Expected main ancestry cannot be verified')


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(',', ':'), ensure_ascii=False).encode()


def protected_gate_path(path):
    name = path.rsplit('/', 1)[-1]
    return (path.startswith('.github/') or path.startswith('.cargo/') or '/.cargo/' in path
        or path.startswith('src/rust/scripts/') or path.startswith('tests/') or '/tests/' in path
        or path.startswith('.config/') or '/.config/' in path
        or path.startswith('deploy/mission-control/jenkins/') or name.startswith('test_')
        or name in {'Makefile', 'makefile', 'GNUmakefile', 'Cargo.toml', 'Cargo.lock', 'build.rs',
            'rust-toolchain', 'rust-toolchain.toml', 'clippy.toml', '.clippy.toml', 'rustfmt.toml',
            '.rustfmt.toml', 'deny.toml', 'work_publication_checks.py', 'work_verification.py',
            'work_verification_init.py', 'work_policy.py', 'work_external_delivery.py'})


def profile_protected_gate_path(path, profile):
    return (protected_gate_path(path)
        or path == 'deploy/mission-control/agent-runner/work_ci_contract.py'
        or profile == EVENT_PROFILE and (path in {
            'deploy/mission-control/agent-runner/work_ci_phases.py',
            'deploy/mission-control/agent-runner/work_retry_policy.py',
            'deploy/mission-control/agent-runner/work_retry_events.py',
            'deploy/mission-control/agent-runner/work_reviewer_correction.py',
            'deploy/mission-control/agent-runner/work_superseding_review.py',
            'deploy/mission-control/agent-runner/work_retry_continuation.py',
            'deploy/mission-control/agent-runner/work_context_recovery.py',
            'deploy/mission-control/sql/management/verify-retry-events.sql',
            'deploy/mission-control/sql/management/verify-delivery-lineage.sql',
            'deploy/mission-control/sql/management/verify-superseding-reviews.sql',
        } or path.startswith('deploy/mission-control/sql/migrations/0031_')
            or path.startswith('deploy/mission-control/sql/fragments/0031_')
            or path.startswith('deploy/mission-control/sql/migrations/0032_')
            or path.startswith('deploy/mission-control/sql/fragments/0032_')
            or path.startswith('deploy/mission-control/sql/migrations/0033_')
            or path.startswith('deploy/mission-control/sql/fragments/0033_'))
        or profile in {SCOPED_PROFILE,EVENT_PROFILE} and path in V2_PROTECTED_PATHS)


# --- Pure branch-identity & compatibility helpers (no I/O or provisioning) ---


def validate_branch_name(value):
    if (not isinstance(value, str) or not BRANCH_NAME_RE.fullmatch(value)
            or '..' in value or '//' in value or '@{' in value
            or value.endswith('/') or value.endswith('.')):
        raise ValueError('Invalid Git branch name')
    for part in value.split('/'):
        if part in {'', '.', '..'} or part.startswith('.') or part.endswith('.lock'):
            raise ValueError('Invalid Git branch name component')
    return value


def canonical_branch_slug(branch_name):
    branch = validate_branch_name(branch_name)
    digest16 = hashlib.sha256(branch.encode('utf-8')).hexdigest()[:16]
    sanitized = re.sub(r'[^a-z0-9._-]+', '-', branch.lower())
    sanitized = re.sub(r'-{2,}', '-', sanitized).strip('._-')
    prefix = sanitized[:40].rstrip('._-')
    if not prefix or not prefix[0].isalnum():
        prefix = 'b'
    slug = f'{prefix}-{digest16}'
    if not BRANCH_SLUG_RE.fullmatch(slug):
        raise ValueError('Canonical branch slug failed validation')
    return slug


def derive_branch_scope(*, candidate_commit, expected_main, source_branch):
    if (not isinstance(candidate_commit, str) or not SHA.fullmatch(candidate_commit)
            or not isinstance(expected_main, str) or not SHA.fullmatch(expected_main)):
        raise ValueError('Invalid commit identity for branch scope derivation')
    if source_branch is None:
        return 'main' if candidate_commit == expected_main else 'detached'
    branch = validate_branch_name(source_branch)
    if branch == 'main':
        if candidate_commit != expected_main:
            raise ValueError('Non-main candidate cannot claim main branch scope')
        return 'main'
    return 'branch/' + canonical_branch_slug(branch)


def validate_branch_scope(scope, *, candidate_commit, expected_main):
    if (not isinstance(candidate_commit, str) or not SHA.fullmatch(candidate_commit)
            or not isinstance(expected_main, str) or not SHA.fullmatch(expected_main)):
        raise ValueError('Invalid commit identity for branch scope validation')
    if not isinstance(scope, str):
        raise ValueError('Invalid CI branch scope')
    if scope == 'main':
        if candidate_commit != expected_main:
            raise ValueError('Main branch scope requires candidate_commit == expected_main')
        return 'main'
    if scope == 'detached':
        return 'detached'
    if '..' not in scope and BRANCH_SCOPE_RE.fullmatch(scope):
        return scope
    raise ValueError('Invalid CI branch scope')


def scope_directory_name(branch_scope):
    if branch_scope == 'main':
        return 'main'
    if branch_scope == 'detached':
        return None
    if isinstance(branch_scope, str) and '..' not in branch_scope and BRANCH_SCOPE_RE.fullmatch(branch_scope):
        return 'branch--' + branch_scope.split('/', 1)[1]
    raise ValueError('Invalid CI branch scope for storage directory')


def extract_compiler_and_feature_env(environment):
    selected = {}
    for key in sorted(environment):
        if (key in COMPILER_ENV_KEYS
                or key.startswith('CARGO_PROFILE_')
                or key.startswith('CARGO_BUILD_')):
            selected[key] = environment[key]
    return selected


def compute_compat_key(
    *,
    project_alias,
    verification_profile,
    checks,
    toolchain_fingerprint,
    host_platform,
    compiler_and_feature_env,
    repo_build_config,
    canonical_build_root,
):
    payload = {
        'cache_schema_version': CACHE_SCHEMA_VERSION,
        'project_alias': project_alias,
        'verification_profile': verification_profile,
        'checks': [list(argv) for argv in checks],
        'toolchain': toolchain_fingerprint,
        'host_platform': host_platform,
        'compiler_and_feature_env': dict(sorted(compiler_and_feature_env.items())),
        'repo_build_config': dict(sorted(repo_build_config.items())),
        'canonical_build_root': str(canonical_build_root),
    }
    return hashlib.sha256(canonical(payload)).hexdigest(), payload


def _is_strict_int(value):
    return type(value) is int


def validate_publish_timestamps(
    *,
    published_at_ns,
    max_artifact_mtime_ns,
    max_fingerprint_mtime_ns,
    source_inventory,
):
    if (not _is_strict_int(published_at_ns)
            or not _is_strict_int(max_artifact_mtime_ns)
            or not _is_strict_int(max_fingerprint_mtime_ns)
            or not isinstance(source_inventory, dict)
            or not source_inventory):
        return False, 'invalid_timestamp_values'
    if published_at_ns <= 0:
        return False, 'non_positive_published_at'
    if max_artifact_mtime_ns <= 0:
        return False, 'non_positive_artifact_mtime'
    if max_artifact_mtime_ns > published_at_ns:
        return False, 'future_artifact_mtime'
    if max_fingerprint_mtime_ns < 0 or max_fingerprint_mtime_ns > max_artifact_mtime_ns:
        return False, 'invalid_fingerprint_mtime'
    for entry in source_inventory.values():
        if not isinstance(entry, dict) or not _is_strict_int(entry.get('mtime_ns')):
            return False, 'invalid_source_inventory_mtime'
        mtime_ns = entry['mtime_ns']
        if mtime_ns <= 0:
            return False, 'non_positive_source_mtime'
        if mtime_ns > published_at_ns:
            return False, 'future_source_mtime'
    return True, None


def validate_restore_timestamp_ordering(
    *,
    published_at_ns,
    observed_max_artifact_mtime_ns,
    observed_max_fingerprint_mtime_ns,
    source_inventory,
    restore_now_ns,
    min_fresh_checkout_mtime_ns,
    min_changed_source_mtime_ns=None,
):
    if (not _is_strict_int(published_at_ns)
            or not _is_strict_int(observed_max_artifact_mtime_ns)
            or not _is_strict_int(observed_max_fingerprint_mtime_ns)
            or not _is_strict_int(restore_now_ns)
            or not _is_strict_int(min_fresh_checkout_mtime_ns)
            or not isinstance(source_inventory, dict)
            or not source_inventory):
        return False, 'invalid_timestamp_values'
    if min_changed_source_mtime_ns is not None and not _is_strict_int(min_changed_source_mtime_ns):
        return False, 'invalid_timestamp_values'
    if published_at_ns <= 0 or restore_now_ns <= 0 or min_fresh_checkout_mtime_ns <= 0:
        return False, 'non_positive_timestamp'
    if published_at_ns > restore_now_ns:
        return False, 'future_published_at'
    if observed_max_artifact_mtime_ns <= 0 or observed_max_artifact_mtime_ns > published_at_ns:
        return False, 'artifact_after_publish'
    if (observed_max_fingerprint_mtime_ns < 0
            or observed_max_fingerprint_mtime_ns > observed_max_artifact_mtime_ns):
        return False, 'fingerprint_after_artifact'
    max_cached_source_mtime_ns = 0
    for entry in source_inventory.values():
        if not isinstance(entry, dict) or not _is_strict_int(entry.get('mtime_ns')):
            return False, 'invalid_source_inventory_mtime'
        mtime_ns = entry['mtime_ns']
        if mtime_ns <= 0:
            return False, 'non_positive_cached_source_mtime'
        if mtime_ns > published_at_ns:
            return False, 'cached_source_after_publish'
        if mtime_ns > max_cached_source_mtime_ns:
            max_cached_source_mtime_ns = mtime_ns
    if observed_max_artifact_mtime_ns >= min_fresh_checkout_mtime_ns:
        return False, 'artifact_not_older_than_checkout'
    if observed_max_fingerprint_mtime_ns >= min_fresh_checkout_mtime_ns:
        return False, 'fingerprint_not_older_than_checkout'
    if max_cached_source_mtime_ns >= min_fresh_checkout_mtime_ns:
        return False, 'cached_source_not_older_than_checkout'
    if published_at_ns >= min_fresh_checkout_mtime_ns:
        return False, 'publish_not_older_than_checkout'
    if min_changed_source_mtime_ns is not None and min_changed_source_mtime_ns < min_fresh_checkout_mtime_ns:
        return False, 'changed_source_before_checkout'
    return True, None


def parse_cargo_duration_ms(text):
    match = DURATION_PART_RE.fullmatch(text.strip())
    if not match:
        return 0
    hours = int(match.group(1) or 0)
    minutes = int(match.group(2) or 0)
    seconds = float(match.group(3))
    return int(round((hours * 3600.0 + minutes * 60.0 + seconds) * 1000.0))


def parse_cargo_telemetry(log_bytes):
    compiling_units = 0
    finished_count = 0
    cargo_compile_ms = 0
    for line in log_bytes.decode('utf-8', errors='replace').splitlines():
        if CARGO_COMPILING_RE.match(line):
            compiling_units += 1
        else:
            finished = CARGO_FINISHED_RE.match(line)
            if finished:
                finished_count += 1
                cargo_compile_ms += parse_cargo_duration_ms(finished.group(1))
    return {
        'cargo_compiling_units': compiling_units,
        'cargo_finished_count': finished_count,
        'cargo_compile_ms': cargo_compile_ms,
        'cargo_compile_ms_kind': 'cargo_reported_cumulative_phase_ms',
    }


# --- Typed cache configuration, ports & providers (Wire Forge Providers) ---


@dataclass(frozen=True)
class CargoCacheConfig:
    enabled: bool = False
    root: Path = DEFAULT_CACHE_ROOT
    max_total_bytes: int = DEFAULT_MAX_TOTAL_BYTES
    max_entries: int = DEFAULT_MAX_ENTRIES
    max_age_seconds: int = DEFAULT_MAX_AGE_SECONDS
    seed_from_main: bool = True
    seed_from_siblings: bool = True

    @classmethod
    def parse(cls, raw, *, trusted_prefix=None):
        if raw is None:
            return cls(enabled=False)
        if not isinstance(raw, dict):
            raise ValueError('Invalid cargo_cache configuration')
        allowed = {'enabled', 'root', 'max_total_bytes', 'max_entries', 'max_age_seconds', 'seed_from_main',
                   'seed_from_siblings'}
        if not set(raw) <= allowed:
            raise ValueError('Unexpected cargo_cache configuration keys')
        enabled = raw.get('enabled', True)
        if type(enabled) is not bool:
            raise ValueError('cargo_cache.enabled must be boolean')
        seed_from_main = raw.get('seed_from_main', True)
        if type(seed_from_main) is not bool:
            raise ValueError('cargo_cache.seed_from_main must be boolean')
        seed_from_siblings = raw.get('seed_from_siblings', True)
        if type(seed_from_siblings) is not bool:
            raise ValueError('cargo_cache.seed_from_siblings must be boolean')
        raw_root = raw.get('root', str(DEFAULT_CACHE_ROOT))
        if not isinstance(raw_root, str) or not raw_root:
            raise ValueError('cargo_cache.root must be an absolute path string')
        root = Path(raw_root)
        prefix = TRUSTED_CACHE_ROOT_PREFIX if trusted_prefix is None else Path(trusted_prefix)
        if (not root.is_absolute() or '..' in root.parts or root.is_symlink()
                or prefix not in root.parents):
            raise ValueError('cargo_cache.root must live under the trusted Forge data prefix')
        max_total_bytes = raw.get('max_total_bytes', DEFAULT_MAX_TOTAL_BYTES)
        max_entries = raw.get('max_entries', DEFAULT_MAX_ENTRIES)
        max_age_seconds = raw.get('max_age_seconds', DEFAULT_MAX_AGE_SECONDS)
        for value in (max_total_bytes, max_entries, max_age_seconds):
            if type(value) is not int or value <= 0:
                raise ValueError('cargo_cache retention bounds must be positive integers')
        return cls(
            enabled=enabled,
            root=root,
            max_total_bytes=max_total_bytes,
            max_entries=max_entries,
            max_age_seconds=max_age_seconds,
            seed_from_main=seed_from_main,
            seed_from_siblings=seed_from_siblings,
        )


@dataclass(frozen=True)
class CacheKey:
    project_alias: str
    branch_scope: str
    scope_dir: str | None
    compat_key: str
    compat_inputs: dict


@dataclass(frozen=True)
class ActiveSlotLayout:
    root: Path
    source: Path
    scratch: Path
    target: Path
    package_targets: Path
    cargo_home: Path
    home: Path
    qa: Path
    package_target_hashes: tuple[str, ...] = ()


@dataclass(frozen=True)
class CacheRestoreOutcome:
    status: str
    fallback_reason: str | None = None
    elapsed_ms: int = 0
    restored_bytes: int = 0
    restored_files: int = 0
    source_scope: str | None = None
    snapshot_id: str | None = None
    details: dict | None = None


@dataclass(frozen=True)
class CachePublishOutcome:
    status: str
    elapsed_ms: int = 0
    published_bytes: int = 0
    published_files: int = 0
    evicted_entries: int = 0
    cache_total_bytes: int = 0
    cache_entry_count: int = 0
    snapshot_id: str | None = None
    reason: str | None = None


@dataclass(frozen=True)
class PruneOutcome:
    evicted_entries: int
    cache_total_bytes: int
    cache_entry_count: int


@dataclass(frozen=True)
class ClearOutcome:
    removed_entries: int
    freed_bytes: int
    cache_total_bytes: int
    cache_entry_count: int


@contextmanager
def deferred_cancellation():
    """Defer TERM/INT only during rollback or the final receipt commit.

    The caller checks pending cancellation before the durable success marker.
    Signals after that terminal boundary cannot invalidate an already committed
    passed receipt. SIGKILL is handled by durable journal reconciliation.
    """
    pending = False
    def remember(_number, _frame):
        nonlocal pending
        pending = True
    old = {number: signal.signal(number, remember) for number in (signal.SIGTERM, signal.SIGINT)}
    def check():
        if pending:
            raise InterruptedError('Jenkins requested verification cancellation')
    try:
        yield check
    finally:
        for number, handler in old.items():
            signal.signal(number, handler)


def mark_cache_rolled_back(result, *, inventory=None):
    cache = result['cache']
    if cache['publish_status'] == 'published':
        cache['publish_status'] = 'rolled_back'
        cache['published_bytes'] = 0
        cache['published_files'] = 0
        cache['cache_total_bytes'] = inventory[0] if inventory is not None else None
        cache['cache_entry_count'] = inventory[1] if inventory is not None else None
        for stage in result['stages']:
            if stage['name'] == 'cache_publish':
                stage.update(status='rolled_back', published_bytes=0, published_files=0, evicted_entries=0)


class CargoCachePort(ABC):
    provider_name: str
    enabled: bool

    @property
    def terminal_result_committed(self):
        return False

    @contextmanager
    def verification_transaction(self):
        """Own optional publication until the supervisor has a durable result."""
        yield

    def recover_pending(self, *, action_id=None, result_path=None):
        """Resolve durable provisional publications before result replay."""

    def rollback_result(self, result):
        mark_cache_rolled_back(result)

    def finish_result(self, path, result, reader_group, *, cancelled):
        if result['status'] == 'passed' and cancelled.exists():
            raise InterruptedError('Verification cancelled before result commit')
        write_result(path, result, reader_group)

    @abstractmethod
    def resolve_key(
        self,
        *,
        project_alias,
        branch_scope,
        verification_profile,
        checks,
        toolchain_path,
        environment,
        repo_build_config,
        canonical_build_root,
    ) -> CacheKey:
        raise NotImplementedError

    @abstractmethod
    def restore(self, key: CacheKey, layout: ActiveSlotLayout, tree_blobs: dict, account) -> CacheRestoreOutcome:
        raise NotImplementedError

    @abstractmethod
    def publish(
        self,
        key: CacheKey,
        layout: ActiveSlotLayout,
        source_inventory: dict,
        account,
        *,
        action_id: str,
    ) -> CachePublishOutcome:
        raise NotImplementedError

    @abstractmethod
    def prune(self) -> PruneOutcome:
        raise NotImplementedError

    @abstractmethod
    def clear(self, *, project=None, scope=None) -> ClearOutcome:
        raise NotImplementedError


class NullCargoCacheProvider(CargoCachePort):
    """Zero-effect cache provider selected when caching is disabled or substituted."""

    provider_name = 'null'
    enabled = False

    def __init__(self, config: CargoCacheConfig):
        self.config = config

    def resolve_key(
        self,
        *,
        project_alias,
        branch_scope,
        verification_profile,
        checks,
        toolchain_path,
        environment,
        repo_build_config,
        canonical_build_root,
    ) -> CacheKey:
        compat_key, compat_inputs = compute_compat_key(
            project_alias=project_alias,
            verification_profile=verification_profile,
            checks=checks,
            toolchain_fingerprint={'provider': 'null'},
            host_platform={'platform': sys.platform, 'machine': platform.machine()},
            compiler_and_feature_env=extract_compiler_and_feature_env(environment),
            repo_build_config=repo_build_config,
            canonical_build_root=canonical_build_root,
        )
        return CacheKey(
            project_alias=project_alias,
            branch_scope=branch_scope,
            scope_dir=scope_directory_name(branch_scope),
            compat_key=compat_key,
            compat_inputs=compat_inputs,
        )

    def restore(self, key: CacheKey, layout: ActiveSlotLayout, tree_blobs: dict, account) -> CacheRestoreOutcome:
        return CacheRestoreOutcome(status='disabled')

    def publish(
        self,
        key: CacheKey,
        layout: ActiveSlotLayout,
        source_inventory: dict,
        account,
        *,
        action_id: str,
    ) -> CachePublishOutcome:
        return CachePublishOutcome(status='disabled')

    def prune(self) -> PruneOutcome:
        return PruneOutcome(evicted_entries=0, cache_total_bytes=0, cache_entry_count=0)

    def clear(self, *, project=None, scope=None) -> ClearOutcome:
        return ClearOutcome(removed_entries=0, freed_bytes=0, cache_total_bytes=0, cache_entry_count=0)


# --- Low-level supervisor & safe filesystem primitives ---


def read_file(path, *, owner, maximum):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, 'rb') as stream:
        info = os.fstat(stream.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_uid != owner or info.st_mode & 0o022 or info.st_nlink != 1 or info.st_size > maximum:
            raise ValueError('Unsafe CI input')
        data = stream.read(maximum + 1)
        if len(data) > maximum:
            raise ValueError('Oversized CI input')
        return data


def regular_directory(path, *, owner):
    info = path.lstat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != owner or info.st_mode & 0o022:
        raise ValueError('Unsafe CI directory')


def _fsync_dir(path):
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def _atomic_write_json(path, value, *, mode=0o600):
    temporary = path.with_name(path.name + '.' + str(os.getpid()) + '.new')
    if temporary.exists() or temporary.is_symlink():
        temporary.unlink()
    fd = os.open(temporary, os.O_CREAT | os.O_EXCL | os.O_WRONLY | os.O_NOFOLLOW, mode)
    try:
        with os.fdopen(fd, 'wb') as stream:
            os.fchmod(stream.fileno(), mode)
            stream.write(canonical(value) + b'\n')
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
        _fsync_dir(path.parent)
    finally:
        if temporary.exists() or temporary.is_symlink():
            temporary.unlink()


def _safe_rmtree(path):
    try:
        info = path.lstat()
    except FileNotFoundError:
        return
    if not stat.S_ISDIR(info.st_mode) or not shutil.rmtree.avoids_symlink_attacks:
        raise ValueError('Unsafe CI directory removal')
    def _make_writable(func, target, _exc):
        if isinstance(_exc[1], InterruptedError):
            raise _exc[1]
        target_path = Path(target)
        parent = target_path.parent
        try:
            parent_info = parent.lstat()
            if stat.S_ISDIR(parent_info.st_mode):
                os.chmod(parent, parent_info.st_mode | 0o700)
            entry_info = target_path.lstat()
            if stat.S_ISDIR(entry_info.st_mode):
                os.chmod(target_path, entry_info.st_mode | 0o700)
        except InterruptedError:
            raise
        except OSError:
            pass
        func(target)
    shutil.rmtree(path, onerror=_make_writable)


def _hash_regular_file(path):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    try:
        info = os.fstat(fd)
        if not stat.S_ISREG(info.st_mode):
            raise ValueError('Expected regular toolchain file')
        hasher = hashlib.sha256()
        with os.fdopen(fd, 'rb') as stream:
            fd = None
            while True:
                chunk = stream.read(COPY_CHUNK_BYTES)
                if not chunk:
                    break
                hasher.update(chunk)
        return hasher.hexdigest()
    finally:
        if fd is not None:
            os.close(fd)


def observe_toolchain_fingerprint(toolchain_path):
    toolchain = Path(toolchain_path)
    safe_env = {'PATH': str(toolchain / 'bin') + ':/usr/bin:/bin', 'HOME': '/nonexistent', 'LANG': 'C.UTF-8'}
    rustc_bin = toolchain / 'bin/rustc'
    cargo_bin = toolchain / 'bin/cargo'
    rustc_vv = bounded_output([str(rustc_bin), '-vV'], env=safe_env, maximum=65536).decode('utf-8', errors='replace').strip()
    cargo_v = bounded_output([str(cargo_bin), '-V'], env=safe_env, maximum=65536).decode('utf-8', errors='replace').strip()
    binaries = {
        'rustc': _hash_regular_file(rustc_bin),
        'cargo': _hash_regular_file(cargo_bin),
    }
    clippy_bin = toolchain / 'bin/clippy-driver'
    if clippy_bin.exists() and not clippy_bin.is_symlink():
        binaries['clippy-driver'] = _hash_regular_file(clippy_bin)
    rustlib_manifests = {}
    rustlib = toolchain / 'lib/rustlib'
    if rustlib.is_dir() and not rustlib.is_symlink():
        for entry in sorted(os.listdir(rustlib)):
            if entry == 'components' or entry.startswith('manifest-'):
                candidate = rustlib / entry
                if not candidate.is_symlink() and candidate.is_file():
                    rustlib_manifests[entry] = _hash_regular_file(candidate)
    return {
        'rustc_vv': rustc_vv,
        'cargo_v': cargo_v,
        'binaries': binaries,
        'rustlib_manifests': rustlib_manifests,
    }


def _copy_tree_from_build(src_root, dst_root, *, expected_uid, prefix_rel, excluded_top_dirs=frozenset(),
                        excluded_dir_names=frozenset()):
    """Safely copy regular files from build-owned scratch into root staging.

    Uses O_NOFOLLOW + fstat on every directory and file component and rejects
    symlinks, FIFOs, sockets, devices, or files not owned by expected_uid.
    `excluded_top_dirs` skips top-level entries; `excluded_dir_names` skips
    directories of that name at any depth. Files are NOT fsynced here: staging
    is disposable until os.replace publishes it, so publish() issues a single
    os.sync() at the snapshot boundary instead of one fsync per file.
    """
    manifest_entries = []
    total_bytes = 0
    file_count = 0
    max_artifact_mtime_ns = 0
    max_fingerprint_mtime_ns = 0

    def _walk(src_dir_fd, dst_dir, rel_parts, depth):
        nonlocal total_bytes, file_count, max_artifact_mtime_ns, max_fingerprint_mtime_ns
        names = sorted(os.listdir(src_dir_fd))
        for name in names:
            if name in {'', '.', '..'} or '/' in name or '\x00' in name:
                raise UnsafeArtifactError('Unsafe artifact filename')
            if depth == 0 and name in excluded_top_dirs:
                continue
            entry_fd = None
            try:
                try:
                    entry_fd = os.open(
                        name,
                        os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK,
                        dir_fd=src_dir_fd,
                    )
                except InterruptedError:
                    raise
                except OSError as error:
                    raise UnsafeArtifactError('Rejected symlink or unreadable artifact entry') from error
                info = os.fstat(entry_fd)
                if info.st_uid != expected_uid:
                    raise UnsafeArtifactError('Rejected artifact not owned by build user')
                child_parts = rel_parts + (name,)
                if stat.S_ISDIR(info.st_mode):
                    if name in excluded_dir_names:
                        continue
                    # Verify directory fd explicitly with O_DIRECTORY | O_NOFOLLOW.
                    sub_dir_fd = os.open(
                        name,
                        os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
                        dir_fd=src_dir_fd,
                    )
                    os.close(entry_fd)
                    entry_fd = None
                    try:
                        sub_dst = dst_dir / name
                        sub_dst.mkdir(mode=0o700, exist_ok=True)
                        _walk(sub_dir_fd, sub_dst, child_parts, depth + 1)
                    finally:
                        os.close(sub_dir_fd)
                elif stat.S_ISREG(info.st_mode):
                    dst_file = dst_dir / name
                    mode = info.st_mode & 0o777
                    mtime_ns = info.st_mtime_ns
                    dst_fd = os.open(dst_file, os.O_CREAT | os.O_EXCL | os.O_WRONLY | os.O_NOFOLLOW, 0o600)
                    hasher = hashlib.sha256()
                    copied = 0
                    try:
                        with os.fdopen(entry_fd, 'rb') as src_stream:
                            entry_fd = None
                            with os.fdopen(dst_fd, 'wb') as dst_stream:
                                dst_fd = None
                                while True:
                                    chunk = src_stream.read(COPY_CHUNK_BYTES)
                                    if not chunk:
                                        break
                                    dst_stream.write(chunk)
                                    hasher.update(chunk)
                                    copied += len(chunk)
                                dst_stream.flush()
                                os.fchmod(dst_stream.fileno(), mode)
                                os.utime(dst_stream.fileno(), ns=(mtime_ns, mtime_ns))
                                dst_info = os.fstat(dst_stream.fileno())
                    finally:
                        if dst_fd is not None:
                            os.close(dst_fd)
                    actual_mtime_ns = dst_info.st_mtime_ns
                    rel_path = '/'.join((prefix_rel, *child_parts))
                    manifest_entries.append([rel_path, copied, mode, actual_mtime_ns, hasher.hexdigest()])
                    total_bytes += copied
                    file_count += 1
                    if actual_mtime_ns > max_artifact_mtime_ns:
                        max_artifact_mtime_ns = actual_mtime_ns
                    if '.fingerprint' in child_parts and actual_mtime_ns > max_fingerprint_mtime_ns:
                        max_fingerprint_mtime_ns = actual_mtime_ns
                else:
                    raise UnsafeArtifactError('Rejected non-regular artifact entry')
            finally:
                if entry_fd is not None:
                    os.close(entry_fd)

    root_fd = os.open(src_root, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        root_info = os.fstat(root_fd)
        if not stat.S_ISDIR(root_info.st_mode) or root_info.st_uid != expected_uid:
            raise UnsafeArtifactError('Build artifact root is not owned by build user')
        dst_root.mkdir(mode=0o700, exist_ok=True)
        _walk(root_fd, dst_root, (), 0)
    finally:
        os.close(root_fd)
    return manifest_entries, total_bytes, file_count, max_artifact_mtime_ns, max_fingerprint_mtime_ns


def _copy_tree_to_build(src_root, dst_root, *, expected_owner, target_uid, target_gid, prefix_rel):
    """Copy verified regular snapshot files into build-owned scratch preserving mtime_ns."""
    manifest_entries = []
    total_bytes = 0
    file_count = 0
    observed_max_artifact_mtime_ns = 0
    observed_max_fingerprint_mtime_ns = 0

    def _walk(src_dir_fd, dst_dir, rel_parts):
        nonlocal total_bytes, file_count, observed_max_artifact_mtime_ns, observed_max_fingerprint_mtime_ns
        for name in sorted(os.listdir(src_dir_fd)):
            if name in {'', '.', '..'} or '/' in name or '\x00' in name:
                raise CorruptSnapshotError('Invalid snapshot entry name')
            entry_fd = None
            try:
                try:
                    entry_fd = os.open(
                        name,
                        os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK,
                        dir_fd=src_dir_fd,
                    )
                except InterruptedError:
                    raise
                except OSError as error:
                    raise CorruptSnapshotError('Snapshot contains symlink or unreadable entry') from error
                info = os.fstat(entry_fd)
                if info.st_uid != expected_owner:
                    raise CorruptSnapshotError('Snapshot entry owner mismatch')
                child_parts = rel_parts + (name,)
                if stat.S_ISDIR(info.st_mode):
                    sub_dir_fd = os.open(
                        name,
                        os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
                        dir_fd=src_dir_fd,
                    )
                    os.close(entry_fd)
                    entry_fd = None
                    try:
                        sub_dst = dst_dir / name
                        if sub_dst.is_symlink():
                            raise CorruptSnapshotError('Destination symlink collision')
                        sub_dst.mkdir(mode=0o700, exist_ok=True)
                        os.chown(sub_dst, target_uid, target_gid)
                        sub_dst.chmod(0o700)
                        _walk(sub_dir_fd, sub_dst, child_parts)
                    finally:
                        os.close(sub_dir_fd)
                elif stat.S_ISREG(info.st_mode):
                    dst_file = dst_dir / name
                    mode = info.st_mode & 0o777
                    mtime_ns = info.st_mtime_ns
                    dst_fd = os.open(dst_file, os.O_CREAT | os.O_EXCL | os.O_WRONLY | os.O_NOFOLLOW, 0o600)
                    hasher = hashlib.sha256()
                    copied = 0
                    try:
                        with os.fdopen(entry_fd, 'rb') as src_stream:
                            entry_fd = None
                            with os.fdopen(dst_fd, 'wb') as dst_stream:
                                dst_fd = None
                                while True:
                                    chunk = src_stream.read(COPY_CHUNK_BYTES)
                                    if not chunk:
                                        break
                                    dst_stream.write(chunk)
                                    hasher.update(chunk)
                                    copied += len(chunk)
                                dst_stream.flush()
                                os.fchown(dst_stream.fileno(), target_uid, target_gid)
                                os.fchmod(dst_stream.fileno(), mode)
                                os.utime(dst_stream.fileno(), ns=(mtime_ns, mtime_ns))
                                dst_info = os.fstat(dst_stream.fileno())
                    finally:
                        if dst_fd is not None:
                            os.close(dst_fd)
                    actual_mtime_ns = dst_info.st_mtime_ns
                    rel_path = '/'.join((prefix_rel, *child_parts))
                    manifest_entries.append([rel_path, copied, mode, actual_mtime_ns, hasher.hexdigest()])
                    total_bytes += copied
                    file_count += 1
                    if actual_mtime_ns > observed_max_artifact_mtime_ns:
                        observed_max_artifact_mtime_ns = actual_mtime_ns
                    if '.fingerprint' in child_parts and actual_mtime_ns > observed_max_fingerprint_mtime_ns:
                        observed_max_fingerprint_mtime_ns = actual_mtime_ns
                else:
                    raise CorruptSnapshotError('Snapshot contains non-regular file')
            finally:
                if entry_fd is not None:
                    os.close(entry_fd)

    root_fd = os.open(src_root, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        root_info = os.fstat(root_fd)
        if not stat.S_ISDIR(root_info.st_mode) or root_info.st_uid != expected_owner:
            raise CorruptSnapshotError('Snapshot root directory invalid')
        dst_root.mkdir(mode=0o700, exist_ok=True)
        os.chown(dst_root, target_uid, target_gid)
        dst_root.chmod(0o700)
        _walk(root_fd, dst_root, ())
    finally:
        os.close(root_fd)
    return (
        manifest_entries,
        total_bytes,
        file_count,
        observed_max_artifact_mtime_ns,
        observed_max_fingerprint_mtime_ns,
    )


def _reset_scratch_targets(layout: ActiveSlotLayout, account):
    for root in (layout.target, layout.package_targets):
        if root.exists() or root.is_symlink():
            _safe_rmtree(root)
        root.mkdir(mode=0o700)
        os.chown(root, account.pw_uid, account.pw_gid)
        root.chmod(0o700)
    for digest_name in layout.package_target_hashes:
        owned = layout.package_targets / digest_name
        owned.mkdir(mode=0o700, exist_ok=True)
        os.chown(owned, account.pw_uid, account.pw_gid)
        owned.chmod(0o700)


def _snapshot_manifest_digest(meta_body):
    fields = {
        'schema_version': meta_body['schema_version'],
        'project_alias': meta_body['project_alias'],
        'branch_scope': meta_body['branch_scope'],
        'compat_key': meta_body['compat_key'],
        'published_at_ns': meta_body['published_at_ns'],
        'max_artifact_mtime_ns': meta_body['max_artifact_mtime_ns'],
        'max_fingerprint_mtime_ns': meta_body['max_fingerprint_mtime_ns'],
        'source_inventory': meta_body['source_inventory'],
        'file_count': meta_body['file_count'],
        'total_bytes': meta_body['total_bytes'],
        'files_sha256': meta_body['files_sha256'],
    }
    return hashlib.sha256(canonical(fields)).hexdigest()


class LocalCargoCacheProvider(CargoCachePort):
    """Supervisor-owned local persistent Cargo build cache under cache_root."""

    provider_name = 'local'
    enabled = True

    def __init__(self, config: CargoCacheConfig, *, expected_owner: int = 0, expected_group: int | None = None):
        self.config = config
        self.expected_owner = expected_owner
        self._publication = None
        self._publication_lock_context = None
        self._verification_active = False
        self._maintenance_evicted = 0
        self._rollback_inventory = None
        self._unusable_reason = None
        self._terminal_result_committed = False
        if expected_group is None:
            self.expected_group = expected_owner if expected_owner == 0 else os.getgid()
        else:
            self.expected_group = expected_group

    @property
    def terminal_result_committed(self):
        return self._terminal_result_committed

    @contextmanager
    def verification_transaction(self):
        if self._verification_active:
            raise ValueError('CI cache transaction is already active')
        self._verification_active = True
        self._terminal_result_committed = False
        try:
            yield
        finally:
            try:
                if (self._publication is not None
                        and (self._publication / 'pending-publication.json').exists()):
                    # A Python exception, cleanup failure or uncommitted return
                    # must never leave the provisional pointer authoritative.
                    with deferred_cancellation():
                        self._resolve_publication_locked(self._publication, commit=False)
                    self._publication = None
            finally:
                if self._publication_lock_context is not None:
                    try:
                        self._publication_lock_context.__exit__(None, None, None)
                    except OSError:
                        if not self._terminal_result_committed:
                            raise
                    finally:
                        self._publication_lock_context = None
                self._verification_active = False

    @contextmanager
    def _publication_lock(self):
        if not self._verification_active or self._publication_lock_context is not None:
            raise ValueError('Cache publication requires its supervisor transaction')
        context = self._cache_lock()
        root = context.__enter__()
        try:
            yield root
        finally:
            if self._publication is None:
                context.__exit__(None, None, None)
            else:
                # Hold the exclusive lock through log close, UID/scratch
                # cleanup and final root receipt publication. Other cache users
                # cannot reconcile/clear the previous or provisional snapshot.
                self._publication_lock_context = context

    def _optional_metadata(self, path, maximum):
        if not path.exists() and not path.is_symlink():
            return None
        return read_file(path, owner=self.expected_owner, maximum=maximum).hex()

    def _restore_metadata(self, path, encoded):
        if encoded is None:
            if path.exists() or path.is_symlink():
                path.unlink()
                _fsync_dir(path.parent)
            return
        # Preserve the exact before-image, including formatting and newline.
        raw = bytes.fromhex(encoded)
        temporary = path.with_name(path.name + '.rollback')
        if temporary.exists() or temporary.is_symlink():
            read_file(temporary, owner=self.expected_owner, maximum=16384)
            temporary.unlink()
        fd = os.open(temporary, os.O_CREAT | os.O_EXCL | os.O_WRONLY | os.O_NOFOLLOW, 0o600)
        with os.fdopen(fd, 'wb') as stream:
            stream.write(raw)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
        _fsync_dir(path.parent)

    def _read_publication_locked(self, entry_dir):
        path = entry_dir / 'pending-publication.json'
        body = json.loads(read_file(path, owner=self.expected_owner, maximum=131072))
        expected = {'version', 'action_id', 'old_current', 'old_last_used', 'new_pointer', 'result', 'committed'}
        if (not isinstance(body, dict) or set(body) != expected or body['version'] != 1
                or type(body['committed']) is not bool
                or not isinstance(body['action_id'], str)
                or str(UUID(body['action_id'])) != body['action_id']):
            raise ValueError('Invalid pending cache publication')
        self._validate_pointer(body['new_pointer'])
        if not body['new_pointer']['snapshot_id'].endswith('-' + body['action_id']):
            raise ValueError('Pending cache action mismatch')
        for name, maximum in (('old_current', 16384), ('old_last_used', 4096)):
            value = body[name]
            if value is not None:
                if not isinstance(value, str) or len(value) > maximum * 2:
                    raise ValueError('Invalid cache rollback before-image')
                decoded = bytes.fromhex(value)
                parsed = json.loads(decoded)
                if name == 'old_current':
                    self._validate_pointer(parsed)
                elif (not isinstance(parsed, dict) or not _is_strict_int(parsed.get('last_used_at_ns'))):
                    raise ValueError('Invalid cache rollback last-use metadata')
        result = body['result']
        if result is not None:
            if (not isinstance(result, dict) or set(result) != {'path', 'sha256'}
                    or not isinstance(result['path'], str)
                    or not isinstance(result['sha256'], str)
                    or not DIGEST.fullmatch(result['sha256'])):
                raise ValueError('Invalid cache result binding')
            result_path = Path(result['path'])
            if (not result_path.is_absolute() or '..' in result_path.parts
                    or result_path.name != body['action_id'] + '.json'):
                raise ValueError('Invalid cache result path')
        return body

    def _bound_result_path(self, body):
        result_path = Path(body['result']['path'])
        for parent in result_path.parents:
            if not stat.S_ISDIR(parent.lstat().st_mode):
                raise ValueError('Unsafe cache result ancestor')
        regular_directory(result_path.parent, owner=self.expected_owner)
        return result_path

    def _restore_beforeimages(self, entry_dir, body):
        if body['old_current'] is not None:
            previous_id = json.loads(bytes.fromhex(body['old_current']))['snapshot_id']
            regular_directory(entry_dir / previous_id, owner=self.expected_owner)
        self._restore_metadata(entry_dir / 'current.json', body['old_current'])
        self._restore_metadata(entry_dir / 'last_used.json', body['old_last_used'])
        self._rollback_inventory = None

    def _matching_publication_result(self, body):
        if body['result'] is None:
            return False
        try:
            result = json.loads(read_file(self._bound_result_path(body), owner=self.expected_owner, maximum=1024 * 1024))
        except (FileNotFoundError, json.JSONDecodeError):
            return False
        return (isinstance(result, dict) and result.get('action_id') == body['action_id']
                and result.get('status') == 'passed' and result.get('cleanup_confirmed') is True
                and hashlib.sha256(canonical(result)).hexdigest() == body['result']['sha256'])

    def rollback_result(self, result):
        mark_cache_rolled_back(result, inventory=self._rollback_inventory)

    def _pointed_inventory_locked(self):
        total = 0
        count = 0
        for _project, _scope, _compat, entry in self._entry_directories_locked(self.config.root):
            if not (entry / 'current.json').exists():
                continue
            total += self._read_pointer_locked(entry)['total_bytes']
            count += 1
        return total, count

    def _resolve_publication_locked(self, entry_dir, *, commit=None):
        body = self._read_publication_locked(entry_dir)
        committed = False
        if commit is not False and body['committed']:
            # An unavailable receipt is uncertainty, not evidence that an
            # already committed success should be downgraded. Leave its journal
            # and snapshots intact for recovery; fresh callers can cold-build.
            committed = self._matching_publication_result(body)
        if commit is True and not committed:
            raise ValueError('Cache publication lacks its durable successful result')
        if not committed:
            # Restore the known-good cache BEFORE any fallible receipt access.
            # Persist the abort decision first. A crash during restoration or
            # accounting must never reinterpret this transaction as committed.
            body['committed'] = False
            _atomic_write_json(entry_dir / 'pending-publication.json', body)
            self._restore_beforeimages(entry_dir, body)
            if body['result'] is not None:
                result_path = self._bound_result_path(body)
                temporary = result_path.with_suffix('.new')
                if temporary.exists() or temporary.is_symlink():
                    read_file(temporary, owner=self.expected_owner, maximum=1024 * 1024)
                    temporary.unlink()
                    _fsync_dir(temporary.parent)
                if self._matching_publication_result(body):
                    # A crash may have replaced the success receipt before the
                    # transaction's durable marker. Preserve its genuine source
                    # identities and cleanup proof, but make same-action replay
                    # honestly report the unfinished publication as failed.
                    info = result_path.lstat()
                    result = json.loads(read_file(result_path, owner=self.expected_owner, maximum=1024 * 1024))
                    result.update(status='failed', failure='cancelled_or_timed_out')
                    if result.get('verification_profile') == EVENT_PROFILE:
                        result.update(event_stage_evidence(result['branch_scope'],
                            result['checks'], result['failure']))
                    self.rollback_result(result)
                    write_result(result_path, result, info.st_gid)
                elif result_path.exists() or result_path.is_symlink():
                    observed = json.loads(read_file(result_path, owner=self.expected_owner, maximum=1024 * 1024))
                    if not isinstance(observed, dict) or observed.get('status') != 'failed':
                        raise ValueError('Pending cache publication has a conflicting result')
            new_snapshot = entry_dir / body['new_pointer']['snapshot_id']
            if new_snapshot.exists() or new_snapshot.is_symlink():
                regular_directory(new_snapshot, owner=self.expected_owner)
                _safe_rmtree(new_snapshot)
        else:
            current = self._read_pointer_locked(entry_dir)
            if current != body['new_pointer']:
                raise ValueError('Committed cache pointer does not match its result')
            if body['old_current'] is not None:
                previous = json.loads(bytes.fromhex(body['old_current']))['snapshot_id']
                if previous != current['snapshot_id']:
                    old_snapshot = entry_dir / previous
                    if old_snapshot.exists() or old_snapshot.is_symlink():
                        regular_directory(old_snapshot, owner=self.expected_owner)
                        _safe_rmtree(old_snapshot)
        (entry_dir / 'pending-publication.json').unlink()
        _fsync_dir(entry_dir)
        if not committed:
            try:
                self._rollback_inventory = self._pointed_inventory_locked()
            except InterruptedError:
                raise
            except (OSError, ValueError):
                self._rollback_inventory = None

    def recover_pending(self, *, action_id=None, result_path=None):
        if not self.config.root.exists():
            return
        try:
            with self._cache_lock() as root, deferred_cancellation() as check_cancelled:
                try:
                    for _project, _scope, _compat, entry in self._entry_directories_locked(root):
                        pending = entry / 'pending-publication.json'
                        if pending.exists() or pending.is_symlink():
                            self._resolve_publication_locked(entry)
                finally:
                    # Safe recovery may defer a signal; it must propagate
                    # before replaying a result or launching another build.
                    check_cancelled()
        except InterruptedError:
            raise
        except (OSError, ValueError) as error:
            if result_path is None or result_path.exists() or result_path.is_symlink():
                # A saved same-action result is unsafe until reconciliation.
                raise
            # Unusable optional cache must not prevent an unrelated fresh
            # source verification. Do not alter the unknown transaction or
            # acknowledge its receipt; this scoped provider does no more I/O.
            self._unusable_reason = type(error).__name__ + ': ' + str(error)[:160]

    def finish_result(self, path, result, reader_group, *, cancelled):
        if self._publication is None:
            return super().finish_result(path, result, reader_group, cancelled=cancelled)
        entry_dir = self._publication
        if result['status'] != 'passed':
            with deferred_cancellation():
                self._resolve_publication_locked(entry_dir, commit=False)
            self._publication = None
            self.rollback_result(result)
            return super().finish_result(path, result, reader_group, cancelled=cancelled)
        body = self._read_publication_locked(entry_dir)
        body['result'] = {'path': str(path), 'sha256': hashlib.sha256(canonical(result)).hexdigest()}
        _atomic_write_json(entry_dir / 'pending-publication.json', body)
        with deferred_cancellation() as check_signal:
            def check_cancelled():
                check_signal()
                if cancelled.exists():
                    raise InterruptedError('Verification cancelled before cache/result commit')
            try:
                check_cancelled()
                _atomic_write_json(entry_dir / 'current.json', body['new_pointer'])
                check_cancelled()
                _atomic_write_json(entry_dir / 'last_used.json', {'last_used_at_ns': body['new_pointer']['published_at_ns']})
                check_cancelled()
                write_result(path, result, reader_group)
                check_cancelled()
                # This durable marker is the final success boundary. A crash
                # before it restores the previous cache; a crash after it is
                # completed only with the exact passed+cleanup-confirmed receipt.
                body['committed'] = True
                _atomic_write_json(entry_dir / 'pending-publication.json', body)
                check_cancelled()
            except BaseException:
                self._resolve_publication_locked(entry_dir, commit=False)
                self._publication = None
                raise
            self._terminal_result_committed = True
            self._publication = None
            # No fallible cleanup after the durable success boundary. Leave the
            # committed journal for next-operation reconciliation if unlink or
            # directory fsync is unavailable; success is already authoritative.
            try:
                self._resolve_publication_locked(entry_dir, commit=True)
            except (OSError, ValueError):
                pass

    def _ensure_root(self):
        root = self.config.root
        if root.is_symlink():
            raise ValueError('Unsafe CI cache root symlink')
        if not root.exists():
            root.mkdir(mode=0o700, parents=False, exist_ok=False)
            os.chown(root, self.expected_owner, self.expected_group)
            root.chmod(0o700)
        info = root.lstat()
        if (not stat.S_ISDIR(info.st_mode) or info.st_uid != self.expected_owner
                or info.st_mode & 0o077):
            raise ValueError('Unsafe CI cache root permissions or ownership')
        return root

    @contextmanager
    def _cache_lock(self):
        root = self._ensure_root()
        fd = os.open(root / 'cache.lock', os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
        with os.fdopen(fd, 'w') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            yield root

    def _cleanup_orphaned_staging(self, root, *, keep=None):
        for name in os.listdir(root):
            if name == keep:
                continue
            if name.startswith('.staging-'):
                candidate = root / name
                info = candidate.lstat()
                if info.st_uid != self.expected_owner:
                    raise ValueError('Unsafe CI cache staging ownership')
                if stat.S_ISDIR(info.st_mode):
                    regular_directory(candidate, owner=self.expected_owner)
                    _safe_rmtree(candidate)
                else:
                    candidate.unlink()

    def _ensure_entry_directory(self, root, key):
        entry_dir = root
        for name in ('scopes', key.project_alias, key.scope_dir, key.compat_key):
            entry_dir = entry_dir / name
            entry_dir.mkdir(mode=0o700, exist_ok=True)
            regular_directory(entry_dir, owner=self.expected_owner)
        return entry_dir

    def _scope_has_other_compat_key(self, scope_path, compat_key):
        if not scope_path.exists() or scope_path.is_symlink() or not scope_path.is_dir():
            return False
        for entry in os.listdir(scope_path):
            if entry == compat_key or not DIGEST.fullmatch(entry):
                continue
            current_file = scope_path / entry / 'current.json'
            if current_file.exists() and not current_file.is_symlink():
                return True
        return False

    def _select_sibling_seed(self, scopes_project, key: CacheKey):
        # A retry on a new branch scope reuses the most recent compatible
        # entry from another branch scope: retries usually share the same
        # source state, so a sibling snapshot matches more fingerprints than
        # main's older one, and main seeding stays the fallback. Scope
        # directories may nest (branch names contain slashes), so the scan
        # walks the tree and treats every directory holding a matching
        # compat-key entry as a candidate scope, skipping the requester's own
        # scope and main. Compat-key directories are never descended into:
        # no scope directory lives beneath one. The expected scope comes from
        # the snapshot's own meta.json, because restore requires
        # meta.branch_scope to match it exactly; entries whose pointer or
        # metadata cannot be read are skipped, never fatal.
        if key.scope_dir is None:
            return None
        if not scopes_project.exists() or scopes_project.is_symlink() or not scopes_project.is_dir():
            return None
        own_scope_path = scopes_project / key.scope_dir
        main_scope_path = scopes_project / 'main'
        candidates = []
        for dirpath, dirnames, _filenames in os.walk(scopes_project):
            here = Path(dirpath)
            dirnames[:] = [name for name in dirnames
                           if not DIGEST.fullmatch(name) and not (here / name).is_symlink()]
            if here == own_scope_path or here == main_scope_path:
                continue
            entry = here / key.compat_key
            pointer_file = entry / 'current.json'
            if not pointer_file.exists() or pointer_file.is_symlink():
                continue
            try:
                pointer = json.loads(read_file(pointer_file, owner=self.expected_owner, maximum=16384))
                published_at_ns = pointer['published_at_ns']
                snapshot_id = pointer['snapshot_id']
            except (OSError, ValueError, KeyError, TypeError):
                continue
            if not _is_strict_int(published_at_ns) or not isinstance(snapshot_id, str):
                continue
            candidates.append((published_at_ns, entry, snapshot_id))
        candidates.sort(key=lambda row: row[0], reverse=True)
        for _published_at_ns, entry, snapshot_id in candidates:
            try:
                meta = json.loads(read_file(entry / snapshot_id / 'meta.json',
                                            owner=self.expected_owner, maximum=MAX_META_BYTES))
                sibling_scope = meta['branch_scope']
            except (OSError, ValueError, KeyError, TypeError):
                continue
            if isinstance(sibling_scope, str) and sibling_scope:
                return entry, sibling_scope
        return None

    def _select_candidate_entry(self, root, key: CacheKey):
        scopes_project = root / 'scopes' / key.project_alias
        if key.branch_scope == 'main':
            scope_path = scopes_project / 'main'
            entry = scope_path / key.compat_key
            if (entry / 'current.json').exists():
                return entry, 'hit_main', 'main', False
            if self._scope_has_other_compat_key(scope_path, key.compat_key):
                return None, 'incompatible', None, False
            return None, 'miss', None, False

        if key.branch_scope.startswith('branch/'):
            scope_path = scopes_project / key.scope_dir
            own_entry = scope_path / key.compat_key
            if (own_entry / 'current.json').exists():
                return own_entry, 'hit_branch', key.branch_scope, False
            if self.config.seed_from_siblings:
                sibling = self._select_sibling_seed(scopes_project, key)
                if sibling is not None:
                    sibling_entry, sibling_scope = sibling
                    return sibling_entry, 'hit_sibling_seed', sibling_scope, True
            if self.config.seed_from_main:
                main_scope = scopes_project / 'main'
                main_entry = main_scope / key.compat_key
                if (main_entry / 'current.json').exists():
                    return main_entry, 'hit_main_seed', 'main', True
            if self._scope_has_other_compat_key(scope_path, key.compat_key):
                return None, 'incompatible', None, False
            if self.config.seed_from_main and self._scope_has_other_compat_key(scopes_project / 'main', key.compat_key):
                return None, 'incompatible', None, False
            return None, 'miss', None, False

        # detached scope: strictly read-only main seed only
        if self.config.seed_from_main:
            main_scope = scopes_project / 'main'
            main_entry = main_scope / key.compat_key
            if (main_entry / 'current.json').exists():
                return main_entry, 'hit_main_seed', 'main', True
            if self._scope_has_other_compat_key(main_scope, key.compat_key):
                return None, 'incompatible', None, False
        return None, 'miss', None, False

    def resolve_key(
        self,
        *,
        project_alias,
        branch_scope,
        verification_profile,
        checks,
        toolchain_path,
        environment,
        repo_build_config,
        canonical_build_root,
    ) -> CacheKey:
        toolchain_fingerprint = observe_toolchain_fingerprint(toolchain_path)
        compat_key, compat_inputs = compute_compat_key(
            project_alias=project_alias,
            verification_profile=verification_profile,
            checks=checks,
            toolchain_fingerprint=toolchain_fingerprint,
            host_platform={'platform': sys.platform, 'machine': platform.machine()},
            compiler_and_feature_env=extract_compiler_and_feature_env(environment),
            repo_build_config=repo_build_config,
            canonical_build_root=canonical_build_root,
        )
        return CacheKey(
            project_alias=project_alias,
            branch_scope=branch_scope,
            scope_dir=scope_directory_name(branch_scope),
            compat_key=compat_key,
            compat_inputs=compat_inputs,
        )

    def restore(self, key: CacheKey, layout: ActiveSlotLayout, tree_blobs: dict, account) -> CacheRestoreOutcome:
        started = time.monotonic()
        def elapsed():
            return int((time.monotonic() - started) * 1000)

        if self._unusable_reason is not None:
            return CacheRestoreOutcome(status='corrupt_fallback', fallback_reason=self._unusable_reason)
        if not self.config.root.exists():
            return CacheRestoreOutcome(status='miss', fallback_reason='cache_root_missing', elapsed_ms=elapsed())

        try:
            with self._cache_lock() as root:
                self._cleanup_orphaned_staging(root)
                self._reconcile_snapshots_locked(root)
                own_entry = (root / 'scopes' / key.project_alias / key.scope_dir / key.compat_key
                             if key.scope_dir is not None else None)
                entry_dir, status_label, expected_scope, is_seed = self._select_candidate_entry(root, key)
                if own_entry is not None:
                    reserve_entry = 1 if not (own_entry / 'current.json').exists() else 0
                    # Maintenance precedes the publication transaction baseline;
                    # protect the actual selected seed so main fallback remains
                    # usable even when a new branch needs a retention slot. A
                    # sibling seed is NOT protected: under retention pressure
                    # the sibling yields to main, and the restore falls back
                    # to a clean build (the entry is gone when the copy runs)
                    # rather than evicting main to keep the seed.
                    protect_seed = entry_dir is not None and (not is_seed or expected_scope == 'main')
                    maintenance = self._prune_locked(
                        root, protected_entry=entry_dir if protect_seed else own_entry,
                        reserve_entries=reserve_entry,
                    )
                    self._maintenance_evicted = maintenance.evicted_entries
                if entry_dir is None:
                    reason = 'compat_key_mismatch' if status_label == 'incompatible' else 'no_matching_entry'
                    return CacheRestoreOutcome(status=status_label, fallback_reason=reason, elapsed_ms=elapsed())

                # Observe fresh checkout timestamps BEFORE touching active/source mtimes.
                restore_now_ns = time.time_ns()
                fresh_mtimes = {}
                min_fresh_checkout_mtime_ns = None
                for rel_path in sorted(tree_blobs):
                    info = (layout.source / rel_path).lstat()
                    if not stat.S_ISREG(info.st_mode):
                        raise CorruptSnapshotError('Checked-out source path is not a regular file')
                    fresh_mtimes[rel_path] = info.st_mtime_ns
                    if min_fresh_checkout_mtime_ns is None or info.st_mtime_ns < min_fresh_checkout_mtime_ns:
                        min_fresh_checkout_mtime_ns = info.st_mtime_ns
                if min_fresh_checkout_mtime_ns is None:
                    return CacheRestoreOutcome(status='miss', fallback_reason='empty_source_tree', elapsed_ms=elapsed())

                try:
                    pointer = json.loads(read_file(entry_dir / 'current.json', owner=self.expected_owner, maximum=16384))
                    snapshot_id = pointer.get('snapshot_id')
                    if not isinstance(snapshot_id, str) or not re.fullmatch(r'[0-9]+-[0-9a-f-]{36}', snapshot_id):
                        raise CorruptSnapshotError('Invalid snapshot pointer')
                    snapshot_dir = entry_dir / snapshot_id
                    if not snapshot_dir.exists() or snapshot_dir.is_symlink() or not snapshot_dir.is_dir():
                        raise CorruptSnapshotError('Snapshot directory missing')
                    meta = json.loads(read_file(snapshot_dir / 'meta.json', owner=self.expected_owner, maximum=MAX_META_BYTES))
                    if (not isinstance(meta, dict)
                            or meta.get('schema_version') != CACHE_SCHEMA_VERSION
                            or meta.get('project_alias') != key.project_alias
                            or meta.get('compat_key') != key.compat_key
                            or meta.get('branch_scope') != expected_scope
                            or not isinstance(meta.get('source_inventory'), dict)):
                        raise CorruptSnapshotError('Snapshot metadata mismatch')
                    if _snapshot_manifest_digest(meta) != meta.get('manifest_sha256'):
                        raise CorruptSnapshotError('Snapshot manifest digest mismatch')

                    # Phase 1: Copy verified regular files into scratch & observe actual destination mtimes.
                    t_entries, t_bytes, t_files, t_max_art, t_max_fp = _copy_tree_to_build(
                        snapshot_dir / 'target',
                        layout.target,
                        expected_owner=self.expected_owner,
                        target_uid=account.pw_uid,
                        target_gid=account.pw_gid,
                        prefix_rel='target',
                    )
                    p_entries, p_bytes, p_files, p_max_art, p_max_fp = _copy_tree_to_build(
                        snapshot_dir / 'package-targets',
                        layout.package_targets,
                        expected_owner=self.expected_owner,
                        target_uid=account.pw_uid,
                        target_gid=account.pw_gid,
                        prefix_rel='package-targets',
                    )
                    all_entries = sorted(t_entries + p_entries, key=lambda row: row[0])
                    restored_bytes = t_bytes + p_bytes
                    restored_files = t_files + p_files
                    observed_max_art = max(t_max_art, p_max_art)
                    observed_max_fp = max(t_max_fp, p_max_fp)
                    files_sha256 = hashlib.sha256(canonical(all_entries)).hexdigest()
                    if (files_sha256 != meta.get('files_sha256')
                            or restored_files != meta.get('file_count')
                            or restored_bytes != meta.get('total_bytes')
                            or observed_max_art != meta.get('max_artifact_mtime_ns')
                            or observed_max_fp != meta.get('max_fingerprint_mtime_ns')):
                        raise CorruptSnapshotError('Restored files or mtimes diverged from snapshot metadata')
                except InterruptedError:
                    raise
                except (OSError, ValueError, KeyError, TypeError, CorruptSnapshotError) as error:
                    _reset_scratch_targets(layout, account)
                    return CacheRestoreOutcome(
                        status='corrupt_fallback',
                        fallback_reason=type(error).__name__ + ': ' + str(error)[:160],
                        elapsed_ms=elapsed(),
                        source_scope=expected_scope,
                    )

                # Classify unchanged vs changed/added tracked files before mutating active/source.
                source_inventory = meta['source_inventory']
                unchanged_mtimes = []
                min_changed_source_mtime_ns = None
                for rel_path, (mode, blob) in tree_blobs.items():
                    cached = source_inventory.get(rel_path)
                    if (isinstance(cached, dict)
                            and cached.get('blob') == blob
                            and cached.get('mode') == mode
                            and _is_strict_int(cached.get('mtime_ns'))):
                        unchanged_mtimes.append((rel_path, cached['mtime_ns']))
                    else:
                        fresh_ns = fresh_mtimes[rel_path]
                        if min_changed_source_mtime_ns is None or fresh_ns < min_changed_source_mtime_ns:
                            min_changed_source_mtime_ns = fresh_ns

                ordering_ok, ordering_reason = validate_restore_timestamp_ordering(
                    published_at_ns=meta['published_at_ns'],
                    observed_max_artifact_mtime_ns=observed_max_art,
                    observed_max_fingerprint_mtime_ns=observed_max_fp,
                    source_inventory=source_inventory,
                    restore_now_ns=restore_now_ns,
                    min_fresh_checkout_mtime_ns=min_fresh_checkout_mtime_ns,
                    min_changed_source_mtime_ns=min_changed_source_mtime_ns,
                )
                if not ordering_ok:
                    _reset_scratch_targets(layout, account)
                    return CacheRestoreOutcome(
                        status='timestamp_ordering_fallback',
                        fallback_reason=ordering_reason,
                        elapsed_ms=elapsed(),
                        source_scope=expected_scope,
                        snapshot_id=snapshot_id,
                        details={
                            'published_at_ns': meta['published_at_ns'],
                            'observed_max_artifact_mtime_ns': observed_max_art,
                            'observed_max_fingerprint_mtime_ns': observed_max_fp,
                            'min_fresh_checkout_mtime_ns': min_fresh_checkout_mtime_ns,
                            'min_changed_source_mtime_ns': min_changed_source_mtime_ns,
                            'restore_now_ns': restore_now_ns,
                        },
                    )

                # Phase 2: Timestamp ordering verified; align unchanged source file mtimes.
                for rel_path, cached_ns in unchanged_mtimes:
                    os.utime(layout.source / rel_path, ns=(cached_ns, cached_ns))

                # Update LRU timestamp on the scope's own entry (keep main strictly read-only when seeding).
                if not is_seed:
                    try:
                        _atomic_write_json(entry_dir / 'last_used.json', {'last_used_at_ns': restore_now_ns})
                    except InterruptedError:
                        raise
                    except OSError:
                        pass

                return CacheRestoreOutcome(
                    status=status_label,
                    elapsed_ms=elapsed(),
                    restored_bytes=restored_bytes,
                    restored_files=restored_files,
                    source_scope=expected_scope,
                    snapshot_id=snapshot_id,
                )
        except InterruptedError:
            raise
        except (OSError, ValueError) as error:
            _reset_scratch_targets(layout, account)
            return CacheRestoreOutcome(
                status='corrupt_fallback',
                fallback_reason=type(error).__name__ + ': ' + str(error)[:160],
                elapsed_ms=elapsed(),
            )

    def publish(
        self,
        key: CacheKey,
        layout: ActiveSlotLayout,
        source_inventory: dict,
        account,
        *,
        action_id: str,
    ) -> CachePublishOutcome:
        started = time.monotonic()
        def elapsed():
            return int((time.monotonic() - started) * 1000)

        if self._unusable_reason is not None:
            return CachePublishOutcome(status='skipped_error', reason=self._unusable_reason,
                                       cache_total_bytes=None, cache_entry_count=None)
        if key.scope_dir is None or key.branch_scope == 'detached':
            return CachePublishOutcome(status='skipped_unscoped', elapsed_ms=elapsed(), reason='detached_scope')

        staging = None
        try:
            with self._publication_lock() as root:
                self._cleanup_orphaned_staging(root)
                self._reconcile_snapshots_locked(root)
                baseline_bytes, baseline_count = self._inventory_locked(root)
                published_at_ns = time.time_ns()
                staging = root / ('.staging-' + action_id)
                if staging.exists() or staging.is_symlink():
                    _safe_rmtree(staging)
                staging.mkdir(mode=0o700)
                os.chown(staging, self.expected_owner, self.expected_group)

                try:
                    t_entries, t_bytes, t_files, t_max_art, t_max_fp = _copy_tree_from_build(
                        layout.target,
                        staging / 'target',
                        expected_uid=account.pw_uid,
                        prefix_rel='target',
                        excluded_top_dirs=EPHEMERAL_TARGET_DIRS,
                        excluded_dir_names=SNAPSHOT_EXCLUDED_DIR_NAMES,
                    )
                    p_entries, p_bytes, p_files, p_max_art, p_max_fp = _copy_tree_from_build(
                        layout.package_targets,
                        staging / 'package-targets',
                        expected_uid=account.pw_uid,
                        prefix_rel='package-targets',
                        excluded_dir_names=SNAPSHOT_EXCLUDED_DIR_NAMES,
                    )
                except UnsafeArtifactError as error:
                    _safe_rmtree(staging)
                    staging = None
                    summary = self._inventory_locked(root)
                    return CachePublishOutcome(
                        status='skipped_unsafe_artifact',
                        elapsed_ms=elapsed(),
                        cache_total_bytes=summary[0],
                        cache_entry_count=summary[1],
                        reason=str(error)[:160],
                    )

                all_entries = sorted(t_entries + p_entries, key=lambda row: row[0])
                total_bytes = t_bytes + p_bytes
                file_count = t_files + p_files
                max_art = max(t_max_art, p_max_art)
                max_fp = max(t_max_fp, p_max_fp)

                if file_count == 0:
                    _safe_rmtree(staging)
                    staging = None
                    summary = self._inventory_locked(root)
                    return CachePublishOutcome(
                        status='skipped_empty',
                        elapsed_ms=elapsed(),
                        cache_total_bytes=summary[0],
                        cache_entry_count=summary[1],
                        reason='no_cargo_artifacts_produced',
                    )

                ts_ok, ts_reason = validate_publish_timestamps(
                    published_at_ns=published_at_ns,
                    max_artifact_mtime_ns=max_art,
                    max_fingerprint_mtime_ns=max_fp,
                    source_inventory=source_inventory,
                )
                if not ts_ok:
                    _safe_rmtree(staging)
                    staging = None
                    summary = self._inventory_locked(root)
                    return CachePublishOutcome(
                        status='skipped_unsafe_timestamps',
                        elapsed_ms=elapsed(),
                        cache_total_bytes=summary[0],
                        cache_entry_count=summary[1],
                        reason=ts_reason,
                    )

                if total_bytes > self.config.max_total_bytes:
                    _safe_rmtree(staging)
                    staging = None
                    summary = self._inventory_locked(root)
                    return CachePublishOutcome(
                        status='skipped_oversized',
                        elapsed_ms=elapsed(),
                        cache_total_bytes=summary[0],
                        cache_entry_count=summary[1],
                        reason='snapshot_exceeds_max_total_bytes',
                    )

                entry_dir = self._ensure_entry_directory(root, key)
                has_previous = (entry_dir / 'current.json').exists()
                # Make room for this snapshot by evicting least-recently-used
                # entries before giving up: with multi-gigabyte snapshots a
                # full cache would otherwise stop publishing entirely. The
                # entry being published to is protected so its prior
                # known-good snapshot survives until the new one commits, and
                # the live staging directory is never treated as orphaned.
                # Capacity deliberately counts predecessor AND new snapshot:
                # during publication both exist on disk at once (old snapshot
                # plus staging), so the cap bounds peak disk, not just the
                # steady state after the old snapshot is reconciled away.
                pruned = self._prune_locked(
                    root,
                    protected_entry=entry_dir,
                    reserve_entries=0 if has_previous else 1,
                    reserve_bytes=total_bytes,
                    keep_staging=staging.name,
                )
                self._maintenance_evicted += pruned.evicted_entries
                baseline_bytes, baseline_count = self._inventory_locked(root)
                retained_bytes = baseline_bytes + total_bytes
                retained_count = baseline_count + (0 if has_previous else 1)
                if (retained_bytes > self.config.max_total_bytes
                        or retained_count > self.config.max_entries):
                    _safe_rmtree(staging)
                    staging = None
                    return CachePublishOutcome(
                        status='skipped_capacity', elapsed_ms=elapsed(),
                        cache_total_bytes=baseline_bytes, cache_entry_count=baseline_count,
                        evicted_entries=self._maintenance_evicted,
                        reason='retained_predecessor_and_new_snapshot_exceed_capacity',
                    )

                files_sha256 = hashlib.sha256(canonical(all_entries)).hexdigest()
                meta = {
                    'schema_version': CACHE_SCHEMA_VERSION,
                    'project_alias': key.project_alias,
                    'branch_scope': key.branch_scope,
                    'compat_key': key.compat_key,
                    'published_at_ns': published_at_ns,
                    'max_artifact_mtime_ns': max_art,
                    'max_fingerprint_mtime_ns': max_fp,
                    'source_inventory': source_inventory,
                    'file_count': file_count,
                    'total_bytes': total_bytes,
                    'files_sha256': files_sha256,
                }
                meta['manifest_sha256'] = _snapshot_manifest_digest(meta)
                _atomic_write_json(staging / 'meta.json', meta, mode=0o600)

                entry_dir = self._ensure_entry_directory(root, key)
                snapshot_id = f'{published_at_ns}-{action_id}'
                snapshot_dir = entry_dir / snapshot_id
                if snapshot_dir.exists() or snapshot_dir.is_symlink():
                    _safe_rmtree(snapshot_dir)
                # One filesystem-wide sync makes the staged snapshot durable
                # before it goes live; per-file fsync during the copy was
                # paying 37,000+ syncs for a disposable staging directory.
                # If a crash still tears the snapshot, restore detects it via
                # files_sha256 and falls back to a clean build.
                os.sync()
                os.replace(staging, snapshot_dir)
                staging = None
                _fsync_dir(entry_dir)
                old_current = self._optional_metadata(entry_dir / 'current.json', 16384)
                old_last_used = self._optional_metadata(entry_dir / 'last_used.json', 4096)
                pointer = {
                    'snapshot_id': snapshot_id, 'published_at_ns': published_at_ns,
                    'total_bytes': total_bytes, 'file_count': file_count,
                }
                journal = {
                    'version': 1, 'action_id': action_id,
                    'old_current': old_current, 'old_last_used': old_last_used,
                    'new_pointer': pointer, 'result': None, 'committed': False,
                }
                _atomic_write_json(entry_dir / 'pending-publication.json', journal)
                self._publication = entry_dir
                return CachePublishOutcome(
                    status='published',
                    elapsed_ms=elapsed(),
                    published_bytes=total_bytes,
                    published_files=file_count,
                    evicted_entries=self._maintenance_evicted,
                    cache_total_bytes=retained_bytes,
                    cache_entry_count=retained_count,
                    snapshot_id=snapshot_id,
                )
        except InterruptedError:
            raise
        except (OSError, ValueError) as error:
            if staging is not None and (staging.exists() or staging.is_symlink()):
                try:
                    _safe_rmtree(staging)
                except InterruptedError:
                    raise
                except OSError:
                    pass
            return CachePublishOutcome(
                status='skipped_error',
                elapsed_ms=elapsed(),
                reason=type(error).__name__ + ': ' + str(error)[:160],
            )

    def _entry_directories_locked(self, root):
        """Walk only supervisor-owned directories, never a symlink ancestor."""
        scopes_dir = root / 'scopes'
        if not scopes_dir.exists() and not scopes_dir.is_symlink():
            return
        regular_directory(scopes_dir, owner=self.expected_owner)
        for project_name in sorted(os.listdir(scopes_dir)):
            project_dir = scopes_dir / project_name
            regular_directory(project_dir, owner=self.expected_owner)
            for scope_name in sorted(os.listdir(project_dir)):
                scope_dir = project_dir / scope_name
                regular_directory(scope_dir, owner=self.expected_owner)
                for compat_name in sorted(os.listdir(scope_dir)):
                    entry_dir = scope_dir / compat_name
                    regular_directory(entry_dir, owner=self.expected_owner)
                    yield project_name, scope_name, compat_name, entry_dir

    def _validate_pointer(self, pointer):
        if (not isinstance(pointer, dict)
                or not isinstance(pointer.get('snapshot_id'), str)
                or not re.fullmatch(r'[0-9]+-[0-9a-f-]{36}', pointer['snapshot_id'])
                or not _is_strict_int(pointer.get('published_at_ns'))
                or pointer['published_at_ns'] < 0
                or not _is_strict_int(pointer.get('total_bytes'))
                or pointer['total_bytes'] < 0):
            raise ValueError('Invalid CI cache pointer')

    def _read_pointer_locked(self, entry_dir):
        pointer = json.loads(read_file(entry_dir / 'current.json', owner=self.expected_owner, maximum=16384))
        self._validate_pointer(pointer)
        regular_directory(entry_dir / pointer['snapshot_id'], owner=self.expected_owner)
        return pointer

    def _reconcile_snapshots_locked(self, root):
        """Collect incomplete publications before reporting retention totals.

        The global cache lock excludes a live publisher. An entry with no valid
        pointer is an abandoned pre-commit publication. Within a committed
        entry, only its pointed snapshot is live; crash leftovers and previous
        snapshots are garbage. Removal failures propagate instead of allowing
        unaccounted bytes to disappear from inventory. rmtree is fd-based and
        never follows a link; every traversed ancestor is checked above.
        """
        scopes_dir = root / 'scopes'
        for _project, _scope, _compat, entry_dir in list(self._entry_directories_locked(root)):
            pending = entry_dir / 'pending-publication.json'
            if pending.exists() or pending.is_symlink():
                self._resolve_publication_locked(entry_dir)
            try:
                pointer = self._read_pointer_locked(entry_dir)
            except InterruptedError:
                raise
            except (OSError, ValueError, KeyError, TypeError):
                _safe_rmtree(entry_dir)
                self._remove_empty_parents(entry_dir, scopes_dir)
                continue
            for name in os.listdir(entry_dir):
                if name in {'current.json', 'last_used.json', pointer['snapshot_id']}:
                    continue
                orphan = entry_dir / name
                info = orphan.lstat()
                if info.st_uid != self.expected_owner:
                    raise ValueError('Unsafe CI cache orphan ownership')
                if stat.S_ISDIR(info.st_mode):
                    regular_directory(orphan, owner=self.expected_owner)
                    _safe_rmtree(orphan)
                else:
                    # Unlink a link itself; never open its target.
                    orphan.unlink()

    def _collect_entries_locked(self, root):
        self._reconcile_snapshots_locked(root)
        entries = []
        for project_name, scope_name, compat_name, entry_dir in self._entry_directories_locked(root):
            pointer = self._read_pointer_locked(entry_dir)
            published_at_ns = pointer['published_at_ns']
            last_used_ns = published_at_ns
            last_used_path = entry_dir / 'last_used.json'
            if last_used_path.exists() or last_used_path.is_symlink():
                lu = json.loads(read_file(last_used_path, owner=self.expected_owner, maximum=4096))
                if not isinstance(lu, dict) or not _is_strict_int(lu.get('last_used_at_ns')):
                    raise ValueError('Invalid CI cache last-use metadata')
                last_used_ns = max(last_used_ns, lu['last_used_at_ns'])
            entries.append({
                'project': project_name,
                'scope_dir': scope_name,
                'compat_key': compat_name,
                'path': entry_dir,
                'is_main': scope_name == 'main',
                'published_at_ns': published_at_ns,
                'last_used_at_ns': last_used_ns,
                'total_bytes': pointer['total_bytes'],
            })
        return entries

    def _inventory_locked(self, root):
        entries = self._collect_entries_locked(root)
        return sum(e['total_bytes'] for e in entries), len(entries)

    def _remove_empty_parents(self, entry_dir, scopes_dir):
        for parent in (entry_dir.parent, entry_dir.parent.parent):
            if parent == scopes_dir or scopes_dir not in parent.parents:
                break
            try:
                if parent.exists() and not parent.is_symlink() and not os.listdir(parent):
                    parent.rmdir()
            except InterruptedError:
                raise
            except OSError:
                break

    def _prune_locked(self, root, *, protected_entry=None, now_ns=None, reserve_entries=0, reserve_bytes=0,
                      keep_staging=None):
        self._cleanup_orphaned_staging(root, keep=keep_staging)
        scopes_dir = root / 'scopes'
        entries = self._collect_entries_locked(root)
        now = time.time_ns() if now_ns is None else now_ns
        max_age_ns = self.config.max_age_seconds * 1_000_000_000
        evicted = 0
        remaining = []

        # 1. Evict entries older than max_age_seconds.
        for item in entries:
            if item['path'] != protected_entry and (now - item['last_used_at_ns']) > max_age_ns:
                _safe_rmtree(item['path'])
                self._remove_empty_parents(item['path'], scopes_dir)
                evicted += 1
            else:
                remaining.append(item)

        # 2. Enforce max_entries and max_total_bytes (evict non-main LRU before main).
        # reserve_bytes holds room for a snapshot about to be published.
        while (len(remaining) + reserve_entries > self.config.max_entries
               or sum(item['total_bytes'] for item in remaining) + reserve_bytes > self.config.max_total_bytes):
            candidates = [item for item in remaining if item['path'] != protected_entry]
            if not candidates:
                # An active publication is protected even under a tighter
                # operator retention policy. Never delete its current snapshot.
                break
            candidates.sort(key=lambda item: (
                1 if item['is_main'] else 0,
                item['last_used_at_ns'],
                item['published_at_ns'],
                item['scope_dir'],
                item['compat_key'],
            ))
            victim = candidates[0]
            _safe_rmtree(victim['path'])
            self._remove_empty_parents(victim['path'], scopes_dir)
            remaining.remove(victim)
            evicted += 1

        total_bytes = sum(item['total_bytes'] for item in remaining)
        return PruneOutcome(
            evicted_entries=evicted,
            cache_total_bytes=total_bytes,
            cache_entry_count=len(remaining),
        )

    def prune(self, *, now_ns=None) -> PruneOutcome:
        if not self.config.root.exists():
            return PruneOutcome(evicted_entries=0, cache_total_bytes=0, cache_entry_count=0)
        with self._cache_lock() as root:
            return self._prune_locked(root, now_ns=now_ns)

    def clear(self, *, project=None, scope=None) -> ClearOutcome:
        if scope is not None and project is None:
            raise ValueError('Clearing a specific scope requires --project')
        target_scope_dir = None
        if scope is not None:
            if scope == 'main':
                target_scope_dir = 'main'
            elif '..' not in scope and BRANCH_SCOPE_RE.fullmatch(scope):
                target_scope_dir = 'branch--' + scope.split('/', 1)[1]
            else:
                raise ValueError('Invalid scope for cache clear')
        if not self.config.root.exists():
            return ClearOutcome(removed_entries=0, freed_bytes=0, cache_total_bytes=0, cache_entry_count=0)
        with self._cache_lock() as root:
            self._cleanup_orphaned_staging(root)
            scopes_dir = root / 'scopes'
            entries = self._collect_entries_locked(root)
            removed = 0
            freed = 0
            for item in entries:
                if project is not None and item['project'] != project:
                    continue
                if target_scope_dir is not None and item['scope_dir'] != target_scope_dir:
                    continue
                _safe_rmtree(item['path'])
                self._remove_empty_parents(item['path'], scopes_dir)
                removed += 1
                freed += item['total_bytes']
            if project is None and scope is None and scopes_dir.exists() and not scopes_dir.is_symlink():
                _safe_rmtree(scopes_dir)
            total_bytes, count = self._inventory_locked(root)
            return ClearOutcome(
                removed_entries=removed,
                freed_bytes=freed,
                cache_total_bytes=total_bytes,
                cache_entry_count=count,
            )


class CargoCacheFactory:
    """Effect-free bootstrap factory that freezes CargoCacheConfig before creation."""

    def __init__(self, config: CargoCacheConfig, *, expected_owner: int = 0, expected_group: int | None = None):
        if not isinstance(config, CargoCacheConfig):
            raise TypeError('Expected frozen CargoCacheConfig')
        self.config = config
        self.expected_owner = expected_owner
        self.expected_group = expected_group

    def create(self) -> CargoCachePort:
        if self.config.enabled:
            return LocalCargoCacheProvider(
                self.config,
                expected_owner=self.expected_owner,
                expected_group=self.expected_group,
            )
        return NullCargoCacheProvider(self.config)


# --- Process isolation & execution helpers ---


def private_network():
    # The trusted supervisor retains host networking. Every candidate check gets
    # its own empty network namespace; only that namespace's loopback is raised.
    os.unshare(os.CLONE_NEWNET)
    subprocess.run(['/usr/sbin/ip', 'link', 'set', 'dev', 'lo', 'up'],
        env={'PATH': '/usr/sbin:/usr/bin:/bin'}, check=True, timeout=5,
        stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL, close_fds=True)


def child_identity(account):
    def apply():
        private_network()
        os.setgroups([])
        os.setgid(account.pw_gid)
        os.setuid(account.pw_uid)
        if ctypes.CDLL(None, use_errno=True).prctl(38, 1, 0, 0, 0) != 0:
            raise OSError('Cannot constrain CI child')
        os.umask(0o077)
    return apply


def kill_build_user(account):
    # This UID is exclusively reserved for one Jenkins executor's build children.
    stopped = subprocess.run(['/usr/bin/pkill', '-KILL', '-u', str(account.pw_uid)],
                   stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                   stderr=subprocess.DEVNULL, check=False, timeout=10).returncode
    if stopped not in (0, 1):
        raise RuntimeError('CI process termination could not be verified')
    for _ in range(30):
        observed = subprocess.run(['/usr/bin/pgrep', '-u', str(account.pw_uid)],
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                               check=False, timeout=5).returncode
        if observed == 1:
            return
        if observed != 0:
            raise RuntimeError('CI process observation failed')
        time.sleep(.1)
    raise RuntimeError('CI children did not quiesce')


def run_child(argv, *, cwd, env, account, log, timeout, cancelled):
    process = subprocess.Popen(argv, cwd=cwd, env=env, stdin=subprocess.DEVNULL,
                               stdout=log, stderr=subprocess.STDOUT,
                               preexec_fn=child_identity(account), start_new_session=True,
                               close_fds=True)
    deadline = time.monotonic() + timeout
    try:
        while process.poll() is None:
            if cancelled.exists() or time.monotonic() >= deadline or os.fstat(log.fileno()).st_size > 64 * 1024 * 1024:
                os.killpg(process.pid, signal.SIGTERM)
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                process.wait(timeout=10)
                return 124
            time.sleep(.2)
        return process.returncode
    finally:
        if process.poll() is None:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait(timeout=10)


def bounded_output(argv, *, env, maximum=32 * 1024 * 1024):
    process = subprocess.Popen(argv, env=env, stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, start_new_session=True)
    data = bytearray()
    deadline = time.monotonic() + 30
    try:
        with selectors.DefaultSelector() as selector:
            os.set_blocking(process.stdout.fileno(), False)
            selector.register(process.stdout, selectors.EVENT_READ)
            while selector.get_map():
                if time.monotonic() >= deadline:
                    raise ValueError('Bounded Git observation timed out')
                for key, _mask in selector.select(.2):
                    chunk = os.read(key.fd, 65536)
                    if not chunk:
                        selector.unregister(key.fileobj)
                    else:
                        data.extend(chunk)
                        if len(data) > maximum:
                            raise ValueError('Bounded Git observation exceeded output limit')
        if process.wait(timeout=5) != 0:
            raise ValueError('Git observation failed')
        return bytes(data)
    finally:
        if process.poll() is None:
            os.killpg(process.pid, signal.SIGTERM)
            try:
                process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait(timeout=5)
        process.stdout.close()


def write_result(path, result, reader_group):
    temporary = path.with_suffix('.new')
    fd = os.open(temporary, os.O_CREAT | os.O_EXCL | os.O_WRONLY | os.O_NOFOLLOW, 0o640)
    with os.fdopen(fd, 'w') as stream:
        os.fchown(stream.fileno(), 0, reader_group)
        os.fchmod(stream.fileno(), 0o640)
        json.dump(result, stream, sort_keys=True)
        stream.write('\n')
        stream.flush()
        os.fsync(stream.fileno())
    os.replace(temporary, path)
    _fsync_dir(path.parent)


def import_verified_bundle(bundle, source, env):
    # Delivery bundles intentionally advertise refs/forge/*, not branch heads.
    # Default clone refspecs can silently import nothing from those bundles.
    # Import objects explicitly without materializing any candidate file until
    # its complete tree has passed the privileged checkout validation below.
    subprocess.run(['/usr/bin/git', '-c', 'core.hooksPath=/dev/null', 'init', '--quiet', str(source)],
        env=env, check=True, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL, timeout=15)
    subprocess.run(['/usr/bin/git', '-C', str(source), '-c', 'core.hooksPath=/dev/null',
        'fetch', '--quiet', '--no-tags', '--no-recurse-submodules', '--no-write-fetch-head',
        '--', str(bundle), '+refs/*:refs/forge-ci-import/*'],
        env=env, check=True, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL, timeout=60)


def map_cargo_targets(source, scratch, manifest_parents, account):
    # Parents come only from the complete validated Git tree, never rglob across
    # injected dependency links or candidate-selected filesystem destinations.
    mappings = []
    for relative in sorted(set(manifest_parents)):
        if relative != '.' and (relative.startswith('/')
                or any(part in {'', '.', '..'} for part in relative.split('/'))):
            raise ValueError('Unsafe Cargo manifest directory')
        parent = source
        for part in (() if relative == '.' else relative.split('/')):
            parent = parent / part
            if not stat.S_ISDIR(parent.lstat().st_mode):
                raise ValueError('Cargo parent is not a plain source directory')
        target = parent / 'target'
        if target.exists() or target.is_symlink():
            if (target == source / 'src/rust/target' and target.is_symlink()
                    and target.readlink() == scratch / 'target'):
                continue
            raise ValueError('Candidate includes a Cargo output path')
        mappings.append((relative, target))
    output = scratch / 'package-targets'
    if output.is_symlink():
        raise ValueError('Unsafe package-targets symlink')
    output.mkdir(mode=0o700, exist_ok=True)
    os.chown(output, account.pw_uid, account.pw_gid)
    output.chmod(0o700)
    for relative, target in mappings:
        owned = output / hashlib.sha256(relative.encode('utf-8')).hexdigest()
        owned.mkdir(mode=0o700, exist_ok=True)
        os.chown(owned, account.pw_uid, account.pw_gid)
        owned.chmod(0o700)
        target.symlink_to(owned, target_is_directory=True)
    return [target for _relative, target in mappings]


def exclude_generated_outputs(source, paths):
    # Only exact generated untracked outputs are ignored. Git's cached source
    # inventory remains intact, including any tracked file with a similar name.
    patterns = []
    for path in paths:
        relative = path.relative_to(source).as_posix()
        patterns.append('/' + ''.join('\\' + c if c in '\\*?[]!# ' else c for c in relative))
    with (source / '.git/info/exclude').open('a', encoding='utf-8') as stream:
        stream.write('\n# Trusted CI generated output links only.\n' + '\n'.join(patterns) + '\n')


def create_short_tmp(account):
    # The agent service already supplies PrivateTmp. Keep the literal pathname
    # short enough for AF_UNIX fixtures; the long scratch symlink is provenance.
    path = Path(tempfile.mkdtemp(prefix='fci-', dir='/tmp'))
    try:
        os.chown(path, account.pw_uid, account.pw_gid)
        path.chmod(0o700)
        return path
    except BaseException:
        path.rmdir()
        raise


def cleanup_short_tmp(path):
    # Use the supervisor's captured path, never resolve the candidate-writable
    # scratch/tmp symlink. rmtree's directory-fd implementation rejects links.
    if path.parent != Path('/tmp') or not re.fullmatch(r'fci-[a-z0-9_]{8,32}', path.name):
        raise ValueError('Unexpected CI temporary directory')
    try:
        info = path.lstat()
    except FileNotFoundError:
        return
    if not stat.S_ISDIR(info.st_mode) or not shutil.rmtree.avoids_symlink_attacks:
        raise ValueError('Unsafe CI temporary cleanup')
    shutil.rmtree(path)


def create_job_directory(path):
    path.mkdir(mode=0o711, exist_ok=False)
    path.chmod(0o711)  # The Jenkins service deliberately inherits umask0077.


def open_log(path, reader_group):
    fd = os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY | os.O_NOFOLLOW, 0o640)
    try:
        os.fchown(fd, 0, reader_group)
        os.fchmod(fd, 0o640)
        return fd
    except BaseException:
        os.close(fd)
        raise


def _read_ui_qa_stages(qa_root, *, expected_uid):
    """Safely extract ui-qa stage durations from build-owned QA_OUTPUT_ROOT after quiescence."""
    try:
        root_fd = os.open(qa_root, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    except InterruptedError:
        raise
    except OSError:
        return None
    try:
        root_info = os.fstat(root_fd)
        if not stat.S_ISDIR(root_info.st_mode) or root_info.st_uid != expected_uid:
            return None
        candidates = sorted(n for n in os.listdir(root_fd) if n.startswith('forge-ui-qa-'))
        if not candidates:
            return None
        run_dir_fd = os.open(candidates[-1], os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=root_fd)
        try:
            dir_info = os.fstat(run_dir_fd)
            if not stat.S_ISDIR(dir_info.st_mode) or dir_info.st_uid != expected_uid:
                return None
            file_fd = os.open('run.json', os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=run_dir_fd)
            with os.fdopen(file_fd, 'rb') as stream:
                info = os.fstat(stream.fileno())
                if not stat.S_ISREG(info.st_mode) or info.st_uid != expected_uid or info.st_size > 65536:
                    return None
                raw = stream.read(65537)
                if len(raw) > 65536:
                    return None
        finally:
            os.close(run_dir_fd)
        parsed = json.loads(raw)
        stages = parsed.get('stages')
        if not isinstance(stages, list):
            return None
        summary = {}
        for item in stages:
            if (isinstance(item, dict)
                    and isinstance(item.get('id'), str)
                    and isinstance(item.get('status'), str)
                    and _is_strict_int(item.get('duration_ms'))):
                summary[item['id']] = {
                    'status': item['status'],
                    'duration_ms': item['duration_ms'],
                }
        return summary or None
    except InterruptedError:
        raise
    except (OSError, ValueError, KeyError, TypeError):
        return None
    finally:
        os.close(root_fd)


def _capture_source_inventory(source, tree_blobs):
    inventory = {}
    for rel_path, (mode, blob) in sorted(tree_blobs.items()):
        info = (source / rel_path).lstat()
        if not stat.S_ISREG(info.st_mode):
            raise ValueError('Tracked source file is no longer regular')
        inventory[rel_path] = {
            'blob': blob,
            'mode': mode,
            'mtime_ns': info.st_mtime_ns,
        }
    return inventory


PORTAL_ARTIFACT_POLICY = 'forge-portal-artifact-v1'


def portal_artifact_commands(toolchain):
    return (
        (str(Path(toolchain) / 'bin/cargo'), 'build', '--manifest-path', 'src/rust/Cargo.toml',
         '--offline', '--locked', '--release', '-p', 'forge-ide-web-host', '--jobs', '2'),
        ('/usr/bin/python3', '-B', 'src/rust/scripts/ui_tools.py', 'check'),
    )


def build_portal_artifacts(config, *, source, environment, account, log, log_path,
                           cancelled, started, stages):
    """Two fixed isolated builds; their results never masquerade as a fourth gate."""
    records = []
    output_start = None
    for index, argv in enumerate(portal_artifact_commands(config['toolchain'])):
        remaining = config['timeout_seconds'] - (time.time() - started)
        if remaining <= 0:
            raise InterruptedError('Portal artifact deadline elapsed')
        output_start = log.tell()
        before = time.monotonic()
        code = run_child(argv, cwd=source, env=environment, account=account,
            log=log, timeout=remaining, cancelled=cancelled)
        log.flush()
        stages.append({'name': 'portal_artifact_' + str(index),
            'status': 'passed' if code == 0 else 'failed', 'returncode': code,
            'elapsed_ms': int((time.monotonic() - before) * 1000)})
        if code == 124:
            raise InterruptedError('Portal artifact build cancelled')
        if code != 0:
            raise ValueError('Portal artifact build failed')
        records.append({'argv': list(argv), 'returncode': 0})
    output = log_path.read_bytes()[output_start:log.tell()]
    paths = re.findall(rb'^ASSETS ([^\r\n]+)$', output, re.M)
    if len(paths) != 1:
        raise ValueError('Portal build did not produce one exact asset output')
    return Path(paths[0].decode('utf-8')), records


def read_portal_host(target, *, owner, maximum):
    """Read only Cargo's fixed host output after all build-user processes stopped.

    Cargo links release/NAME to release/deps/NAME-HASH. This is a build-output
    exception, never an exception to the single-link authority/input reader.
    """
    release = target / 'release'
    regular_directory(release, owner=owner)
    host = release / 'forge-ide-web-host'
    alias_fd = None
    def identity(info):
        return (info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns,
                info.st_ctime_ns, info.st_uid, info.st_mode, info.st_nlink)
    def check(info):
        if (not stat.S_ISREG(info.st_mode) or info.st_uid != owner
                or info.st_mode & 0o022 or info.st_nlink not in (1, 2)
                or not 0 < info.st_size <= maximum):
            raise ValueError('Unsafe fixed Cargo host output')
    fd = os.open(host, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    try:
        with os.fdopen(fd, 'rb') as stream:
            before = os.fstat(stream.fileno())
            check(before)
            if identity(host.lstat()) != identity(before):
                raise ValueError('Cargo host output changed before read')
            alias = None
            if before.st_nlink == 2:
                deps = release / 'deps'
                regular_directory(deps, owner=owner)
                matches = []
                for index, path in enumerate(deps.iterdir()):
                    if index >= 65536:
                        raise ValueError('Cargo dependency output bound exceeded')
                    if not re.fullmatch(r'forge_ide_web_host-[0-9a-f]{16,64}', path.name):
                        continue
                    info = path.lstat()
                    if (info.st_dev, info.st_ino) == (before.st_dev, before.st_ino):
                        check(info)
                        if identity(info) != identity(before):
                            raise ValueError('Cargo host alias changed')
                        matches.append(path)
                if len(matches) != 1:
                    raise ValueError('Cargo host requires its exact owned dependency alias')
                alias = matches[0]
                alias_fd = os.open(alias, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
                if identity(os.fstat(alias_fd)) != identity(before):
                    raise ValueError('Cargo host alias was replaced')
            data = stream.read(maximum + 1)
            if (len(data) != before.st_size or len(data) > maximum
                    or identity(os.fstat(stream.fileno())) != identity(before)
                    or identity(host.lstat()) != identity(before)
                    or (alias is not None and (
                        identity(os.fstat(alias_fd)) != identity(before)
                        or identity(alias.lstat()) != identity(before)))):
                raise ValueError('Cargo host output changed while reading')
            return data
    finally:
        if alias_fd is not None:
            os.close(alias_fd)


def seal_portal_artifacts(request, *, request_sha256, layout, assets, build_commands,
                          account, results, reader_group, cancelled):
    """After process cleanup, copy regular build outputs into a Root-owned export."""
    if cancelled.exists():
        raise InterruptedError('Portal export cancelled')
    assets = assets.resolve(strict=True)
    target = layout.target.resolve(strict=True)
    if not assets.is_relative_to(target / 'ui-assets') or assets.name != 'assets':
        raise ValueError('Portal assets escaped their isolated output')
    host = target / 'release/forge-ide-web-host'
    if not host.resolve(strict=True).is_relative_to(target / 'release'):
        raise ValueError('Portal executable escaped its isolated output')
    files = [('bin/forge-ide-web-host', host)]
    for path in sorted(assets.rglob('*')):
        info = path.lstat()
        if stat.S_ISDIR(info.st_mode):
            if info.st_uid != account.pw_uid:
                raise ValueError('Unexpected portal directory owner')
        elif stat.S_ISREG(info.st_mode):
            files.append(('assets/' + path.relative_to(assets).as_posix(), path))
        else:
            raise ValueError('Links and special files are outside portal exports')
    names = {name for name, _path in files}
    if not {'assets/index.html', 'assets/main.js', 'assets/worker.js',
            'assets/style.css', 'assets/app.wasm', 'assets/manifest.json'} <= names:
        raise ValueError('Incomplete portal asset bundle')
    if len(files) > 1024:
        raise ValueError('Portal artifact file bound exceeded')
    payloads, inventory, total = {}, {}, 0
    for name, path in files:
        raw = (read_portal_host(target, owner=account.pw_uid, maximum=64 * 1024 * 1024)
               if name == 'bin/forge-ide-web-host'
               else read_file(path, owner=account.pw_uid, maximum=64 * 1024 * 1024))
        info = path.lstat()
        if (name != 'bin/forge-ide-web-host' and info.st_nlink != 1) or not raw:
            raise ValueError('Linked or empty portal artifact')
        total += len(raw)
        if total > 256 * 1024 * 1024:
            raise ValueError('Portal artifact byte bound exceeded')
        payloads[name] = raw
        inventory[name] = {'bytes': len(raw), 'sha256': hashlib.sha256(raw).hexdigest()}
    manifest = {'version': 1, 'policy': PORTAL_ARTIFACT_POLICY,
        'action_id': request['action_id'],
        'source_commit': request['candidate_commit'], 'source_tree': request['candidate_tree'],
        'request_sha256': request_sha256, 'commands': build_commands,
        'files': [{'path': name, 'sha256': inventory[name]['sha256'],
                   'byte_count': inventory[name]['bytes'], 'executable': name.startswith('bin/')}
                  for name in sorted(inventory)]}
    raw_manifest = canonical(manifest) + b'\n'
    manifest_sha = hashlib.sha256(raw_manifest).hexdigest()
    destination = results / (request['action_id'] + '.artifacts')
    temporary = results / ('.' + request['action_id'] + '.artifacts.new')
    if destination.exists() or destination.is_symlink():
        regular_directory(destination, owner=0)
        actual = {p.relative_to(destination).as_posix() for p in destination.rglob('*')
                  if not p.is_dir() or p.is_symlink()}
        if actual != set(payloads) | {'manifest.json'}:
            raise ValueError('Portal export replay inventory changed')
        if read_file(destination / 'manifest.json', owner=0, maximum=262144) != raw_manifest:
            raise ValueError('Portal export replay changed')
        for name, raw in payloads.items():
            if read_file(destination / name, owner=0, maximum=64 * 1024 * 1024) != raw:
                raise ValueError('Sealed portal export changed')
    else:
        if temporary.is_symlink():
            raise ValueError('Unsafe portal export staging path')
        if temporary.exists():
            regular_directory(temporary, owner=0)
            _safe_rmtree(temporary)
        temporary.mkdir(mode=0o750)
        os.chown(temporary, 0, reader_group)
        for name, raw in [*payloads.items(), ('manifest.json', raw_manifest)]:
            path = temporary / name
            path.parent.mkdir(mode=0o750, parents=True, exist_ok=True)
            fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o440)
            with os.fdopen(fd, 'wb') as stream:
                os.fchown(stream.fileno(), 0, reader_group)
                os.fchmod(stream.fileno(), 0o550 if name.startswith('bin/') else 0o440)
                stream.write(raw); stream.flush(); os.fsync(stream.fileno())
        for directory in sorted([temporary, *[p for p in temporary.rglob('*') if p.is_dir()]], reverse=True):
            os.chown(directory, 0, reader_group)
            directory.chmod(0o550)
            _fsync_dir(directory)
        if cancelled.exists():
            raise InterruptedError('Portal export cancelled before publication')
        os.rename(temporary, destination)
        _fsync_dir(results)
    return {'version': 1, 'policy': PORTAL_ARTIFACT_POLICY, 'action_id': request['action_id'],
        'source_commit': request['candidate_commit'], 'source_tree': request['candidate_tree'],
        'manifest_sha256': manifest_sha, 'file_count': len(files), 'total_bytes': total}


def execute(action, config, *, cache_port=None):
    require_main = config.get('require_main_ancestor', False)
    if type(require_main) is not bool:
        raise ValueError('Main ancestry selection must be an operator boolean')
    if cache_port is None:
        cache_port = CargoCacheFactory(CargoCacheConfig.parse(config.get('cargo_cache'))).create()
    results = Path(config['results_root'])
    regular_directory(results, owner=0)
    fd = os.open(results / 'executor.lock', os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, 'w') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        return execute_locked(action, config, cache_port=cache_port)


def execute_locked(action, config, *, cache_port=None):
    if cache_port is None:
        cache_port = CargoCacheFactory(CargoCacheConfig.parse(config.get('cargo_cache'))).create()
    with cache_port.verification_transaction():
        return _execute_locked(action, config, cache_port=cache_port)


def _execute_locked(action, config, *, cache_port):
    require_main = config.get('require_main_ancestor', False)
    if type(require_main) is not bool:
        raise ValueError('Main ancestry selection must be an operator boolean')
    export_portal = config.get('portal_artifact_export', False)
    if type(export_portal) is not bool:
        raise ValueError('Portal artifact export must be an operator boolean')
    if cache_port is None:
        cache_port = CargoCacheFactory(CargoCacheConfig.parse(config.get('cargo_cache'))).create()
    account = pwd.getpwnam(config['build_user'])
    submitter = pwd.getpwnam(config['submitter_user'])
    if (account.pw_uid == 0 or account.pw_uid == submitter.pw_uid or account.pw_name != 'forge_ci'
            or grp.getgrgid(account.pw_gid).gr_name != 'forge_ci'
            or account.pw_gid in (0, submitter.pw_gid)):
        raise ValueError('CI requires a separate nonroot build account')
    spool = Path(config['spool_root'])
    results = Path(config['results_root'])
    work = Path(config['work_root'])
    regular_directory(spool, owner=submitter.pw_uid)
    regular_directory(results, owner=0)
    directory = spool / action
    regular_directory(directory, owner=submitter.pw_uid)
    raw = read_file(directory / 'request.json', owner=submitter.pw_uid, maximum=65536)
    request = json.loads(raw)
    required = {'version', 'action_id', 'workflow_id', 'project_alias', 'candidate_commit',
                'candidate_tree', 'expected_main', 'gate_manifest_sha256', 'bundle_sha256',
                'verification_profile', 'correlation_id', 'branch_scope'}
    expected_version = 3 if config['verification_profile'] == EVENT_PROFILE else 2 if config['verification_profile'] == SCOPED_PROFILE else 1
    if (set(request) != required or type(request['version']) is not int
            or request['version'] != expected_version or request['action_id'] != action):
        raise ValueError('Invalid CI request')
    for key in ('workflow_id', 'correlation_id'):
        if str(UUID(request[key])) != request[key]:
            raise ValueError('Invalid CI identity')
    for key in ('candidate_commit', 'candidate_tree', 'expected_main'):
        if not isinstance(request[key], str) or not SHA.fullmatch(request[key]):
            raise ValueError('Invalid CI source')
    for key in ('gate_manifest_sha256', 'bundle_sha256'):
        if not isinstance(request[key], str) or not DIGEST.fullmatch(request[key]):
            raise ValueError('Invalid CI digest')
    if request['verification_profile'] != config['verification_profile'] or request['project_alias'] not in config['projects']:
        raise ValueError('CI scope denied')
    # Fail-closed branch scope validation before any cache or build slot filesystem access.
    branch_scope = validate_branch_scope(
        request['branch_scope'],
        candidate_commit=request['candidate_commit'],
        expected_main=request['expected_main'],
    )
    bundle = read_file(directory / 'candidate.bundle', owner=submitter.pw_uid, maximum=MAX_SOURCE_BYTES)
    if hashlib.sha256(bundle).hexdigest() != request['bundle_sha256']:
        raise ValueError('CI bundle changed')
    identity = hashlib.sha256(raw).hexdigest()
    result_path = results / (action + '.json')
    lock_fd = os.open(results / (action + '.lock'), os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    with os.fdopen(lock_fd, 'w') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        cache_port.recover_pending(action_id=action, result_path=result_path)
        if result_path.exists():
            previous = json.loads(read_file(result_path, owner=0, maximum=262144))
            if previous['request_sha256'] != identity:
                raise ValueError('CI replay intent changed')
            return previous
        kill_build_user(account)

        # Canonical stable execution slot under <work_root>/active for effective Cargo reuse.
        active = work / 'active'
        if active.is_symlink():
            raise ValueError('Unsafe CI active slot symlink')
        if active.exists():
            _safe_rmtree(active)
        create_job_directory(active)

        verified_bundle = active / 'verified.bundle'
        bfd = os.open(verified_bundle, os.O_CREAT | os.O_EXCL | os.O_WRONLY | os.O_NOFOLLOW, 0o444)
        with os.fdopen(bfd, 'wb') as stream:
            stream.write(bundle)
            stream.flush()
            os.fsync(stream.fileno())
        scratch = active / 'scratch'
        scratch.mkdir(mode=0o700)
        os.chown(scratch, account.pw_uid, account.pw_gid)
        for name in ('home', 'target', 'cargo-home', 'qa'):
            p = scratch / name
            p.mkdir(mode=0o700)
            os.chown(p, account.pw_uid, account.pw_gid)
        (scratch / 'cargo-home/registry').symlink_to(config['cargo_registry'], target_is_directory=True)
        source = active / 'source'
        environment = {'PATH': config['toolchain'] + '/bin:/usr/bin:/bin',
            'HOME': str(scratch / 'home'),
            'LANG': 'C.UTF-8', 'CARGO_HOME': str(scratch / 'cargo-home'),
            'CARGO_TARGET_DIR': str(scratch / 'target'), 'CARGO_NET_OFFLINE': 'true',
            # Incremental compilation writes multi-gigabyte query-cache trees
            # that only accelerate rebuilds of changed crates; CI reuses deps/
            # plus fingerprints instead. Line-tables-only debug info keeps
            # file/line backtraces while dropping full DWARF from every
            # test binary. Both rotate compat_key via the compiler env inputs.
            'CARGO_INCREMENTAL': '0', 'CARGO_PROFILE_DEV_DEBUG': 'line-tables-only',
            'PYTHONDONTWRITEBYTECODE': '1', 'QA_OUTPUT_ROOT': str(scratch / 'qa'),
            'GIT_CONFIG_NOSYSTEM': '1', 'GIT_CONFIG_GLOBAL': '/dev/null',
            'GIT_NO_REPLACE_OBJECTS': '1', 'GIT_OPTIONAL_LOCKS': '0',
            'GIT_CONFIG_COUNT': '2', 'GIT_CONFIG_KEY_0': 'safe.directory',
            'GIT_CONFIG_VALUE_0': str(source), 'GIT_CONFIG_KEY_1': 'core.hooksPath',
            'GIT_CONFIG_VALUE_1': '/dev/null'}
        active_checks, active_labels = checks_for_scope(branch_scope, config['verification_profile'])
        if config['verification_profile'] in {SCOPED_PROFILE,EVENT_PROFILE} and branch_scope != 'main':
            # DB tests run only on the main pipeline build.
            environment['FORGE_DISABLE_DISPOSABLE_POSTGRES'] = '1'
        cancelled = directory / 'cancelled'
        log_path = results / (action + '.log')
        fd = open_log(log_path, submitter.pw_gid)
        checks = []
        stages = []
        check_log_spans = {}
        started = time.time()
        failure = None
        short_tmp = None
        cache_key = None
        restore_outcome = CacheRestoreOutcome(status='disabled' if not cache_port.enabled else 'not_run')
        publish_outcome = CachePublishOutcome(status='disabled' if not cache_port.enabled else 'not_run')
        tree_blobs = {}
        layout = None
        post_check_quiesced = False
        portal_export = None
        portal_assets = None
        portal_builds = None
        try:
            prep_started = time.monotonic()
            short_tmp = create_short_tmp(account)
            (scratch / 'tmp').symlink_to(short_tmp, target_is_directory=True)
            environment['TMPDIR'] = str(short_tmp)
            # Git only reads the trusted, bounded, digest-bound bundle. Source is
            # then root-owned read-only, with explicit writable scratch symlinks.
            safe_env = {'PATH': '/usr/bin:/bin', 'HOME': '/nonexistent',
                        'GIT_CONFIG_NOSYSTEM': '1', 'GIT_CONFIG_GLOBAL': '/dev/null',
                        'GIT_NO_REPLACE_OBJECTS': '1'}
            import_verified_bundle(verified_bundle, source, safe_env)
            observed = bounded_output(['/usr/bin/git', '-C', str(source), 'rev-parse', request['candidate_commit'] + '^{tree}'], env=safe_env).decode().strip()
            if observed != request['candidate_tree']:
                raise ValueError('CI tree mismatch')
            rows = bounded_output(['/usr/bin/git', '-C', str(source), 'ls-tree', '-r', '-l', '-z', '--full-tree', request['candidate_commit']], env=safe_env)
            protected = {}
            manifest_parents = []
            repo_build_config = {}
            count = 0
            total = 0
            for record in rows.split(b'\0'):
                if not record:
                    continue
                descriptor, raw_path = record.split(b'\t', 1)
                mode, kind, blob, size = descriptor.decode().split()
                path = raw_path.decode('utf-8')
                count += 1
                total += int(size) if size.isdecimal() else MAX_SOURCE_BYTES * 3
                if (kind != 'blob' or mode not in {'100644', '100755'} or count > 20000
                        or total > 256 * 1024 * 1024 or len(raw_path) > 1024
                        or any(ord(c) < 32 for c in path) or path.startswith('/')
                        or any(part in {'', '.', '..'} for part in path.split('/'))):
                    raise ValueError('CI source contains links or exceeds checkout bounds')
                tree_blobs[path] = (mode, blob)
                if Path(path).name == 'Cargo.toml':
                    manifest_parents.append(str(Path(path).parent))
                if path in REPO_BUILD_CONFIG_PATHS:
                    repo_build_config[path] = [mode, blob]
                if profile_protected_gate_path(path, config['verification_profile']):
                    protected[path] = [mode, blob]
            helper_digest = hashlib.sha256(Path(__file__).read_bytes()).hexdigest()
            if helper_digest != config['verifier_sha256']:
                raise ValueError('Installed verification policy changed')
            manifest = profile_gate_manifest(config['verification_profile'], branch_scope, protected, helper_digest)
            if manifest != request['gate_manifest_sha256']:
                raise ValueError('Gate manifest does not match the installed policy and source')
            if require_main:
                try:
                    main_ancestor_preflight(source, request, safe_env)
                except MainAncestorRequired:
                    os.write(fd, ('FORGE_MAIN_ANCESTOR_REQUIRED expected_main=' +
                        request['expected_main'] + ' candidate_commit=' + request['candidate_commit'] + '\n').encode())
                    raise
            subprocess.run(['/usr/bin/git', '-C', str(source), '-c', 'core.hooksPath=/dev/null',
                            'checkout', '--detach', request['candidate_commit']],
                           env=safe_env, check=True, stdout=subprocess.DEVNULL,
                           stderr=subprocess.DEVNULL, timeout=30)
            node_modules = source / 'src/rust/forge-ide-web/tests/browser/node_modules'
            if node_modules.exists() or node_modules.is_symlink():
                raise ValueError('Candidate includes dependency output')
            node_modules.symlink_to(config['node_modules'], target_is_directory=True)
            target_link = source / 'src/rust/target'
            if target_link.exists() or target_link.is_symlink():
                raise ValueError('Candidate includes build output')
            target_link.symlink_to(scratch / 'target', target_is_directory=True)
            generated = [node_modules, target_link]
            mapped_targets = map_cargo_targets(source, scratch, manifest_parents, account)
            generated.extend(mapped_targets)
            exclude_generated_outputs(source, generated)
            for current, directories, files in os.walk(source, followlinks=False):
                os.chmod(current, 0o755)
                for name in files:
                    path = Path(current) / name
                    if not path.is_symlink():
                        path.chmod(0o755 if path.stat().st_mode & 0o111 else 0o644)
            package_target_hashes = tuple(
                sorted(p.readlink().name for p in mapped_targets if p.is_symlink())
            )
            layout = ActiveSlotLayout(
                root=active,
                source=source,
                scratch=scratch,
                target=scratch / 'target',
                package_targets=scratch / 'package-targets',
                cargo_home=scratch / 'cargo-home',
                home=scratch / 'home',
                qa=scratch / 'qa',
                package_target_hashes=package_target_hashes,
            )
            stages.append({
                'name': 'prepare_source',
                'status': 'passed',
                'elapsed_ms': int((time.monotonic() - prep_started) * 1000),
            })

            cache_key = cache_port.resolve_key(
                project_alias=request['project_alias'],
                branch_scope=branch_scope,
                verification_profile=config['verification_profile'],
                # The key always covers the full gate, even though a branch
                # run executes only the in-memory subset: the compiled
                # artifacts are identical, so branch runs keep reusing
                # main's snapshots.
                checks=(tuple(p.argv for scope in ('branch/cache','main') for p in event_phases_for_scope(scope))
                        if config['verification_profile'] == EVENT_PROFILE else
                        SCOPED_BRANCH_CHECKS + SCOPED_MAIN_CHECKS
                        if config['verification_profile'] == SCOPED_PROFILE else CHECKS),
                toolchain_path=config['toolchain'],
                environment=environment,
                repo_build_config=repo_build_config,
                canonical_build_root=active,
            )
            restore_outcome = cache_port.restore(cache_key, layout, tree_blobs, account)
            restore_stage = {
                'name': 'cache_restore',
                'status': restore_outcome.status,
                'elapsed_ms': restore_outcome.elapsed_ms,
                'restored_bytes': restore_outcome.restored_bytes,
                'restored_files': restore_outcome.restored_files,
            }
            if restore_outcome.fallback_reason is not None:
                restore_stage['fallback_reason'] = restore_outcome.fallback_reason
            stages.append(restore_stage)

            with os.fdopen(fd, 'wb') as log:
                fd = None
                log.write((
                    f'FORGE_CACHE_RESTORE status={restore_outcome.status} '
                    f'scope={branch_scope} compat_key={cache_key.compat_key} '
                    f'elapsed_ms={restore_outcome.elapsed_ms} '
                    f'files={restore_outcome.restored_files} bytes={restore_outcome.restored_bytes}\n'
                ).encode('utf-8'))
                log.flush()
                for index, argv in enumerate(active_checks):
                    remaining = config['timeout_seconds'] - (time.time() - started)
                    if remaining <= 0:
                        failure = 'deadline_exceeded'
                        break
                    log.write(('FORGE_FIXED_CHECK_START ' + str(index) + '\n').encode())
                    log.flush()
                    span_start = log.tell()
                    check_started = time.monotonic()
                    code = run_child(argv, cwd=source, env=environment, account=account,
                                     log=log, timeout=remaining, cancelled=cancelled)
                    check_elapsed_ms = int((time.monotonic() - check_started) * 1000)
                    log.flush()
                    span_end = log.tell()
                    check_log_spans[index] = (span_start, span_end)
                    log.write((
                        f'FORGE_FIXED_CHECK_END {index} rc={code} elapsed_ms={check_elapsed_ms}\n'
                    ).encode('utf-8'))
                    log.flush()
                    checks.append({'index': index, 'argv': list(argv), 'returncode': code})
                    stages.append({
                        'name': f'check_{index}',
                        'label': active_labels[index] if index < len(active_labels) else f'check_{index}',
                        'status': 'passed' if code == 0 else 'failed',
                        'returncode': code,
                        'elapsed_ms': check_elapsed_ms,
                    })
                    if code:
                        failure = 'cancelled_or_timed_out' if code == 124 else 'checks_failed'
                        break

                if (export_portal and config['verification_profile'] in {SCOPED_PROFILE,EVENT_PROFILE}
                        and branch_scope == 'main' and failure is None
                        and len(checks) == len(active_checks)):
                    portal_assets, portal_builds = build_portal_artifacts(config, source=source,
                        environment=environment, account=account, log=log, log_path=log_path,
                        cancelled=cancelled, started=started, stages=stages)

                clean_started = time.monotonic()
                after = subprocess.check_output(['/usr/bin/git', '-C', str(source), 'status', '--porcelain', '--untracked-files=no'], env=safe_env, text=True)
                if after.strip():
                    failure = 'source_changed_during_checks'
                    clean_status = 'failed'
                else:
                    clean_status = 'passed'
                stages.append({
                    'name': 'verify_source_clean',
                    'status': clean_status,
                    'elapsed_ms': int((time.monotonic() - clean_started) * 1000),
                })

                # Quiesce all build-user processes BEFORE reading scratch outputs or publishing cache.
                kill_build_user(account)
                post_check_quiesced = True

                if portal_assets is not None and failure is None:
                    try:
                        portal_export = seal_portal_artifacts(request, request_sha256=identity,
                            layout=layout, assets=portal_assets, build_commands=portal_builds,
                            account=account, results=results, reader_group=submitter.pw_gid,
                            cancelled=cancelled)
                    except (OSError, ValueError):
                        stages.append({'name': 'portal_artifact_export', 'status': 'failed',
                                       'failure': 'artifact_validation_failed'})
                        raise
                    stages.append({'name': 'portal_artifact_export', 'status': 'sealed',
                        'file_count': portal_export['file_count'], 'total_bytes': portal_export['total_bytes']})

                # Enrich check_0 and check_1 stages with Cargo compilation & ui-qa telemetry.
                if check_log_spans:
                    raw_log_now = log_path.read_bytes()
                    ui_label='browser-journeys' if config['verification_profile']==EVENT_PROFILE else 'make ui-qa'
                    ui_qa_index = active_labels.index(ui_label) if ui_label in active_labels else None
                    for stage in stages:
                        if stage['name'].startswith('check_') and (config['verification_profile']==EVENT_PROFILE
                                or stage['name'] in {'check_0','check_1'}):
                            idx = int(stage['name'].split('_', 1)[1])
                            if idx in check_log_spans:
                                s0, s1 = check_log_spans[idx]
                                stage.update(parse_cargo_telemetry(raw_log_now[s0:s1]))
                            if ui_qa_index is not None and idx == ui_qa_index:
                                ui_qa_stages = _read_ui_qa_stages(layout.qa, expected_uid=account.pw_uid)
                                if ui_qa_stages is not None:
                                    stage['ui_qa_stages'] = ui_qa_stages

                # Publish cache only on clean passing checks of a writable branch scope.
                if not cache_port.enabled:
                    publish_outcome = CachePublishOutcome(status='disabled')
                elif failure == 'cancelled_or_timed_out':
                    publish_outcome = CachePublishOutcome(status='skipped_cancelled', reason='cancelled_or_timed_out')
                elif failure == 'source_changed_during_checks':
                    publish_outcome = CachePublishOutcome(status='skipped_dirty', reason='source_changed_during_checks')
                elif failure is not None or len(checks) != len(active_checks):
                    publish_outcome = CachePublishOutcome(status='skipped_failed', reason=failure or 'incomplete_checks')
                elif branch_scope == 'detached':
                    publish_outcome = CachePublishOutcome(status='skipped_unscoped', reason='detached_scope')
                else:
                    source_inventory = _capture_source_inventory(source, tree_blobs)
                    publish_outcome = cache_port.publish(
                        cache_key,
                        layout,
                        source_inventory,
                        account,
                        action_id=action,
                    )
                stages.append({
                    'name': 'cache_publish',
                    'status': publish_outcome.status,
                    'elapsed_ms': publish_outcome.elapsed_ms,
                    'published_bytes': publish_outcome.published_bytes,
                    'published_files': publish_outcome.published_files,
                    'evicted_entries': publish_outcome.evicted_entries,
                })
                log.write((
                    f'FORGE_CACHE_PUBLISH status={publish_outcome.status} '
                    f'scope={branch_scope} elapsed_ms={publish_outcome.elapsed_ms} '
                    f'files={publish_outcome.published_files} bytes={publish_outcome.published_bytes} '
                    f'evicted={publish_outcome.evicted_entries}\n'
                ).encode('utf-8'))
                log.flush()
        except MainAncestorRequired:
            failure = 'main_ancestor_required'
            if cache_port.enabled and publish_outcome.status == 'not_run':
                publish_outcome = CachePublishOutcome(status='skipped_failed', reason='main_ancestor_required')
        except InterruptedError:
            failure = 'cancelled_or_timed_out'
            if cache_port.enabled and publish_outcome.status == 'not_run':
                publish_outcome = CachePublishOutcome(status='skipped_cancelled', reason='interrupted')
        except (OSError, ValueError, subprocess.SubprocessError):
            failure = 'verification_unavailable'
            if cache_port.enabled and publish_outcome.status == 'not_run':
                publish_outcome = CachePublishOutcome(status='skipped_failed', reason='verification_unavailable')
        finally:
            cleanup_started = time.monotonic()
            if fd is not None:
                os.close(fd)
            if not post_check_quiesced:
                kill_build_user(account)
            if short_tmp is not None:
                cleanup_short_tmp(short_tmp)
            if active.exists() and not active.is_symlink():
                _safe_rmtree(active)
            stages.append({
                'name': 'cleanup',
                'status': 'passed',
                'elapsed_ms': int((time.monotonic() - cleanup_started) * 1000),
            })

        cache_summary = {
            'enabled': cache_port.enabled,
            'provider': cache_port.provider_name,
            'project_alias': request['project_alias'],
            'branch_scope': branch_scope,
            'compat_key': cache_key.compat_key if cache_key is not None else None,
            'restore_status': restore_outcome.status,
            'restore_fallback_reason': restore_outcome.fallback_reason,
            'restore_elapsed_ms': restore_outcome.elapsed_ms,
            'restored_bytes': restore_outcome.restored_bytes,
            'restored_files': restore_outcome.restored_files,
            'publish_status': publish_outcome.status,
            'publish_elapsed_ms': publish_outcome.elapsed_ms,
            'published_bytes': publish_outcome.published_bytes,
            'published_files': publish_outcome.published_files,
            'evicted_entries': publish_outcome.evicted_entries,
            'cache_total_bytes': publish_outcome.cache_total_bytes,
            'cache_entry_count': publish_outcome.cache_entry_count,
        }
        result = {**{key: request[key] for key in ('action_id', 'workflow_id', 'candidate_commit',
            'candidate_tree', 'expected_main', 'gate_manifest_sha256', 'verification_profile',
            'correlation_id', 'branch_scope')},
            'version': 3 if config['verification_profile'] == EVENT_PROFILE else 2 if config['verification_profile'] == SCOPED_PROFILE else 1,
            'request_sha256': identity, 'checks': checks,
            'cache': cache_summary, 'stages': stages,
            'status': 'passed' if failure is None and len(checks) == len(active_checks) else 'failed',
            'failure': failure, 'cleanup_confirmed': True,
            'log_sha256': hashlib.sha256(log_path.read_bytes()).hexdigest(),
            'elapsed_ms': int((time.time() - started) * 1000)}
        if config['verification_profile'] in {SCOPED_PROFILE,EVENT_PROFILE}:
            result['coverage'] = profile_coverage(config['verification_profile'], branch_scope)
        if config['verification_profile'] == EVENT_PROFILE:
            result.update(event_stage_evidence(branch_scope,checks,failure))
        if portal_export is not None:
            result['artifact_export'] = portal_export
        try:
            cache_port.finish_result(result_path, result, submitter.pw_gid, cancelled=cancelled)
        except (OSError, ValueError) as error:
            result['status'] = 'failed'
            result['failure'] = ('cancelled_or_timed_out' if isinstance(error, InterruptedError)
                                 else 'verification_unavailable')
            if config['verification_profile'] == EVENT_PROFILE:
                result.update(event_stage_evidence(branch_scope,checks,result['failure']))
            cache_port.rollback_result(result)
            write_result(result_path, result, submitter.pw_gid)
        return result


def main():
    if os.geteuid() != 0 or len(sys.argv) != 2 or str(UUID(sys.argv[1])) != sys.argv[1]:
        raise ValueError('Expected root-owned CI helper and canonical action UUID')
    config = json.loads(read_file(CONFIG, owner=0, maximum=65536))
    if config.get('verification_profile') not in {LEGACY_PROFILE, SCOPED_PROFILE,EVENT_PROFILE}:
        raise ValueError('Unsupported Jenkins verification profile')
    # Stage and freeze the cache factory before any disk or process effect.
    cache_factory = CargoCacheFactory(CargoCacheConfig.parse(config.get('cargo_cache')))
    cache_port = cache_factory.create()
    def interrupted(_number, _frame):
        if not cache_port.terminal_result_committed:
            raise InterruptedError('Jenkins requested verification cancellation')
    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)
    result = execute(sys.argv[1], config, cache_port=cache_port)
    print(json.dumps({
        'action_id': result['action_id'],
        'status': result['status'],
        'branch_scope': result['branch_scope'],
        'log_sha256': result['log_sha256'],
        'elapsed_ms': result['elapsed_ms'],
        'cache_restore_status': result['cache']['restore_status'],
        'cache_publish_status': result['cache']['publish_status'],
    }))
    return 0 if result['status'] == 'passed' else 1


if __name__ == '__main__':
    try:
        sys.exit(main())
    except Exception:
        print('Trusted CI supervision failed.', file=sys.stderr)
        sys.exit(1)
