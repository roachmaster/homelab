"""Read-only, operator-selected deployment and failed-CI evidence for M38 rescue."""
import ast,copy,hashlib,json,os,stat
from contextlib import contextmanager,closing
from dataclasses import dataclass
from pathlib import Path
from uuid import UUID,uuid5
from queue_store import Rejected
from work_ci_contract import EVENT_PROFILE,CiGateContractFactory
from work_external_delivery import DeliveryConfig,read_private,jenkins_gate_manifest,native_action_ids
from work_external_review_context import canonical,strict_json
from work_publication_clients import command
from work_native_ci_postfix import NativePostfixEvidence

OLD_HELPER='80d0edca103852fca1474245516f2cc56894ebe94857bd5af6d9dd70bd92927d'
M38=frozenset(('deploy/mission-control/agent-runner/work_external_plan_override.py',
    'deploy/mission-control/sql/migrations/0038_external_plan_approve_override.sql'))


@dataclass(frozen=True)
class ExhaustedPostfixEvent:
    event_id:UUID
    source_digest:str


class NativePostfixLedger:
    """Root-selected private evidence lock, never wired into the HTTP service."""
    def __init__(self,connect):
        if not callable(connect):raise TypeError('Expected private evidence connection')
        self.connect=connect
    @contextmanager
    def acquire(self,owner,project,workflow,review,event):
        with closing(self.connect())as connection,connection,connection.cursor()as cursor:
            cursor.execute("SET LOCAL lock_timeout='4s'");cursor.execute("SET LOCAL statement_timeout='10s'")
            cursor.execute('''SELECT e.source_digest FROM forge_agent.coding_retry_events e
              JOIN forge_agent.coding_retry_review_events l ON l.event_id=e.event_id
              JOIN forge_agent.coding_retry_event_observations o ON o.event_id=e.event_id
              WHERE(e.owner_id,e.project_alias,e.scope_kind,e.scope_id,e.kind,e.phase_id,e.event_id)=
                (%s,%s,'native_workflow',%s,'infrastructure','infrastructure',%s)
              AND(e.used,e.attempt_limit,e.status)=(20,20,'exhausted')AND e.source_id=%s
              AND(l.review_id,l.reservation_key)=(%s,%s)
              AND(o.owner_id,o.project_alias,o.scope_kind,o.scope_id,o.kind,o.phase_id,o.source_id,o.source_digest,o.observation_kind)=
                (e.owner_id,e.project_alias,e.scope_kind,e.scope_id,e.kind,e.phase_id,e.source_id,e.source_digest,'failure')
              AND(SELECT count(*)FROM forge_agent.coding_retry_event_admissions a WHERE a.event_id=e.event_id)=20
              FOR SHARE OF e,l,o''',(owner,project,workflow,event,str(review),review,'review-fix.'+str(review)))
            row=cursor.fetchone()
            if not row:raise Rejected(409,'retry_exhausted')
            yield ExhaustedPostfixEvent(event,row[0])


def _root_read(path,maximum=2*1024*1024):
    path=Path(path)
    for parent in path.parents:
        info=parent.lstat()
        if not stat.S_ISDIR(info.st_mode)or info.st_uid!=0 or info.st_mode&0o022:raise ValueError('Mutable deployment scope')
    return read_private(path,owner=0,maximum=maximum)


def _helper(raw):
    tree=ast.parse(raw);config=None;functions=[];v2=None
    for node in tree.body:
        if isinstance(node,ast.Assign)and any(isinstance(t,ast.Name)and t.id=='CONFIG'for t in node.targets):
            if config is not None or not isinstance(node.value,ast.Call)or not isinstance(node.value.func,ast.Name)or node.value.func.id!='Path' or len(node.value.args)!=1:
                raise ValueError('Unexpected immutable verifier selector')
            config=Path(ast.literal_eval(node.value.args[0]));node.value.args[0]=ast.Constant('/etc/forge-ci/pipeline.json')
        if isinstance(node,ast.FunctionDef)and node.name in {'protected_gate_path','profile_protected_gate_path'}:functions.append(copy.deepcopy(node))
        if isinstance(node,ast.Assign)and any(isinstance(t,ast.Name)and t.id=='V2_PROTECTED_PATHS'for t in node.targets):
            if not isinstance(node.value,ast.Call)or len(node.value.args)!=1:raise ValueError('Unexpected verifier V2 policy')
            v2=frozenset(ast.literal_eval(node.value.args[0]))
    if config is None or not config.is_absolute()or '..'in config.parts or len(functions)!=2:raise ValueError('Incomplete verifier policy proof')
    namespace={'__builtins__':{},'set':set,'EVENT_PROFILE':EVENT_PROFILE,'SCOPED_PROFILE':'forge-sdlc-jenkins-v2','V2_PROTECTED_PATHS':v2 or frozenset()}
    exec(compile(ast.Module(body=functions,type_ignores=[]),'<trusted-verifier-policy>','exec'),namespace)
    return config,ast.dump(tree,include_attributes=False),namespace['profile_protected_gate_path']


