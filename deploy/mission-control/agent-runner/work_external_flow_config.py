"""Frozen role delegation for the external database SDLC provider."""
from dataclasses import dataclass
from types import MappingProxyType
import re
from work_retry_policy import AGENT_RETRY_LIMIT

ID = re.compile(r'[A-Za-z0-9_.-]{1,64}')
POLICY_ID = 'forge-external-db-v2'


@dataclass(frozen=True)
class ExternalWorkflowPolicy:
    author_profile: str
    reviewer_profile: str
    max_plan_revisions: int = AGENT_RETRY_LIMIT
    max_code_revisions: int = AGENT_RETRY_LIMIT
    auto_merge: bool = True
    merge_method: str = 'merge'
    version: str = 'forge-external-autonomy-v1'

    def __post_init__(self):
        if (any(not isinstance(v,str) or not ID.fullmatch(v) for v in (self.author_profile,self.reviewer_profile))
                or self.author_profile==self.reviewer_profile or self.version!='forge-external-autonomy-v1'
                or type(self.auto_merge) is not bool or not self.auto_merge or self.merge_method!='merge'
                or any(type(n) is not int or not 0<=n<=AGENT_RETRY_LIMIT for n in (self.max_plan_revisions,self.max_code_revisions))):
            raise ValueError('Invalid external workflow delegation')

    def snapshot(self,catalog):
        from work_config import sha256_canonical
        from work_policy import POLICY_ID as NATIVE_ID,POLICY_DIGEST
        recipes={'author_plan':catalog.resolve(self.author_profile,'plan'),
                 'author_implement':catalog.resolve(self.author_profile,'implement'),
                 'reviewer_review':catalog.resolve(self.reviewer_profile,'review')}
        if (any(recipes[k].resource_profile!=POLICY_ID for k in ('author_plan','author_implement'))
                or recipes['reviewer_review'].resource_profile!=NATIVE_ID
                or recipes['reviewer_review'].policy_digest!=POLICY_DIGEST
                or recipes['reviewer_review'].backend!='codex'):
            raise ValueError('External workflow requires external-v2 author and independent native Codex reviewer')
        value={**vars(self),'recipes':{k:{'recipe':r.to_dict(),'digest':r.digest} for k,r in recipes.items()}}
        value['policy_digest']=sha256_canonical(value)
        return value

    def summary(self,catalog):
        value=self.snapshot(catalog)
        return {k:value[k] for k in ('policy_digest','author_profile','reviewer_profile','max_plan_revisions','max_code_revisions','auto_merge')}


def freeze_external_workflows(value,projects,profiles,modes):
    from work_config import WorkCatalog
    if not isinstance(value,(dict,MappingProxyType)):
        raise ValueError('Invalid external workflows')
    result={}
    for project,raw in value.items():
        if project not in projects or not {'plan','review','implement'}<=modes:
            raise ValueError('External workflow project and modes must be granted')
        policy=raw if isinstance(raw,ExternalWorkflowPolicy) else ExternalWorkflowPolicy(**raw) if isinstance(raw,dict) else None
        if policy is None or policy.author_profile not in profiles or policy.reviewer_profile not in profiles:
            raise ValueError('Unknown external workflow role')
        policy.snapshot(WorkCatalog(profiles))
        result[project]=policy
    return MappingProxyType(result)
