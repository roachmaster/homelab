"""Pinned Grok headless adapter from installed1.0.92 help/bundled guides.

Fresh attempt sessions, terminal-only accounting, no native calls at import.
"""
import os
import re
import stat
from uuid import UUID

VERSION='1.0.92'
BINARY_SHA256='add4d201dbf2c0bd6077a36feb55a76939de30bae48dbb06f16b2307d38ceacb'
MAX_COUNTER=10**40-1
CONFIG='''[cli]
auto_update = false
use_leader = false
session_registry = false
[session]
load_envrc = false
[features]
shared_daemon = false
auto_wake = false
ask_user_question = false
cloud_agents = false
backend_tools = false
title_refresh = false
telemetry = false
[managed_mcps]
enabled = false
gateway_tools_enabled = false
[memory]
enabled = false
[memory_v2]
enabled = false
[workflows]
enabled = false
[goal]
enabled = false
[compat.claude]
hooks = false
mcps = false
[compat.codex]
hooks = false
[compat.cursor]
hooks = false
mcps = false
[telemetry]
otel_enabled = false
trace_upload = false
'''


# Installed1.0.92 guides05 (project config/LSP),07 (MCP compatibility),
# 09 (plugins),10 (hooks),16 (agent roles) and22 (folder trust).
# Fixed metadata checks only: never recurse into or read project config bodies.
PROJECT_EXECUTABLE_PATHS=(
    '.grok/config.toml', '.grok/sandbox.toml', '.grok/lsp.json',
    '.grok/hooks', '.grok/plugins', '.grok/agents', '.grok/personas', '.grok/roles', '.grok/workflows',
    '.claude/settings.json', '.claude/settings.local.json', '.claude/plugins',
    '.cursor/hooks.json', '.cursor/mcp.json', '.mcp.json', '.envrc',
)
PROJECT_CONFIG_ROOTS=('.grok','.claude','.cursor')
MANAGED_POLICY='allow_managed_hooks_only = true\nallow_managed_mcp_servers_only = true\nallowed_mcp_servers = []\nenable_all_project_mcp_servers = false\nstrict_known_marketplaces = []\n'


def validate_project_subset(workspace):
    """Accept instruction-only projects before acquiring auth or native state.

    Leaf or container symlinks (including dangling links) are presence, not an
    invitation to follow/read another directory. Unreadable metadata fails shut.
    """
    root=workspace.lstat()
    if not stat.S_ISDIR(root.st_mode):
        raise ValueError('Unsupported Grok project scope')
    for relative in PROJECT_CONFIG_ROOTS:
        try: info=(workspace/relative).lstat()
        except FileNotFoundError: continue
        if not stat.S_ISDIR(info.st_mode):
            raise ValueError('Unsupported Grok project configuration')
    for relative in PROJECT_EXECUTABLE_PATHS:
        try: (workspace/relative).lstat()
        except FileNotFoundError: continue
        raise ValueError('Unsupported Grok project executable configuration')


def argv(binary,model,workspace,prompt_file,session_id,leader_socket,*,implement):
    tools='read_file,grep,list_dir,run_terminal_cmd'
    if implement:tools+=',search_replace,write'
    args=(str(binary),'--no-auto-update','--trust','--prompt-file',str(prompt_file),
          '--output-format','streaming-json','--model',model,'--cwd',str(workspace),
          '--session-id',session_id,'--leader-socket',str(leader_socket),
          '--no-memory','--no-subagents','--no-plan','--disable-web-search',
          '--permission-mode','dontAsk','--tools',tools,
          '--allow','Read(*)','--allow','Bash(*)','--deny','MCPTool(*)',
          '--sandbox','workspace' if implement else 'read-only')
    return args+('--allow','Edit(*)') if implement else args


def stage_auth(account,destination):
    """Acquire only the private account auth inode, never hooks/config/history."""
    directory=None
    try:
        directory=os.open(account.home,os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW)
        child=os.open('.grok',os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW,dir_fd=directory)
        os.close(directory);directory=child
        info=os.fstat(directory)
        if info.st_uid!=account.uid or info.st_mode&0o077:raise ValueError('Unsafe Grok account directory')
        descriptor=os.open('auth.json',os.O_RDONLY|os.O_NOFOLLOW|os.O_NONBLOCK,dir_fd=directory)
        with os.fdopen(descriptor,'rb')as source:
            info=os.fstat(source.fileno())
            if (not stat.S_ISREG(info.st_mode) or info.st_uid!=account.uid
                    or info.st_mode&0o077 or not 0<info.st_size<=1024*1024):
                raise ValueError('Unsafe Grok account state')
            data=source.read(1024*1024+1)
            if len(data)!=info.st_size:raise ValueError('Grok account changed during acquisition')
        descriptor=os.open(destination,os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,0o400)
        with os.fdopen(descriptor,'wb')as stream:stream.write(data)
        return True
    except FileNotFoundError:return False
    finally:
        if directory is not None:os.close(directory)


