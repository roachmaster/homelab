"""Selected ACP-v1 transport and deny-by-default native permission broker.

No subprocesses, credentials, session discovery or network effects live here.
The existing supervisor owns the process, OS sandbox and credential lifetime.
"""
from dataclasses import dataclass
import json
from pathlib import Path, PurePosixPath
import re
import shlex
import stat
from uuid import UUID
from work_cli_transport import parse_record
from work_quota_diagnostics import quota_diagnostic

WORKSPACE='/run/forge-coding-workspace'
STATE='/run/forge-coding-attempt'
SELECTION='acp-constrained-v1'
MAX_FRAME=1024*1024
MAX_TOTAL=32*1024*1024
# Bare named ask rules are documented to request approval even for normally
# automatic local reads and built-in read-only shell handling. No allow rules.
CONFIG_SUFFIX='\n[permission]\nask = ["Bash", "Read", "Edit", "Grep", "Glob", "run_terminal_command", "read_file", "list_dir", "grep", "search_replace", "write"]\ndeny = ["MCPTool(*)"]\n'

DIRECTION=('Use native read_file/grep/list_dir in the workspace, closed read-only git commands, '
           'or the exact report_progress.py helper. Implementation alone permits workspace edits and bounded build/test commands. '
           'Use plain report text. Do not use shell substitutions, glob/brace/tilde expansion, '
           'redirections, publishing, network or external tools.')


def wire(value):
    return json.dumps(value,ensure_ascii=False,separators=(',',':')).encode('utf-8')+b'\n'


def terminal_usage(value):
    """ACP inputTokens includes cache; emit the existing disjoint end buckets.

    Installed guide14 documents this difference from headless output. Missing
    buckets stay unknown; never subtract inferred zeros or sum step counters.
    """
    if value is None:return None
    from work_grok import GrokEvents,MAX_COUNTER
    if not isinstance(value,dict):raise ValueError('Invalid ACP terminal usage')
    fields=('inputTokens','outputTokens','cachedReadTokens','cacheCreationTokens','totalTokens')
    if any(value.get(key)is not None and(type(value[key])is not int or not 0<=value[key]<=MAX_COUNTER)for key in fields):
        raise ValueError('Invalid ACP terminal counter')
    full,cached,created=(value.get(key)for key in ('inputTokens','cachedReadTokens','cacheCreationTokens'))
    uncached=None
    if all(v is not None for v in (full,cached,created)):
        uncached=full-cached-created
        if uncached<0:raise ValueError('Invalid ACP cached input')
    output=value.get('outputTokens');total=value.get('totalTokens')
    if all(v is not None for v in (full,output,total)) and full+output!=total:
        raise ValueError('Inconsistent ACP terminal accounting')
    return GrokEvents._usage({'input_tokens':uncached,'output_tokens':output,
        'cache_read_input_tokens':cached,'cache_creation_input_tokens':created,'total_tokens':total})


def argv(binary,model,*,implement):
    # Ask every permission through ACP; no Bash/Read/Edit allow rules, yolo,
    # dontAsk, leader, managed MCP or client terminal/filesystem delegation.
    tools='read_file,grep,list_dir,run_terminal_cmd'
    if implement:tools+=',search_replace,write'
    return (str(binary),'--no-auto-update','--trust','--cwd',WORKSPACE,
            '--no-memory','--no-subagents','--no-plan','--disable-web-search',
            '--permission-mode','default','--tools',tools,'--deny','MCPTool(*)',
            '--sandbox','workspace' if implement else 'read-only',
            'agent','--model',model,'--no-leader','stdio')


def git_config_paths(workspace):
    # WorkspaceProvider has already proved this acquired isolated worktree's
    # absolute Git directory belongs to the approved common repository.
    # Derive from that proof for both bare feature and legacy .git repositories.
    directory=workspace.git_directory
    if directory.parent.name!='worktrees':raise ValueError('ACP requires an acquired isolated worktree')
    return (directory.parent.parent/'config',directory/'config.worktree',Path('/etc/gitconfig'))


