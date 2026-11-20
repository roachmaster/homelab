"""Immutable execution policy shared by admission and coding workers.

Changing these contents requires a new policy ID; existing recipes never drift.
Pure values only: importing this module performs no I/O or registration.
"""

import hashlib
import json
from types import MappingProxyType

POLICY_ID = "forge-coding-v9"
_POLICY = {
    "id": POLICY_ID,
    "adapter_versions": {"antigravity": "1.5.12", "codex": "0.160.0", "muse": "1.4.24-R9131.1"},
    "antigravity_tool_policy": "workspace-elevated-v1",
    "muse_session_policy": "fresh-per-attempt-v1",
    "muse_execution_policy": "msp-v1-single-agent-nested-and-outer-sandbox-v1",
    "muse_native_state": "isolated-home-selected-auth-only-v1",
    "muse_binary_sha256": "d752c45dce079ee29e118804b091d81ac2b1b89b67f317b4a162fa1af1206465",
    "max_wall_seconds": 1800,
    "max_stream_bytes": 67108864,
    "max_result_bytes": 16777216,
    "lease_seconds": 60,
    "renew_seconds": 10,
    "antigravity_session_policy": "fresh-unless-receipt-owned-task-resume-v1",
    "codex_session_policy": "fresh-unless-receipt-owned-author-resume-ttl-v1",
    "codex_feature_controls": "no-user-config-no-multi-agent-no-fast-mode-v1",
    "codex_sandbox": "supervised-external-v1",
    "codex_external_io": "host-network-staged-native-state-v1",
    "scope": "systemd-root-v1",
    "workspace": "isolated-git-worktree-v1",
    "billing": "included-no-fallback",
    "plan_workflow": "artifact-bound-plan-decisions-publication-v1",
    "feedback_workflow": "lease-bound-immutable-feedback-review-v1",
    "peer_review_workflow": "source-bound-readonly-peer-review-v1",
}
POLICY_DIGEST = hashlib.sha256(
    json.dumps(_POLICY, sort_keys=True, separators=(",", ":")).encode("utf-8")
).hexdigest()
POLICY = MappingProxyType({**_POLICY, "adapter_versions": MappingProxyType(_POLICY["adapter_versions"])})
