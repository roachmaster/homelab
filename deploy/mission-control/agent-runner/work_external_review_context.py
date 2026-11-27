"""Lease-bound external-source review material for real native read-only reviewers."""
from dataclasses import dataclass
from work_defect import validate_defect_context
import hashlib,json,re
from uuid import UUID
from work_feedback import validate_findings

# Both producers and independent reviewers hash these exact patch bytes.
# Abbreviated blob IDs depend on unrelated cache objects; full IDs do not.
EXTERNAL_DIFF_FLAGS = ('--full-index', '--binary', '--no-ext-diff', '--no-textconv',
                       '--no-color', '--diff-algorithm=myers', '--no-renames')

MAX_CONTEXT_BYTES=1024*1024
MAX_VERDICT_BYTES=65536
HEX40=re.compile(r'[0-9a-f]{40}')
HEX64=re.compile(r'[0-9a-f]{64}')


def canonical(value):
    return json.dumps(value,sort_keys=True,ensure_ascii=False,allow_nan=False,separators=(',',':')).encode('utf-8')


def strict_json(text):
    def pairs(items):
        value={}
        for key,item in items:
            if key in value:raise ValueError('Duplicate JSON field')
            value[key]=item
        return value
    def invalid(value):raise ValueError('Non-finite JSON value')
    return json.loads(text,object_pairs_hook=pairs,parse_constant=invalid)


def uuid(value):
    if not isinstance(value,str) or str(UUID(value))!=value:raise ValueError('Invalid external review identity')
    return value


@dataclass(frozen=True)
class ExternalReviewContext:
    content:bytes
    sha256:str
    reviewer_task_id:str
    review_id:str
    workflow_id:str
    target_kind:str


@dataclass(frozen=True)
class ExternalSourceIntegration:
    """Trusted, freshly reviewed integration of one immutable author artifact."""
    prepare_action_id:str
    source_base_revision:str
    source_candidate_commit:str
    source_candidate_tree:str
    integration_base:str
    candidate_tree:str

    def to_dict(self):
        return {'version':1,**self.__dict__}

    @classmethod
    def from_dict(cls,value):
        fields=set(cls.__dataclass_fields__)
        if not isinstance(value,dict) or set(value)!={'version'}|fields or type(value['version']) is not int or value['version']!=1:
            raise ValueError('Invalid external source integration receipt')
        uuid(value['prepare_action_id'])
        for key in fields-{'prepare_action_id'}:
            if not isinstance(value[key],str) or not HEX40.fullmatch(value[key]):
                raise ValueError('Invalid external source integration identity')
        return cls(**{key:value[key] for key in fields})


def validate_target(raw,digest):
    if not isinstance(raw,str) or not 1<=len(raw.encode())<=MAX_CONTEXT_BYTES or hashlib.sha256(raw.encode()).hexdigest()!=digest:
        raise ValueError('External review context digest mismatch')
    value=strict_json(raw)
    keys={'version','kind','workflow_id','review_id','reviewer_task_id','reviewer_profile','target_kind','source_task_id','source_assignment_id','source_sha256','source_revision','source_base_revision','review_base_revision','request_prompt','acceptance_criteria','source_content','approved_plan','candidate','required_attestations'}
    if not isinstance(value,dict) or set(value)-{'task_kind','defect','triage_instructions'}!=keys or type(value['version']) is not int or value['version'] not in (1,2) or value['kind']!='external_database_review' or value['target_kind'] not in {'plan','code'}:
        raise ValueError('Invalid external review target')
    validate_defect_context(value)
    for key in ('workflow_id','review_id','reviewer_task_id','source_task_id','source_assignment_id'):uuid(value[key])
    for key in ('source_base_revision','review_base_revision'):
        if not isinstance(value[key],str) or not HEX40.fullmatch(value[key]):raise ValueError('Invalid review revision')
    if type(value['source_revision']) is not int or value['source_revision']<1:raise ValueError('Invalid source version')
    text=value['source_content']
    if not isinstance(text,str) or '\0' in text or not 1<=len(text.encode())<=262144 or hashlib.sha256(text.encode()).hexdigest()!=value['source_sha256']:
        raise ValueError('External source content does not match its immutable artifact')
    if value['target_kind']=='code':
        plan,candidate=value['approved_plan'],value['candidate']
        if not isinstance(plan,dict) or set(plan)!={'assignment_id','sha256','content'} or not isinstance(plan['content'],str) or not 1<=len(plan['content'].encode())<=262144 or hashlib.sha256(plan['content'].encode()).hexdigest()!=plan['sha256']:
            raise ValueError('Approved external plan unavailable')
        uuid(plan['assignment_id'])
        if not isinstance(candidate,dict) or any(not HEX40.fullmatch(candidate.get(k,'')) for k in ('candidate_commit','candidate_tree','integration_base')) or candidate['candidate_commit']!=value['review_base_revision']:
            raise ValueError('External review candidate unavailable')
        if 'source_integration' in candidate:
            integration=ExternalSourceIntegration.from_dict(candidate['source_integration'])
            authored=strict_json(text)
            if (not isinstance(authored,dict) or integration.prepare_action_id!=candidate.get('prepare_action_id')
                    or integration.source_base_revision!=value['source_base_revision']
                    or integration.source_base_revision!=authored.get('base_revision')
                    or integration.source_candidate_commit!=authored.get('candidate_commit')
                    or integration.source_candidate_commit!=candidate.get('author_candidate_commit')
                    or integration.source_candidate_tree!=authored.get('candidate_tree')
                    or integration.integration_base!=candidate['integration_base']
                    or integration.candidate_tree!=candidate['candidate_tree']
                    or integration.integration_base==integration.source_base_revision):
                raise ValueError('External review integration lost its immutable author source')
        if value['version']==2:
            from work_external_identity import validate_content_identity
            if candidate.get('identity_protocol')!='content-v2':raise ValueError('Missing frozen content protocol')
            identity=validate_content_identity(candidate.get('content_identity'))
            if identity['verification']['generation']!=candidate.get('prepare_action_id'):
                raise ValueError('Content identity belongs to a different preparation generation')
        elif 'content_identity' in candidate or candidate.get('identity_protocol','legacy-v1')!='legacy-v1':raise ValueError('Legacy target cannot reinterpret content identity')
    elif value['approved_plan'] is not None or value['candidate'] is not None or value['required_attestations']:
        raise ValueError('Plan target carries code authority')
    elif value['version']!=1:raise ValueError('Content identity protocol applies only to code targets')
    attest=value['required_attestations']
    if not isinstance(attest,list) or len(attest)>256 or any(not isinstance(x,str) or not re.fullmatch(r'[A-Za-z0-9_.-]{1,64}',x) for x in attest) or len(set(attest))!=len(attest):
        raise ValueError('Invalid protected gate attestations')
    return value