def harmless_git_config(content):
    """Closed Git config subset, not a growing list of executable-key guesses.

    Ordinary repository identity is allowed. Includes, filters, aliases,
    signature programs, attribute paths, pagers and unknown syntax fail shut.
    Inline section+key is valid Git syntax and is parsed here explicitly.
    """
    if '\\' in content or any(ord(c)<32 and c not in '\n\r\t' for c in content):return False
    section=None
    for line in content.splitlines():
        line=line.strip()
        if not line or line.startswith(('#',';')):continue
        if line.startswith('['):
            match=re.match(r'\[\s*(core|extensions|remote|branch)(?:\s+"([A-Za-z0-9_./-]+)")?\s*\]',line,re.I)
            if match is None:return False
            section=match[1].lower()
            if (section in {'remote','branch'}) != (match[2] is not None):return False
            line=line[match.end():].strip()
            if not line:continue
        assignment=re.fullmatch(r'([A-Za-z][A-Za-z0-9-]*)\s*=\s*(.*?)\s*',line)
        if section is None or assignment is None:return False
        key,value=assignment[1].lower(),assignment[2]
        if section=='core':
            if key=='repositoryformatversion' and value in {'0','1'}:continue
            if key in {'filemode','bare','logallrefupdates','ignorecase','precomposeunicode','symlinks'} and value.lower() in {'true','false','0','1'}:continue
            if key=='fsmonitor' and value.lower() in {'false','0'}:continue
            if key=='hookspath' and value=='/dev/null':continue
        elif section=='extensions':
            if key=='worktreeconfig' and value.lower() in {'true','false','0','1'}:continue
            if key=='objectformat' and value=='sha1':continue
        elif section=='remote':
            if key=='url' and re.fullmatch(r'[A-Za-z0-9@:/._~+-]{1,4096}',value):continue
            if key=='fetch' and re.fullmatch(r'\+?refs/(?:heads/)?\*:refs/(?:remotes/[A-Za-z0-9_.-]+/)?\*',value):continue
            if key=='mirror' and value.lower() in {'true','false','0','1'}:continue
        elif section=='branch':
            if key=='remote' and re.fullmatch(r'[A-Za-z0-9_.-]{1,128}',value):continue
            if key=='merge' and re.fullmatch(r'refs/heads/[A-Za-z0-9_./-]{1,128}',value):continue
        return False
    return True


def same_tool_input(previous,current):
    if 'rawInput' in current and current['rawInput']!=previous.get('rawInput'):return False
    meta=current.get('_meta',{})
    original_meta=previous.get('_meta',{})
    if not isinstance(meta,dict) or not isinstance(original_meta,dict):return False
    # Progress adds non-authoritative metadata such as toolNotRun. Only native
    # tool identity is frozen; status/diagnostic metadata is not that identity.
    return ('x.ai/tool' not in meta or meta['x.ai/tool']==original_meta.get('x.ai/tool'))


@dataclass(frozen=True)
class NativePaths:
    """Explicit parent-side read-only path proof; never expose the auth root.

    Reject symlinks in every component (including broken links). The OS sandbox
    remains the authority against races and native tools that violate requests.
    """
    workspace: Path
    notes: tuple[tuple[str,Path],...]=()
    git_configs: tuple[Path,...]=()

    @staticmethod
    def directory_chain(path):
        if not path.is_absolute():return False
        try:
            return all(stat.S_ISDIR(parent.lstat().st_mode) for parent in (path,*path.parents))
        except OSError:return False

    def git_allows(self):
        # Reads of built-ins can execute configured fsmonitor/diff helpers.
        # No Git subprocess is launched to prove a command safe.
        for path in self.git_configs:
            try:
                if not self.directory_chain(path.parent):return False
                info=path.lstat()
                if not stat.S_ISREG(info.st_mode) or info.st_size>65536:return False
                content=path.read_text(encoding='utf-8')
            except FileNotFoundError:continue
            except (OSError,UnicodeError):return False
            if not harmless_git_config(content):return False
        return True

    def allows(self,value,*,write=False):
        if not isinstance(value,str) or not value or len(value)>4096 or '\x00' in value:return False
        path=PurePosixPath(value)
        if '..' in path.parts:return False
        if not path.is_absolute():path=PurePosixPath(WORKSPACE)/path
        base=PurePosixPath(WORKSPACE)
        if path==base or base in path.parents:
            relative=path.relative_to(base)
            if any(p in {'.git','.grok','.claude','.cursor','.envrc'} for p in relative.parts):return False
            root=self.workspace
            parts=relative.parts
        else:
            if write:return False
            match=next((host for child,host in self.notes if str(path)==child),None)
            if match is None:return False
            root=match.parent;parts=(match.name,)
        try:
            if not self.directory_chain(root):return False
            current=root
            for index,part in enumerate(parts):
                current=current/part
                try:mode=current.lstat().st_mode
                except FileNotFoundError:return write
                if stat.S_ISLNK(mode) or not (stat.S_ISDIR(mode) or stat.S_ISREG(mode)):return False
                if index<len(parts)-1 and not stat.S_ISDIR(mode):return False
            return True
        except OSError:return False


