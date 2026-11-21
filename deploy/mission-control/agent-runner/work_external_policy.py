"""External database dispatch identity, independent of native CLI policy P10."""
import hashlib

POLICY_ID = "forge-external-db-v1"
POLICY_DIGEST = hashlib.sha256(POLICY_ID.encode("ascii")).hexdigest()
MODES = frozenset({"plan", "review"})
MODEL = "external-runner"
