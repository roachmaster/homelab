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
_SUMMARY=re.compile(r'Provider quota exhausted; waiting for allowance\. \[forge-provider-quota-v1 reset=(none|[1-9][0-9]{0,4}) error=([0-9a-f]{64})\]')


def quota_summary(diagnostic):
    if (not isinstance(diagnostic,dict) or set(diagnostic)!={'version','code','error_sha256','retry_after_seconds'}
        or type(diagnostic['version'])is not int or diagnostic['version']!=1 or diagnostic['code']!='provider_quota_exhausted'
        or not isinstance(diagnostic['error_sha256'],str) or not re.fullmatch('[0-9a-f]{64}',diagnostic['error_sha256'])
        or diagnostic['retry_after_seconds'] is not None and(type(diagnostic['retry_after_seconds'])is not int
            or not 1<=diagnostic['retry_after_seconds']<=MAX_RESET_SECONDS)):
        raise ValueError('Invalid safe quota diagnostic')
    reset='none' if diagnostic['retry_after_seconds']is None else str(diagnostic['retry_after_seconds'])
    return f"Provider quota exhausted; waiting for allowance. [forge-provider-quota-v1 reset={reset} error={diagnostic['error_sha256']}]"


def parse_quota_summary(summary):
    match=_SUMMARY.fullmatch(summary) if isinstance(summary,str) else None
    if not match:return None
    reset=None if match[1]=='none' else int(match[1])
    diagnostic=dict(version=1,code='provider_quota_exhausted',error_sha256=match[2],retry_after_seconds=reset)
    try:quota_summary(diagnostic)
    except ValueError:return None
    return diagnostic


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