class Permissions:
    def __init__(self,mode,paths,*,progress=False):
        if not isinstance(mode,str) or mode not in {'plan','review','implement'} or not callable(getattr(paths,'allows',None)):
            raise ValueError('Invalid Grok capability composition')
        self.mode,self.paths,self.progress=mode,paths,progress

    def terminal(self,command,cwd=WORKSPACE):
        if cwd!=WORKSPACE or not isinstance(command,str) or not 0<len(command)<=16384:return False
        # shlex is not a shell parser. Refuse all shell syntax outside the one
        # explicitly supported && sequence, even when quoted. No interpolation.
        command=command.replace('\\"','"')
        # Reject expansion syntax even inside quotes rather than pretending
        # shlex proves what a shell will execute. Wildcards can become explicit
        # symlink operands which were absent from the admitted argument list.
        if any(c in command for c in '$`\n\r;|<>\\\x00*?[]{}~()') or re.search(r'(?<!&)&(?!&)',command):return False
        try:segments=[shlex.split(p,posix=True) for p in command.split('&&')]
        except ValueError:return False
        return 0<len(segments)<=16 and all(self._command(args) for args in segments)

    def _command(self,args):
        if not args:return False
        if args[0]=='echo':
            return len(args)==2 and re.fullmatch(r'---[A-Z_]{1,24}---',args[1]) is not None
        if args[:2]==['python3',STATE+'/report_progress.py']:
            return (self.progress and len(args)==4 and args[2] in {'plan','progress'}
                    and 0<len(args[3].encode('utf-8'))<=4096)
        if args[0]=='git':
            if not self.paths.git_allows():return False
            tail=args[1:]
            if tail in (['status'],['status','--short'],['status','--short','--branch'],
                        ['branch','--show-current'],['rev-parse','HEAD'],['worktree','list'],['ls-files']):return True
            if tail and tail[0]=='log':
                return len(tail)<=5 and all(a in {'--oneline','--no-decorate','HEAD'} or re.fullmatch(r'-[1-9][0-9]?',a) for a in tail[1:])
            if tail[:3]==['grep','-n','--'] or tail[:4]==['grep','-n','-F','--']:
                prefix=4 if tail[:4]==['grep','-n','-F','--'] else 3
                return len(tail)==prefix+1 and 0<len(tail[-1])<=2048
            if tail[:2]==['show','--stat']:return tail[2:]==['HEAD']
            if tail and tail[0]=='diff':
                return tail[1:] in ([],['--stat'],['--name-only'],['--check'])
            return False
        if args[0]=='rg':
            if args[1:]==['--files']:return True
            prefix=4 if args[1:4]==['-n','-F','--'] else 3 if args[1:3]==['-n','--'] else 0
            return (prefix>0 and len(args) in {prefix+1,prefix+2} and 0<len(args[prefix])<=2048
                    and (len(args)==prefix+1 or self.paths.allows(args[-1])))
        if self.mode!='implement':return False
        if args[0]=='make':return args[1:] in (['ci-preflight'],['ci'],['ui-qa'])
        if args[:4]==['python3','-B','-m','unittest']:
            if args[4:8]==['discover','-s','deploy/mission-control/agent-runner','-p']:
                return (len(args) in {9,10} and re.fullmatch(r'test_[a-z0-9_]+\.py',args[8]) is not None
                        and (len(args)==9 or args[9] in {'-q','-v'}))
            return 4<len(args)<=20 and all(a in {'-q','-v'} or re.fullmatch(r'test_[a-z0-9_]+(?:\.[A-Za-z0-9_]+){0,2}',a) for a in args[4:])
        if args[:2]==['python3','-B'] and args[2:]==['deploy/mission-control/backend-suites.py','--scope','branch']:return True
        # Explicit compiler/test commands only, no arbitrary scripts, shell,
        # package installation, network/publishing or user-provided executables.
        if args[0]=='cargo' and len(args)>=2 and args[1] in {'check','build','test','clippy','fmt'}:
            return len(args)<=16 and all(a in {'--workspace','--all-targets','--all-features','--locked','--offline','--release','--check','--all','--','--nocapture','--exact'} or re.fullmatch(r'[A-Za-z][A-Za-z0-9_]{0,127}',a) for a in args[2:])
        return False

    def admit(self,tool):
        if not isinstance(tool,dict):return False
        meta=tool.get('_meta',{})
        native=meta.get('x.ai/tool',{}) if isinstance(meta,dict) else {}
        name=(native.get('name') if isinstance(native,dict) and native.get('namespace')=='grok_build' and type(native.get('version'))is int and native['version']==1 else None)
        if not isinstance(name,str):return False
        raw=tool.get('rawInput')
        if not isinstance(raw,dict):return False
        if name=='run_terminal_command':
            return (set(raw)<={'command','cwd','timeout_ms','max_output_chars','description'} and
                    self.terminal(raw.get('command'),raw.get('cwd',WORKSPACE)))
        if name in {'read_file','list_dir','grep','write','search_replace'}:
            write=name in {'write','search_replace'}
            if write and self.mode!='implement':return False
            fields={'read_file':{'target_file','offset','limit'},
                    'list_dir':{'relative_workspace_path','path'},
                    'grep':{'pattern','path','include_pattern','exclude_pattern','case_sensitive'},
                    'write':{'file_path','target_file','path','contents','content'},
                    'search_replace':{'file_path','target_file','path','old_string','new_string','replace_all'}}[name]
            if not set(raw)<=fields:return False
            pathfields=[key for key in ('target_file','relative_workspace_path','file_path','path') if key in raw]
            if len(pathfields)!=1:return False
            path=raw[pathfields[0]]
            for key in ('include_pattern','exclude_pattern'):
                pattern=raw.get(key,'')
                if not isinstance(pattern,str) or pattern.startswith('/') or '..' in PurePosixPath(pattern).parts:return False
            return self.paths.allows(path,write=write)
        return False


