"""External database dispatch identity, independent of native CLI policy P10."""
import hashlib

POLICY_ID = "forge-external-db-v1"
POLICY_DIGEST = hashlib.sha256(POLICY_ID.encode("ascii")).hexdigest()
MODES = frozenset({"plan", "review"})
MODEL = "external-runner"

V2_POLICY_ID = "forge-external-db-v2"
V2_POLICY_DIGEST = hashlib.sha256(V2_POLICY_ID.encode("ascii")).hexdigest()
V2_MODES = frozenset({"plan", "review", "implement"})
POLICIES = {POLICY_ID: (POLICY_DIGEST, MODES), V2_POLICY_ID: (V2_POLICY_DIGEST, V2_MODES)}
