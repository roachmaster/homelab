"""Owner-scoped, factory-selected access to durable per-event retry budgets."""
from dataclasses import dataclass
from uuid import UUID
import re
from work_retry_policy import AGENT_RETRY_LIMIT

RETRY_EVENT_KINDS=frozenset({'compilation','tests','browser','lint','formatting','planning','review','merge',
    'infrastructure','verification','feedback_delivery','reviewer_correction'})
RETRY_SCOPE_KINDS=frozenset({'native_workflow','external_workflow','delivery','manual_task'})
_ID=re.compile(r'[A-Za-z0-9_.-]{1,64}')
_DIGEST=re.compile(r'[0-9a-f]{64}')

@dataclass(frozen=True)
class RetryScope:
    owner_id:str
    project_alias:str
    scope_kind:str
    scope_id:UUID
    def __post_init__(self):
        if (any(not isinstance(v,str) or not _ID.fullmatch(v) for v in (self.owner_id,self.project_alias))
            or self.scope_kind not in RETRY_SCOPE_KINDS or not isinstance(self.scope_id,UUID)):
            raise ValueError('Invalid retry event scope')

class RetryEventProvider:
    def __init__(self,owner_id):
        if not isinstance(owner_id,str) or not _ID.fullmatch(owner_id):raise ValueError('Invalid retry event owner')
        self.owner_id=owner_id
    def _scope(self,scope):
        if not isinstance(scope,RetryScope) or scope.owner_id!=self.owner_id:raise ValueError('Retry event owner mismatch')
        return (scope.project_alias,scope.scope_kind,scope.scope_id)
    @staticmethod
    def _proof(kind,source_id,digest):
        if kind not in RETRY_EVENT_KINDS or not isinstance(source_id,str) or not 1<=len(source_id.encode())<=256 or any(ord(c)<32 for c in source_id) or not isinstance(digest,str) or not _DIGEST.fullmatch(digest):
            raise ValueError('Invalid trusted retry event observation')
    @staticmethod
    def _result(cursor):
        row=cursor.fetchone()
        return row['event'] if isinstance(row,dict) else row[0]
    def observe_failure(self,cursor,scope,kind,source_id,digest,passed_kinds=(),phase_id='',passed_phase_ids=()):
        self._proof(kind,source_id,digest)
        passed=list(passed_kinds)
        if len(set(passed))!=len(passed) or kind in passed or any(k not in RETRY_EVENT_KINDS for k in passed):raise ValueError('Invalid trusted passed stages')
        cursor.execute('SELECT forge_agent.retry_event_observe(%s,%s,%s,%s,%s,%s,%s,%s,%s) AS event',
            (*self._scope(scope),kind,source_id,digest,passed,phase_id,list(passed_phase_ids)))
        return self._result(cursor)
    def reserve(self,cursor,event_id,action_key):
        if not isinstance(event_id,UUID):event_id=UUID(str(event_id))
        if not isinstance(action_key,str) or not 1<=len(action_key.encode())<=256 or any(ord(c)<32 for c in action_key):raise ValueError('Invalid retry admission key')
        cursor.execute('SELECT forge_agent.retry_event_reserve(%s,%s) AS event',(event_id,action_key))
        return self._result(cursor)
    def resolve_verified(self,cursor,scope,kind,source_id,digest,phase_id=None):
        self._proof(kind,source_id,digest)
        cursor.execute('SELECT forge_agent.retry_event_resolve(%s,%s,%s,%s,%s,%s,%s) AS event',
            (*self._scope(scope),kind,source_id,digest,phase_id))
        return self._result(cursor)
    def adopt_legacy(self,cursor,event_id,used,source_id,digest):
        if type(used) is not int or not 0<=used<=AGENT_RETRY_LIMIT:raise ValueError('Invalid retained event count')
        self._proof('planning',source_id,digest)
        cursor.execute('SELECT forge_agent.retry_event_adopt_legacy(%s,%s,%s,%s) AS event',(UUID(str(event_id)),used,source_id,digest))
        return self._result(cursor)
    def bind_review(self,cursor,review_id,event_id,action_key):
        cursor.execute('SELECT forge_agent.retry_event_bind_review(%s,%s,%s)',(UUID(str(review_id)),UUID(str(event_id)),action_key))
    def get(self,cursor,event_id):
        cursor.execute('SELECT forge_agent.retry_event_get(%s) AS event',(UUID(str(event_id)),))
        return self._result(cursor)
    def project(self,cursor,scope):
        args=self._scope(scope)
        cursor.execute("SELECT to_regprocedure('forge_agent.retry_event_project(text,text,uuid)') IS NOT NULL AS available")
        row=cursor.fetchone();available=row['available'] if isinstance(row,dict) else row[0]
        if not available:return None
        cursor.execute('SELECT forge_agent.retry_event_project(%s,%s,%s) AS event',args)
        return self._result(cursor)

class RetryEventProviderFactory:
    def __init__(self,owner_id):self.owner_id=owner_id
    def create(self):return RetryEventProvider(self.owner_id)