def _gate(files,helper_sha,scope):
    plan=CiGateContractFactory(EVENT_PROFILE).create().plan(scope)
    return hashlib.sha256(canonical({'version':3,'provider':'jenkins','profile':EVENT_PROFILE,'checks':plan.checks,
        'verifier_sha256':helper_sha,'source_gate_sha256':hashlib.sha256(canonical(files)).hexdigest(),'branch_scope':scope,
        'phase_plan':[{'phase_id':p.phase_id,'kind':p.kind,'argv':list(p.argv),'logical_gate':p.logical_gate}for p in plan.phases]})).hexdigest()


class ActivePublicationSelector:
    def __init__(self,execute=command):self.execute=execute
    def _pid(self):
        raw=self.execute(['/usr/bin/systemctl','show','forge-publication-worker.service','--property=MainPID','--value'],env={'PATH':'/usr/bin:/bin'},timeout=5)
        value=raw.decode().strip()
        if not value.isdecimal()or int(value)<2:raise ValueError('Publication worker is not active')
        return int(value)
    @contextmanager
    def acquire(self,publication_path,account_uid):
        pid=self._pid();directory=Path('/proc')/str(pid)
        if directory.stat().st_uid!=account_uid:raise ValueError('Publication worker account changed')
        argv=(directory/'cmdline').read_bytes()
        if len(argv)>8192:raise ValueError('Oversized publication command')
        parts=argv.rstrip(b'\0').split(b'\0')
        if len(parts)!=3 or parts[:2]!=[b'/usr/bin/python3',b'-B']:raise ValueError('Publication executable changed')
        module=Path(parts[2].decode())
        if (len(module.parents)<5 or module.parents[4].parent!=Path('/opt/homelab-forge/releases')
                or module!=module.parents[4]/'source/deploy/mission-control/agent-runner/work_publication.py'):
            raise ValueError('Publication runtime is not immutable')
        _root_read(module)
        environ=(directory/'environ').read_bytes()
        if len(environ)>65536:raise ValueError('Oversized publication environment')
        selected=[p.split(b'=',1)[1]for p in environ.split(b'\0')if p.startswith(b'FORGE_PUBLICATION_CONFIG=')]
        if selected!=[str(publication_path).encode()]:raise ValueError('Publication config is not the active selector')
        yield module
        if self._pid()!=pid:raise ValueError('Publication selector changed during proof')


@dataclass(frozen=True)
class NativePostfixProofConfig:
    publication_path:Path
    state_root:Path
    worker_uid:int
    root_results:Path
    def __post_init__(self):
        if (any(not isinstance(p,Path)or not p.is_absolute()or '..'in p.parts for p in (self.publication_path,self.state_root,self.root_results))
                or type(self.worker_uid)is not int or self.worker_uid<0):raise ValueError('Invalid private post-fix proof scope')