class Transport:
    """One fresh ACP session, serial host requests, bounded JSONL frames.

    Only initialize/new/prompt and permission replies can leave this adapter.
    No authenticate, extension RPC, MCP, file or terminal service is offered.
    """
    def __init__(self,prompt,permissions):
        if not isinstance(prompt,str) or len(prompt.encode('utf-8'))>MAX_FRAME//2:raise ValueError('Invalid ACP prompt bound')
        if not isinstance(permissions,Permissions):raise TypeError('ACP requires ready permission broker')
        self.prompt=prompt+'\n\nCONTROLLER PERMISSION BOUNDARY: '+DIRECTION
        self.permissions=permissions
        self.buffer=bytearray();self.total=0;self.complete=False;self.session=None
        self.phase='initialize';self.seen=set();self.calls={};self.usage=None;self.cancellation=None
        self.early=[];self.terminal_stop=None
        self.quota_diagnostic=None
        self.outbound=bytearray(wire({'jsonrpc':'2.0','id':'host-initialize','method':'initialize','params':{
            'protocolVersion':1,'clientCapabilities':{'fs':{'readTextFile':False,'writeTextFile':False},'terminal':False},
            'clientInfo':{'name':'homelab-constrained-grok','version':'1'}}}))

    def drain(self):
        data=bytes(self.outbound);self.outbound.clear();return data

    def _send(self,value):
        data=wire(value)
        if len(self.outbound)+len(data)>MAX_FRAME:raise ValueError('ACP outbound bound exceeded')
        self.outbound.extend(data)

    def feed(self,chunk):
        self.total+=len(chunk)
        if self.total>MAX_TOTAL:raise ValueError('ACP output bound exceeded')
        self.buffer.extend(chunk);events=[]
        while b'\n' in self.buffer:
            line,_,rest=self.buffer.partition(b'\n');self.buffer=bytearray(rest)
            if not line or len(line)>MAX_FRAME:raise ValueError('Invalid ACP frame')
            events.extend(self._record(parse_record(bytes(line),'grok')))
        if len(self.buffer)>MAX_FRAME:raise ValueError('ACP frame bound exceeded')
        return b''.join(wire(event) for event in events)

    def eof(self):
        if self.buffer or not self.complete:raise ValueError('Incomplete ACP turn')

    def _record(self,value):
        if value.get('jsonrpc')!='2.0' or self.complete:raise ValueError('Invalid ACP envelope')
        method=value.get('method')
        if method is not None:
            if not isinstance(method,str) or 'result' in value or 'error' in value:raise ValueError('Invalid ACP request')
            params=value.get('params')
            if not isinstance(params,dict):raise ValueError('Invalid ACP parameters')
            if self.session is None and self.phase=='new' and 'id' not in value and method=='session/update':
                if len(self.early)>=8:raise ValueError('ACP early notification bound exceeded')
                self.early.append(value);return []
            if self.session is None or params.get('sessionId')!=self.session:
                raise ValueError('ACP session changed')
            if 'id' in value:
                identity=value['id']
                if (type(identity)not in (str,int) or isinstance(identity,str) and not re.fullmatch(r'[A-Za-z0-9_.:-]{1,128}',identity)
                        or isinstance(identity,str) and identity.startswith('host-')
                        or identity in self.seen or len(self.seen)>=4096):raise ValueError('Invalid ACP request identity')
                self.seen.add(identity)
                if method!='session/request_permission':
                    self._send({'jsonrpc':'2.0','id':identity,'error':{'code':-32601,'message':'Client capability unavailable'}})
                    return []
                if self.phase!='prompt':raise ValueError('ACP permission outside turn')
                tool=params.get('toolCall');options=params.get('options')
                if not isinstance(tool,dict) or not isinstance(options,list) or not 1<=len(options)<=8:
                    raise ValueError('Invalid ACP permission')
                call=tool.get('toolCallId')
                if not isinstance(call,str) or call not in self.calls:raise ValueError('Unbound ACP tool request')
                # Native tool identity/input must agree with its preceding call.
                previous=self.calls[call]
                if not same_tool_input(previous,tool):raise ValueError('ACP permission identity changed')
                admitted=self.permissions.admit(previous)
                wanted='allow_once' if admitted else 'reject_once'
                ids=[o.get('optionId') for o in options if isinstance(o,dict)]
                selected=[o for o in options if isinstance(o,dict) and o.get('kind')==wanted]
                if (len(ids)!=len(options) or any(not isinstance(i,str) or not re.fullmatch(r'[A-Za-z0-9_.:-]{1,128}',i) for i in ids)
                        or len(set(ids))!=len(ids) or len(selected)!=1):raise ValueError('Missing one-call ACP permission option')
                self._send({'jsonrpc':'2.0','id':identity,'result':{'outcome':{'outcome':'selected','optionId':selected[0]['optionId']},
                    '_meta':{'homelab/permission': 'admitted' if admitted else DIRECTION}}})
                return []
            if method=='session/update':return self._update(params.get('update'))
            # Ignore no arbitrary extensions: recognized final metadata alone may
            # provide terminal counters; per-response spend is never summed.
            if method=='_x.ai/session/update':
                update=params.get('update')
                if (not isinstance(update,dict) or update.get('sessionUpdate')!='turn_completed'
                        or not isinstance(update.get('stop_reason'),str)
                        or update['stop_reason'] not in {'end_turn','cancelled','max_tokens','max_turn_requests','refusal'}):
                    raise ValueError('Invalid ACP terminal metadata')
                if self.terminal_stop is not None:raise ValueError('Duplicate ACP terminal metadata')
                self.terminal_stop=update['stop_reason']
                meta=params.get('_meta',{})
                if (update['stop_reason']=='cancelled' and isinstance(meta,dict)
                        and meta.get('cancellationCategory')=='PermissionCancelled'):
                    self.cancellation='permission_cancelled'
                # Notification usage can be partial. Only the matching final
                # prompt result's cumulative _meta.usage grants spend authority.
                return []
            raise ValueError('Unsupported ACP notification')
        if set(value)-{'jsonrpc','id','result','error'} or value.get('id')!='host-'+self.phase:
            raise ValueError('ACP host response identity changed')
        if 'error' in value:
            error=value['error']
            if ('result' in value or not isinstance(error,dict) or type(error.get('code'))is not int
                    or not isinstance(error.get('message'),str) or len(error['message'].encode('utf-8'))>4096):
                raise ValueError('Invalid ACP error response')
            message=error['message'];lowered=message.lower()
            self.quota_diagnostic=quota_diagnostic(message)
            if self.quota_diagnostic is not None:canonical='usage_limit_reached'
            elif any(key in lowered for key in ('unauthorized','authentication_required','authentication required','invalid_grant','not logged in')):canonical='authentication_required'
            elif any(key in lowered for key in ('permission_denied','permission denied','approval required')):canonical='permission_denied'
            elif error['code']==-32800:canonical='cancelled'
            else:canonical='Grok ACP request failed.'
            self.complete=True;self.prompt=''
            # Fixed public classification only, never native error prose/data.
            event={'type':'error','message':canonical}
            if self.session is not None:
                event.update(sessionId=self.session,cwd=WORKSPACE,requestId='host-'+self.phase)
            return [event]
        result=value.get('result')
        if not isinstance(result,dict):raise ValueError('Invalid ACP response')
        if self.phase=='initialize':
            if type(result.get('protocolVersion'))is not int or result['protocolVersion']!=1:raise ValueError('Unsupported ACP version')
            meta=result.get('_meta',{})
            if (not isinstance(meta,dict) or meta.get('currentWorkingDirectory',WORKSPACE)!=WORKSPACE
                    or meta.get('agentVersion','1.0.92')!='1.0.92'):
                raise ValueError('ACP native capability identity changed')
            self.phase='new'
            self._send({'jsonrpc':'2.0','id':'host-new','method':'session/new','params':{'cwd':WORKSPACE,'mcpServers':[]}})
        elif self.phase=='new':
            session=result.get('sessionId')
            if not isinstance(session,str) or str(UUID(session))!=session:raise ValueError('Invalid ACP session')
            self.session=session;self.phase='prompt'
            self._send({'jsonrpc':'2.0','id':'host-prompt','method':'session/prompt','params':{
                'sessionId':session,'prompt':[{'type':'text','text':self.prompt}]}})
            self.prompt=''
            early=self.early;self.early=[]
            return [event for notification in early for event in self._record(notification)]
        elif self.phase=='prompt':
            stop=result.get('stopReason')
            if not isinstance(stop,str) or stop not in {'end_turn','cancelled','max_tokens','max_turn_requests','refusal'}:raise ValueError('Invalid ACP stop reason')
            if self.terminal_stop is not None and self.terminal_stop!=stop:raise ValueError('ACP stop identity changed')
            meta=result.get('_meta',{})
            if not isinstance(meta,dict):raise ValueError('Invalid ACP result metadata')
            self.usage=terminal_usage(meta.get('usage'))
            self.complete=True
            return [{'cancellationCategory':self.cancellation,'type':'end','sessionId':self.session,'cwd':WORKSPACE,'requestId':'host-prompt',
                     'stopReason':stop,'usage':self.usage}]
        else:raise ValueError('Invalid ACP phase')
        return []

    def _update(self,update):
        if self.phase!='prompt' or not isinstance(update,dict):raise ValueError('Invalid ACP update')
        kind=update.get('sessionUpdate')
        if not isinstance(kind,str):raise ValueError('Invalid ACP update kind')
        if kind=='agent_message_chunk':
            content=update.get('content')
            if not isinstance(content,dict) or content.get('type')!='text' or not isinstance(content.get('text'),str):
                raise ValueError('Invalid ACP text')
            return [{'type':'text','data':content['text']}]
        if kind=='tool_call':
            identity=update.get('toolCallId')
            if not isinstance(identity,str) or not re.fullmatch(r'[A-Za-z0-9_.:-]{1,128}',identity) or identity in self.calls or len(self.calls)>=4096:
                raise ValueError('Invalid ACP tool identity')
            self.calls[identity]=update
            return []
        if kind=='tool_call_update':
            identity=update.get('toolCallId')
            if not isinstance(identity,str) or identity not in self.calls:raise ValueError('Unbound ACP tool update')
            if not same_tool_input(self.calls[identity],update):raise ValueError('ACP tool input changed after binding')
            return []
        if kind in {'user_message_chunk','agent_thought_chunk','plan','available_commands','current_mode_update','config_option_update','usage_update'}:return []
        raise ValueError('Unsupported ACP update')


class Factory:
    def create(self,*,mode,workspace,git_configs,notes,prompt,progress):
        return Transport(prompt,Permissions(mode,NativePaths(workspace,tuple(notes),tuple(git_configs)),progress=progress))


def selected_factory(config):
    if config.grok_transport=='headless-v1':return None
    if config.grok_transport!=SELECTION:raise ValueError('Unsupported Grok transport selection')
    return Factory()
