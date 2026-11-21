"""Owner-scoped, worker-owned durable delivery port. No Git or Jenkins in API."""
from contextlib import closing
from dataclasses import dataclass
from uuid import UUID
from psycopg2.extras import Json
from queue_store import Rejected

@dataclass(frozen=True)
class ExternalDeliveryLease:
    action_id: str
    workflow_id: str
    project_alias: str
    kind: str
    generation: int
    token: str
    lease_until: str
    request: dict

class ExternalDeliveryStore:
    def __init__(self,connect_factory,owner):
        if not callable(connect_factory) or not isinstance(owner,str) or not owner.strip():raise ValueError('Invalid delivery scope')
        self._connect,self.owner=connect_factory,owner

    def _call(self,name,*args):
        try:
            with closing(self._connect()) as connection:
                with connection,connection.cursor() as cursor:
                    cursor.execute("SET LOCAL lock_timeout='5s'");cursor.execute("SET LOCAL statement_timeout='10s'")
                    cursor.execute('SELECT forge_external_workflow_delivery_api.'+name+'('+','.join(['%s']*len(args))+')',args)
                    return cursor.fetchone()[0]
        except Exception as error:
            if getattr(error,'pgcode',None) in {'42501','P0009'}:raise Rejected(409,'stale_delivery_lease') from None
            raise

    def claim(self,worker_id):
        value=self._call('claim',self.owner,worker_id)
        return ExternalDeliveryLease(**value) if value is not None else None

    def renew(self,lease):
        return self._call('renew',UUID(lease.action_id),lease.generation,UUID(lease.token))

    def checkpoint(self,lease,result):
        return self._call('checkpoint',UUID(lease.action_id),lease.generation,UUID(lease.token),Json(result))

class ExternalDeliveryStoreFactory:
    def __init__(self,connect_factory,owner):self._connect,self._owner=connect_factory,owner
    def create(self):return ExternalDeliveryStore(self._connect,self._owner)
