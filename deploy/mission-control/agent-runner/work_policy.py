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
    "adapter_versions": {"antigravity": "1.5.12", "codex": "0.160.0", "grok": "1.0.92"},
    "antigravity_tool_policy": "workspace-elevated-v1",
    "grok_session_policy": "fresh-attempt-session-id-v1",
    "grok_execution_policy": "dontask-trusted-instructions-managed-only-supervised-v1",
    "grok_project_policy": "plain-instructions-reject-project-executable-config-v1",
    "grok_config_sha256": "478dc64ed679f9d5d91ec0a0b051f3dbb4406a84b280594e3e00871d09b5da34",
    "grok_managed_policy_sha256": "bdd376cc07a776d50d04e906e25a15b0057e1f6d421aa9c0fdf7336592a8beaf",
    "grok_state_policy": "isolated-home-no-shared-leader-auth-only-v1",
    "grok_auth_cleanup_policy": "retained-dirfd-inode-after-proven-stop-v1",
    "grok_binary_sha256": "add4d201dbf2c0bd6077a36feb55a76939de30bae48dbb06f16b2307d38ceacb",
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