class NativePostfixProof:
    def __init__(self,config,selector,execute=command,root_read=_root_read):self.config,self.selector,self.execute,self.root_read=config,selector,execute,root_read
    def acquire(self,request,task,q,workflow):
        c=self.config
        def local(path,maximum=262144):return read_private(path,owner=c.worker_uid,maximum=maximum)
        index_raw=local(c.state_root/'native-ci/reviews'/(str(q['review_id'])+'.json'));index=strict_json(index_raw)
        if set(index)!={'version','action_id'}or index['version']!='forge-native-jenkins-v3':raise ValueError('Wrong failed CI index')
        action=str(UUID(index['action_id']));directory=c.state_root/'native-ci/actions'/action
        binding_raw=local(directory/'native-binding.json');binding=strict_json(binding_raw);old=binding['request']
        expected={'owner_id':task['owner_id'],'project_alias':task['project_alias'],'task_id':str(task['id']),'attempt_id':str(task['latest_attempt_id']),
            'review_id':str(q['review_id']),'workflow_id':str(workflow['workflow_id']),'correlation_id':str(task['correlation_id'])}
        identity={**expected,'candidate_commit':old['candidate_commit'],'candidate_tree':old['candidate_tree'],'native_gate_manifest_sha256':binding['native_gate_manifest_sha256']}
        if (binding['version']!=index['version']or binding['scope']!=expected or any(old.get(k)!=v for k,v in expected.items())
                or old['action_id']!=action or action not in native_action_ids('forge-native-jenkins-v3',identity) or old['prepare_action_id']!=str(uuid5(UUID(action),'candidate-bundle'))
                or old['candidate_commit']!=q['candidate_commit']or binding['native_gate_manifest_sha256']!=q['gate_manifest_sha256']
                or old['integration_base']!=q['verification']['integration_base']or binding['verifier_sha256']!=OLD_HELPER):raise ValueError('Failed CI source binding changed')
        observation_raw=local(directory/'observation.json');observation=strict_json(observation_raw)
        root_raw=self.root_read(c.root_results/(action+'.json'),262144);root=strict_json(root_raw);digest=hashlib.sha256(root_raw).hexdigest()
        receipt=observation.get('receipt',{});failure=q['verification'].get('delivery_failure',{});build=failure.get('build',{})
        if (observation.get('status')!='failed'or receipt.get('check_receipt_sha256')!=digest or receipt.get('cleanup_confirmed')is not True
                or receipt.get('failure')!='verification_unavailable'or root.get('status')!='failed'or root.get('cleanup_confirmed')is not True
                or root.get('failure')!='verification_unavailable'or root.get('checks')!=[]
                or any(root.get(k)!=old[k]for k in ('action_id','workflow_id','candidate_commit','candidate_tree','gate_manifest_sha256','verification_profile','correlation_id'))
                or root.get('expected_main')!=old['integration_base']or root.get('branch_scope')!=old['branch_scope']
                or q['finish_receipt']['outcome'].get('retry_observation',{}).get('stage_evidence_digest')!=digest
                or failure.get('tested_candidate')!=q['candidate_commit']or failure.get('current_main')!=old['integration_base']
                or build!={k:receipt.get(k)for k in ('provider','job','action_id','queue_id','build_number')}
                or root.get('log_sha256')!=hashlib.sha256(b'').hexdigest()):raise ValueError('Failed CI cleanup or root receipt changed')
        journal=strict_json(local(directory/'jenkins.json'))
        if journal.get('post_started')is not True or journal.get('queue_id')!=receipt.get('queue_id'):raise ValueError('Actual Jenkins submission proof missing')
        with self.selector.acquire(c.publication_path,c.worker_uid)as module:
            pub_raw=self.root_read(c.publication_path);publication=strict_json(pub_raw)
            delivery_path=Path(publication['jenkins_delivery_config']);delivery_raw=read_private(delivery_path,owner=c.worker_uid);raw=strict_json(delivery_raw)
            selected=DeliveryConfig.parse(raw)
            if (publication.get('hosted_provider')!='jenkins'or publication.get('jenkins_verification_profile')!=EVENT_PROFILE
                    or publication.get('native_identity_protocol','legacy-v1')!='legacy-v1'
                    or selected.verification_profile!=EVENT_PROFILE or selected.owner!=task['owner_id']or selected.verifier.name!='verify-candidate.py'
                    or selected.verifier.parent!=Path('/opt/forge-ci')/('native-verifier-'+request.new_helper_sha256[:16])or selected.results_root!=c.root_results):raise ValueError('New publication helper selection changed')
            new_raw=self.root_read(selected.verifier);new_sha=hashlib.sha256(new_raw).hexdigest()
            if new_sha!=request.new_helper_sha256 or new_sha==OLD_HELPER:raise ValueError('Selected helper is not the deployed fix')
            launcher=self.root_read(Path('/usr/local/libexec/forge-native-ci-check'))
            if hashlib.sha256(launcher).hexdigest()!=new_sha:raise ValueError('Actual Jenkins launcher does not use the fixed helper')
            config_path,normalized,new_protects=_helper(new_raw)
            baseline=self.root_read(module.parent.parent/'jenkins/verify-candidate.py')
            if normalized!=_helper(baseline)[1]:raise ValueError('Selected helper differs from qualified runtime policy')
            root_config_raw=self.root_read(config_path);root_config=strict_json(root_config_raw)
            if root_config.get('verifier_sha256')!=new_sha or root_config.get('verification_profile')!=EVENT_PROFILE:
                raise ValueError('Actual root verifier config does not select the fixed helper')
            old_helper=Path('/opt/forge-ci')/('native-verifier-'+OLD_HELPER[:16])/'verify-candidate.py'
            old_raw=self.root_read(old_helper)
            if hashlib.sha256(old_raw).hexdigest()!=OLD_HELPER:raise ValueError('Historical helper proof changed')
            _old_config,_old_ast,old_protects=_helper(old_raw)
            repository=c.state_root/str(q['review_id'])/'repository'
            if repository.is_symlink()or not repository.is_dir():raise ValueError('Failed candidate repository is unavailable')
            git_prefix=['/usr/bin/git','-c','safe.directory='+str(repository),'-c','core.fsmonitor=false','-C',str(repository)]
            git_env={'PATH':'/usr/bin:/bin','HOME':'/nonexistent','GIT_CONFIG_NOSYSTEM':'1','GIT_CONFIG_GLOBAL':'/dev/null','GIT_NO_REPLACE_OBJECTS':'1'}
            tree=self.execute([*git_prefix,'rev-parse','--verify',q['candidate_commit']+'^{tree}'],env=git_env,timeout=20).decode().strip()
            if tree!=old['candidate_tree']:raise ValueError('Failed candidate tree changed')
            listing=self.execute([*git_prefix,'ls-tree','-r','-z','--full-tree',q['candidate_commit']],env=git_env,timeout=20)
            files={}
            for row in listing.split(b'\0'):
                if not row:continue
                descriptor,path=row.split(b'\t',1);mode,kind,blob=descriptor.decode().split();name=path.decode()
                if kind!='blob'or mode not in {'100644','100755'}or len(files)>=20000:raise ValueError('Unsupported failed candidate proof')
                files[name]=[mode,blob]
            old_files={p:v for p,v in files.items()if old_protects(p,EVENT_PROFILE)};new_files={p:v for p,v in files.items()if new_protects(p,EVENT_PROFILE)}
            if set(new_files)-set(old_files)!=M38 or set(old_files)-set(new_files):raise ValueError('Verifier drift is not the exact M38 defect')
            if (_gate(new_files,OLD_HELPER,old['branch_scope'])!=old['gate_manifest_sha256']or _gate(old_files,OLD_HELPER,old['branch_scope'])==old['gate_manifest_sha256']):raise ValueError('Historical gate mismatch was not reproduced')
            expected_new=_gate(new_files,new_sha,old['branch_scope'])
            if jenkins_gate_manifest(selected,files,old['branch_scope'])[2]!=expected_new:raise ValueError('Fixed producer and actual helper policy still differ')
            proof={'cleanup_confirmed':True,'checks_executed':0,'matched_m38_policy':True,'old_action_id':action,'old_helper_sha256':OLD_HELPER,
                'old_receipt_sha256':digest,'index_sha256':hashlib.sha256(index_raw).hexdigest(),'binding_sha256':hashlib.sha256(binding_raw).hexdigest(),
                'publication_config_sha256':hashlib.sha256(pub_raw).hexdigest(),'delivery_config_sha256':hashlib.sha256(delivery_raw).hexdigest(),
                'root_config_sha256':hashlib.sha256(root_config_raw).hexdigest(),'new_helper_sha256':new_sha,'new_gate_manifest_sha256':expected_new,
                'launcher_sha256':new_sha,'deployed_fix_revision':request.deployed_fix_revision,'active_runtime_sha256':hashlib.sha256(self.root_read(module)).hexdigest()}
        return NativePostfixEvidence(q['review_id'],q['candidate_commit'],digest,request.deployed_fix_revision,new_sha,proof)


class NativePostfixProofFactory:
    def __init__(self,config,*,selector=None,execute=command):
        if not isinstance(config,NativePostfixProofConfig):raise TypeError('Expected typed private post-fix proof scope')
        self.config,self.selector,self.execute=config,selector if selector is not None else ActivePublicationSelector(),execute
    def create(self):return NativePostfixProof(self.config,self.selector,self.execute)
