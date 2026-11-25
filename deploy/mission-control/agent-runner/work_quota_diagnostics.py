"""Bounded provider-error classification; normal model prose is never an input.

HTTP 429 and RESOURCE_EXHAUSTED alone can describe transient throttling or
non-quota resources. Only explicit exhausted-account quota diagnostics admit a
provider wait. The digest is telemetry, never approval or retry authority.
"""
import hashlib
import re

MAX_ERROR_BYTES = 4096
MAX_RESET_SECONDS = 86400
_QUOTA_CODES = ('usage_limit_reached', 'insufficient_quota', 'quota_exhausted', 'quota_exceeded')
_ACCOUNT_QUOTA = re.compile(r'\bindividual\s+quota\s+(?:reached|exhausted)\b', re.IGNORECASE)
_RESET = re.compile(r'\bresets?\s+in\s+((?:[0-9]{1,5}\s*[hms]\s*){1,3})(?=[.\s\]]|$)', re.IGNORECASE)


def quota_diagnostic(message):
    """Return a safe, finite diagnostic from a CLI ERROR field or stderr only."""
    if not isinstance(message, str):
        return None
    raw = message.encode('utf-8', errors='replace')[:MAX_ERROR_BYTES]
    text = raw.decode('utf-8', errors='replace')
    lowered = text.lower()
    if not any(code in lowered for code in _QUOTA_CODES) and not _ACCOUNT_QUOTA.search(text):
        return None
    delay = None
    match = _RESET.search(text)
    if match:
        parts = re.findall(r'([0-9]+)\s*([hms])', match.group(1).lower())
        units = [unit for _, unit in parts]
        if len(set(units)) == len(units) and units == sorted(units, key='hms'.index):
            total = sum(int(value) * {'h': 3600, 'm': 60, 's': 1}[unit] for value, unit in parts)
            if 1 <= total <= MAX_RESET_SECONDS:
                delay = total
    return {'version': 1, 'code': 'provider_quota_exhausted',
            'error_sha256': hashlib.sha256(raw).hexdigest(), 'retry_after_seconds': delay}
