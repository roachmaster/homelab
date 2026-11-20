"""Pure, bounded feedback DTO validation; no registration, I/O or privileges."""
import hashlib
import json
import re
import unicodedata
from uuid import UUID

SHA = re.compile(r'[0-9a-f]{40}')
DIGEST = re.compile(r'[0-9a-f]{64}')
IDENTIFIER = re.compile(r'[A-Za-z0-9_.-]{1,64}')
CATEGORIES = frozenset({'source','infrastructure','quota','authentication','permission','scope'})
DISPOSITIONS = frozenset({'repair','retry_infrastructure','awaiting_review','handoff','cancelled','needs_reconcile','revise_plan'})
FEEDBACK_FIELDS = frozenset({'version','feedback_id','task_id','parent_attempt_id','plan_decision_id',
    'source_revision','parent_result_sha256','reconciliation_receipt_id','workspace_digest',
    'candidate_commit','integration_base','gate_manifest_sha256','round','disposition',
    'agent_profile','summary','findings'})


def _text(value, limit, *, empty=True):
    if (not isinstance(value,str) or len(value.encode('utf-8'))>limit
            or (not empty and not value.strip())
            or any(unicodedata.category(c)=='Cc' and c not in '\n\r\t' for c in value)):
        raise ValueError('Invalid feedback text')
    return value


def _uuid(value, nullable=False):
    if nullable and value is None:
        return None
    if not isinstance(value,str) or str(UUID(value))!=value:
        raise ValueError('Invalid feedback identity')
    return value


def validate_findings(values, *, public=False):
    if not isinstance(values,list) or len(values)>32:
        raise ValueError('Invalid feedback findings')
    result=[]
    for item in values:
        required={'finding_id','category','summary'}
        optional={'check_id','path','diagnostic_code','reproduction','expected','actual','evidence_digest','status'}
        if not isinstance(item,dict) or not required<=item.keys()<=required|optional:
            raise ValueError('Invalid feedback finding fields')
        if not isinstance(item['finding_id'],str) or not IDENTIFIER.fullmatch(item['finding_id']) or not isinstance(item['category'],str) or item['category'] not in CATEGORIES:
            raise ValueError('Invalid feedback finding identity')
        value={key:item.get(key,'') for key in ('check_id','path','diagnostic_code','reproduction','expected','actual','evidence_digest')}
        value.update(finding_id=item['finding_id'],category=item['category'],summary=_text(item['summary'],2048,empty=False),status=item.get('status','open'))
        if not isinstance(value['status'],str) or value['status'] not in ({'open'} if public else {'open','verified'}):
            raise ValueError('Finding closure requires independent verification')
        for name,limit in (('check_id',128),('path',1024),('diagnostic_code',128),('reproduction',2048),('expected',2048),('actual',2048)):
            _text(value[name],limit)
        if not isinstance(value['evidence_digest'],str) or (value['evidence_digest'] and not DIGEST.fullmatch(value['evidence_digest'])):
            raise ValueError('Invalid finding evidence digest')
        result.append(value)
    if len({item['finding_id'] for item in result})!=len(result):
        raise ValueError('Duplicate finding identity')
    if len(json.dumps(result,ensure_ascii=False).encode())>65536:
        raise ValueError('Oversized feedback findings')
    return result


def validate_feedback(value):
    if not isinstance(value,dict) or set(value)-{'workspace_digest_kind'}!=FEEDBACK_FIELDS or type(value['version']) is not int or value['version']!=1:
        raise ValueError('Invalid feedback fields')
    if 'workspace_digest_kind' in value and (not isinstance(value['workspace_digest_kind'],str) or value['workspace_digest_kind'] not in {'candidate_tree','native_receipt'}):raise ValueError('Invalid workspace digest kind')
    for name in ('feedback_id','task_id','parent_attempt_id','reconciliation_receipt_id'):
        _uuid(value[name])
    _uuid(value['plan_decision_id'],True)
    for name in ('parent_result_sha256','workspace_digest','gate_manifest_sha256'):
        if not isinstance(value[name],str) or not DIGEST.fullmatch(value[name]):
            raise ValueError('Invalid feedback digest')
    for name in ('candidate_commit','integration_base'):
        if not isinstance(value[name],str) or not SHA.fullmatch(value[name]):
            raise ValueError('Invalid feedback source')
    if (type(value['source_revision']) is not int or not 1<=value['source_revision']<2**53
            or type(value['round']) is not int or not 0<=value['round']<=32
            or not isinstance(value['disposition'],str) or value['disposition'] not in DISPOSITIONS
            or not isinstance(value['agent_profile'],str) or not IDENTIFIER.fullmatch(value['agent_profile'])):
        raise ValueError('Invalid feedback authority')
    _text(value['summary'],2048)
    result={**value,'findings':validate_findings(value['findings'])}
    if len(json.dumps(result,ensure_ascii=False,allow_nan=False,separators=(',',':')).encode('utf-8'))>65536:
        raise ValueError('Oversized complete feedback artifact')
    return result


def parse_feedback_json(raw, expected_sha256):
    if not isinstance(raw,str) or len(raw.encode('utf-8'))>65536 or hashlib.sha256(raw.encode('utf-8')).hexdigest()!=expected_sha256:
        raise ValueError('Feedback artifact digest mismatch')
    def unique(pairs):
        result={}
        for key,value in pairs:
            if key in result:
                raise ValueError('Duplicate feedback field')
            result[key]=value
        return result
    return validate_feedback(json.loads(raw,object_pairs_hook=unique,
        parse_constant=lambda _value:(_ for _ in ()).throw(ValueError('Invalid feedback number'))))