def parse_verdict(text,target,digest):
    if not isinstance(text,str) or not 1<=len(text.encode())<=MAX_VERDICT_BYTES:raise ValueError('Invalid external review verdict size')
    value=strict_json(text)
    keys={'version','workflow_id','review_id','reviewer_task_id','source_assignment_id','source_sha256','context_sha256','target_kind','decision','summary','findings','attested_finding_ids'}
    if not isinstance(value,dict) or set(value)!=keys or type(value['version']) is not int or value['version']!=1:raise ValueError('Invalid external review verdict fields')
    for key in ('workflow_id','review_id','reviewer_task_id','source_assignment_id','source_sha256','target_kind'):
        if value[key]!=target[key]:raise ValueError('External reviewer answered a different target')
    if value['context_sha256']!=digest or value['decision'] not in {'approve','revise','escalate'}:raise ValueError('Invalid external verdict authority')
    if not isinstance(value['summary'],str) or not value['summary'].strip() or len(value['summary'].encode())>2048 or '\0' in value['summary']:raise ValueError('Invalid review summary')
    findings=validate_findings(value['findings'])
    if any(x.get('status','open')!='open' for x in findings):raise ValueError('Reviewer cannot claim closed findings')
    attest=value['attested_finding_ids']
    if not isinstance(attest,list) or len(attest)>256 or any(not isinstance(x,str) for x in attest) or len(set(attest))!=len(attest) or any(x not in target['required_attestations'] for x in attest):raise ValueError('Invalid review attestation')
    if value['decision']=='approve' and (findings or set(attest)!=set(target['required_attestations'])):raise ValueError('Approval omitted findings or protected gate verification')
    if value['decision']=='revise' and not findings:raise ValueError('Revision needs actionable findings')
    value['findings']=findings
    return value


class ExternalReviewContextProvider:
    """Owned worker dependency; database access stays outside the model process."""
    def __init__(self,store,git,*,artifacts=None):
        if artifacts is not None and not callable(getattr(artifacts,'acquire',None)):raise TypeError('Expected ready artifact source provider')
        self.store,self.git,self.artifacts=store,git,artifacts
    def acquire(self,lease,workspace):
        envelope=self.store.read_external_review_context(lease)
        if envelope is None:return None
        if not isinstance(envelope,dict) or set(envelope)!={'target_json','target_sha256'}:raise ValueError('Invalid external context envelope')
        target=validate_target(envelope['target_json'],envelope['target_sha256'])
        if lease['mode']!='review' or target['reviewer_task_id']!=lease['task_id'] or target['reviewer_profile']!=lease['agent_profile'] or target['review_base_revision']!=workspace.base_revision:
            raise ValueError('External context is not bound to this native reviewer')
        if target['target_kind']=='code':
            candidate=target['candidate']
            tree=self.git(workspace.path,'rev-parse','--verify','HEAD^{tree}').decode().strip()
            if tree!=candidate['candidate_tree']:
                raise ValueError('External candidate differs from independent review target')
            if target['version']==2:
                if self.artifacts is None:raise ValueError('Content identity verifier is not selected')
                identity=candidate['content_identity']
                actual=self.artifacts.acquire(workspace.path,candidate['candidate_commit'])
                base=self.artifacts.acquire(workspace.path,candidate['integration_base'])
                if actual.artifact_id!=identity['artifact_id'] or base.artifact_id!=identity['base_artifact_id']:
                    raise ValueError('External source content differs from immutable review target')
            else:
                diff=self.git(workspace.path,'diff',*EXTERNAL_DIFF_FLAGS,candidate['integration_base'],candidate['candidate_commit'],'--')
                if hashlib.sha256(diff).hexdigest()!=candidate['diff_sha256']:
                    raise ValueError('External candidate differs from independent review target')
        return ExternalReviewContext(envelope['target_json'].encode(),envelope['target_sha256'],target['reviewer_task_id'],target['review_id'],target['workflow_id'],target['target_kind'])
