"""Immutable execution policy shared by admission and coding workers.

Changing these contents requires a new policy ID; existing recipes never drift.
Pure values only: importing this module performs no I/O or registration.
"""

import hashlib
import json
from types import MappingProxyType

POLICY_ID = "forge-coding-v2"
_POLICY = {
    "id": POLICY_ID,
    "adapter_versions": {"antigravity": "1.5.12", "codex": "0.160.0"},
    "antigravity_tool_policy": "workspace-elevated-v1",
    "max_wall_seconds": 1800,
    "max_stream_bytes": 67108864,
    "max_result_bytes": 16777216,
    "lease_seconds": 60,
    "renew_seconds": 10,
    "fresh_session": True,
    "scope": "systemd-root-v1",
    "workspace": "isolated-git-worktree-v1",
    "billing": "included-no-fallback",
}
POLICY_DIGEST = hashlib.sha256(
    json.dumps(_POLICY, sort_keys=True, separators=(",", ":")).encode("utf-8")
).hexdigest()
POLICY = MappingProxyType({**_POLICY, "adapter_versions": MappingProxyType(_POLICY["adapter_versions"])})
