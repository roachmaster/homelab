"""Lease-bound external-source review material for real native read-only reviewers."""
from dataclasses import dataclass
import hashlib,json,re
from uuid import UUID
from work_feedback import validate_findings

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


def validate_target(raw,digest):
    if not isinstance(raw,str) or not 1<=len(raw.encode())<=MAX_CONTEXT_BYTES or hashlib.sha256(raw.encode()).hexdigest()!=digest:
        raise ValueError('External review context digest mismatch')
    value=strict_json(raw)
    keys={'version','kind','workflow_id','review_id','reviewer_task_id','reviewer_profile','target_kind','source_task_id','source_assignment_id','source_sha256','source_revision','source_base_revision','review_base_revision','request_prompt','acceptance_criteria','source_content','approved_plan','candidate','required_attestations'}
    if not isinstance(value,dict) or set(value)!=keys or type(value['version']) is not int or value['version']!=1 or value['kind']!='external_database_review' or value['target_kind'] not in {'plan','code'}:
        raise ValueError('Invalid external review target')
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
    elif value['approved_plan'] is not None or value['candidate'] is not None or value['required_attestations']:
        raise ValueError('Plan target carries code authority')
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
    def __init__(self,store,git):self.store,self.git=store,git
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
            diff=self.git(workspace.path,'diff','--binary','--no-ext-diff','--no-textconv','--no-color','--diff-algorithm=myers','--no-renames',candidate['integration_base'],candidate['candidate_commit'],'--')
            if tree!=candidate['candidate_tree'] or hashlib.sha256(diff).hexdigest()!=candidate['diff_sha256']:
                raise ValueError('External candidate differs from independent review target')
        return ExternalReviewContext(envelope['target_json'].encode(),envelope['target_sha256'],target['reviewer_task_id'],target['review_id'],target['workflow_id'],target['target_kind'])