class GrokEvents:
    """One fresh session; ignore thoughts/tool bodies and per-response spend."""
    def __init__(self,expected_session):
        self.session=expected_session
        self.response=''
        self.usage=None
        self.complete=False
        self.failure=None
        self.cancelled=False
        self.saw_work=False
    def record(self,event):
        kind=event.get('type')
        if not isinstance(kind,str):raise ValueError('Missing Grok event type')
        if self.complete:raise ValueError('Grok emitted data after terminal output')
        if kind=='text':
            if not isinstance(event.get('data'),str):raise ValueError('Invalid Grok text')
            self.response+=event['data'];self.saw_work=True
        elif kind in {'tool_call','tool_call_update'}:self.saw_work=True
        elif kind=='error':
            message=event.get('message')
            if not isinstance(message,str):raise ValueError('Invalid Grok error')
            self.failure=message[:1024]
            self.cancelled=event.get('stopReason')=='cancelled' or message.strip().lower()=='cancelled'
            incomplete=event.get('usage_is_incomplete',False)
            if type(incomplete)is not bool:raise ValueError('Invalid Grok accounting coverage')
            # Early errors may not have a native session yet. Retain their
            # blocked reason, but never attribute unbound spend to this job.
            for key in ('sessionId','cwd','requestId'):
                if key not in event:continue
                value=event[key]
                if key=='sessionId':
                    if (not isinstance(value,str) or str(UUID(value))!=value
                            or self.session is not None and value!=self.session):
                        raise ValueError('Grok error session binding changed')
                elif key=='cwd' and value!='/run/forge-coding-workspace':
                    raise ValueError('Grok error workspace binding changed')
                elif key=='requestId' and not (value=='' and self.cancelled and incomplete):
                    if not isinstance(value,str) or not re.fullmatch(r'[A-Za-z0-9_.:-]{1,128}',value):
                        raise ValueError('Invalid Grok error request identity')
            bound=all(key in event for key in ('sessionId','cwd','requestId')) and bool(event.get('requestId'))
            self.usage=self._usage(event.get('usage')) if bound and not incomplete else None
            self.complete=True
        elif kind=='end':
            session=event.get('sessionId')
            if not isinstance(session,str) or str(UUID(session))!=session or self.session is not None and session!=self.session:
                raise ValueError('Grok session binding changed')
            request=event.get('requestId')
            interrupted = event.get('stopReason') == 'cancelled' and event.get('usage_is_incomplete') is True
            if not (request == '' and interrupted) and (not isinstance(request,str) or not re.fullmatch(r'[A-Za-z0-9_.:-]{1,128}',request)):
                raise ValueError('Invalid Grok request identity')
            if event.get('cwd')!='/run/forge-coding-workspace':raise ValueError('Grok workspace binding changed')
            self.session=session
            stop=event.get('stopReason')
            if stop!='end_turn' or event.get('error') is not None:
                self.failure=stop if isinstance(stop,str) else 'failed'
                self.cancelled=stop=='cancelled'
            incomplete=event.get('usage_is_incomplete',False)
            if type(incomplete)is not bool:raise ValueError('Invalid Grok accounting coverage')
            self.usage=None if incomplete else self._usage(event.get('usage'))
            self.complete=True
        elif kind not in {'thought','usage','plan','available_commands'}:
            raise ValueError('Unsupported Grok event type')
    @staticmethod
    def _usage(value):
        if value is None:return None
        if not isinstance(value,dict):raise ValueError('Invalid Grok terminal usage')
        keys=('input_tokens','cache_read_input_tokens','cache_creation_input_tokens','output_tokens','total_tokens')
        result={key:value.get(key)for key in keys}
        if any(v is not None and(type(v)is not int or not 0<=v<=MAX_COUNTER)for v in result.values()):
            raise ValueError('Invalid Grok terminal counter')
        parts=[result[key]for key in keys[:-1]]
        if all(v is not None for v in parts):
            total=sum(parts)
            if total>MAX_COUNTER or result['total_tokens'] is not None and total!=result['total_tokens']:
                raise ValueError('Inconsistent Grok terminal accounting')
        return result
