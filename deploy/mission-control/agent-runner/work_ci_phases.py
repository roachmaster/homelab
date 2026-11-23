"""Closed v3 CI phase data; the privileged verifier mirrors these literals.

These values select real fixed commands, never labels inferred from candidate
stdout. Importing the plan has no I/O, registry or process effects.
"""
from dataclasses import dataclass

EVENT_PROFILE = 'forge-sdlc-jenkins-v3'
FAILURE_KINDS = frozenset({'compilation','tests','browser','lint','formatting',
    'planning','review','merge','infrastructure','verification','feedback_delivery',
    'reviewer_correction'})
ALL_FEATURES = ('PACKAGE=', 'FEATURES=', 'ALL_FEATURES=1', 'NO_DEFAULT_FEATURES=0')
FEATURE_VARIANTS = (
    ('remote-https', 'forge-remote-https', '', True),
    ('ide-remote', 'forge-ide', 'remote', True),
    ('ide-minimal', 'forge-ide', '', True),
    ('template-minimal', 'forge-template', '', True),
    ('terminal-minimal', 'forge-terminal', '', True),
    ('local-minimal', 'forge-local', '', False),
    ('terminal-jobs', 'forge-terminal', 'jobs', True),
    ('terminal-disk', 'forge-terminal', 'disk', True),
    ('system-node', 'forge-system', 'node', False),
    ('web-minimal', 'forge-ide-web', '', True),
)


@dataclass(frozen=True)
class CiPhase:
    phase_id: str
    kind: str
    argv: tuple[str, ...]
    logical_gate: str = 'source'


def event_phases_for_scope(branch_scope: str) -> tuple[CiPhase, ...]:
    """Preserve the complete v2 feature matrix, with a test-build barrier."""
    phases = [
        CiPhase('source-preflight', 'verification', ('/usr/bin/make','ci-preflight')),
        CiPhase('script-tests', 'tests', ('/usr/bin/python3','-B','-m','unittest','discover',
            '-s','src/rust/scripts/tests','-v')),
        CiPhase('formatting', 'formatting', ('/usr/bin/make','fmt-check','PACKAGE=','CHANGED_ONLY=0')),
        CiPhase('imports', 'verification', ('/usr/bin/make','check-imports')),
        CiPhase('dependencies', 'verification', ('/usr/bin/make','check-deps')),
    ]
    for target in ('build','check','test-compile','test-release-compile'):
        phases.append(CiPhase('all-features-'+target, 'compilation', ('/usr/bin/make',target,*ALL_FEATURES)))
    for identity, package, features, _lint in FEATURE_VARIANTS:
        flags = ('PACKAGE='+package,'FEATURES='+features,'ALL_FEATURES=0','NO_DEFAULT_FEATURES=1')
        phases.append(CiPhase(identity+'-test-compile','compilation',('/usr/bin/make','test-compile',*flags)))
    phases.append(CiPhase('ui-compilation','compilation',('/usr/bin/make','ci-ui-compile')))
    for target, kind in (('lint','lint'),('test','tests'),('test-release','tests'),('doc','verification')):
        phases.append(CiPhase('all-features-'+target,kind,('/usr/bin/make',target,*ALL_FEATURES)))
    for identity, package, features, lint in FEATURE_VARIANTS:
        flags = ('PACKAGE='+package,'FEATURES='+features,'ALL_FEATURES=0','NO_DEFAULT_FEATURES=1')
        phases.append(CiPhase(identity+'-tests','tests',('/usr/bin/make','test',*flags)))
        if lint:
            phases.append(CiPhase(identity+'-lint','lint',('/usr/bin/make','lint',*flags)))
    phases.extend((
        CiPhase('ui-contracts','verification',('/usr/bin/make','ui-check')),
        CiPhase('browser-driver-tests','tests',('/usr/bin/make','ui-qa-check')),
    ))
    if branch_scope == 'main':
        phases.extend((
            CiPhase('browser-journeys','browser',('/usr/bin/make','ui-qa'),'browser'),
            CiPhase('backend-main','tests',('/usr/bin/python3','-B','deploy/mission-control/backend-suites.py','--scope','main'),'backend'),
        ))
    else:
        phases.append(CiPhase('backend-memory','tests',('/usr/bin/python3','-B',
            'deploy/mission-control/backend-suites.py','--scope','branch'),'backend'))
    return tuple(phases)


def event_stage_evidence(branch_scope: str, checks: list, failure: str | None) -> dict:
    """Bind observed exit codes to the closed plan, never to stdout markers."""
    phases = event_phases_for_scope(branch_scope)
    if not isinstance(checks,list) or len(checks)>len(phases):
        raise ValueError('Invalid v3 phase execution count')
    records=[]
    for index,check in enumerate(checks):
        phase=phases[index]
        if (not isinstance(check,dict) or set(check)!={'index','argv','returncode'}
                or type(check['index']) is not int or check['index']!=index
                or check['argv']!=list(phase.argv) or type(check['returncode']) is not int
                or not -255<=check['returncode']<=255 or (index<len(checks)-1 and check['returncode']!=0)):
            raise ValueError('Phase result does not match actual closed execution order')
        records.append(dict(index=index,phase_id=phase.phase_id,kind=phase.kind,
            argv=list(phase.argv),returncode=check['returncode']))
    failed=records[-1] if records and records[-1]['returncode'] else None
    if failure is None and (failed is not None or len(records)!=len(phases)):
        raise ValueError('Incomplete phase execution cannot claim success')
    passed=sorted({phase.kind for phase in phases
        if all(index<len(records) and records[index]['returncode']==0
            for index,item in enumerate(phases) if item.kind==phase.kind)})
    if failure=='source_changed_during_checks':
        return dict(failure_kind='verification',passed_kinds=[],phase_results=records)
    if failure in {'cancelled_or_timed_out','verification_unavailable','deadline_exceeded'}:
        kind='infrastructure'
    elif failed is not None:
        kind='infrastructure' if failed['returncode']==124 or failed['returncode']<0 else failed['kind']
    elif failure is not None:
        kind='verification'
    else:
        kind=None
    return dict(failure_kind=kind,passed_kinds=passed,phase_results=records)
