"""Public stop-event attribution. Historical payloads remain immutable.

Only recorded vocabulary and bounded display text cross this boundary; native
logs, arbitrary payload keys and tool commands never do.
"""
import re
import unicodedata

from work_events import ACTOR_KINDS, CAUSES

STOP_KINDS = frozenset(CAUSES) | {'blocked_permission', 'blocked_auth', 'quota_blocked'}


def public_attribution(kind, payload):
    if kind not in STOP_KINDS or not isinstance(payload, dict):
        return None
    result = {}
    actor = payload.get('actor')
    if (isinstance(actor, dict) and set(actor) == {'kind', 'id'}
            and isinstance(actor.get('kind'), str)
            and actor['kind'] in ACTOR_KINDS
            and isinstance(actor.get('id'), str)
            and re.fullmatch(r'[A-Za-z0-9_.:@/-]{1,128}', actor['id'])):
        result['actor'] = dict(actor)
    cause = payload.get('cause')
    if isinstance(cause, str) and cause in CAUSES.get(kind, ()):
        result['cause'] = cause
        detail = payload.get('cause_detail')
        if (isinstance(detail, str) and detail.strip()
                and not any(unicodedata.category(c) == "Cc" for c in detail)):
            try:
                if len(detail.encode('utf-8')) <= 512:
                    result['detail'] = detail
            except UnicodeError:
                pass
    return result or None
