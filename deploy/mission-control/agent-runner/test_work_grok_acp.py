"""Effect-free policy/protocol and real fake-child lifecycle; no Grok account calls."""
from dataclasses import replace
import hashlib
import json
import os
from pathlib import Path
import subprocess
from tempfile import TemporaryDirectory
import threading
import unittest
from unittest.mock import patch
from uuid import uuid4
from work_grok_acp import (Factory,NativePaths,Permissions,Transport,WORKSPACE,STATE,
                           CONFIG_SUFFIX,MAX_FRAME,argv,selected_factory,wire,git_config_paths,terminal_usage)
from work_executor import CommandRunnerProvider,ExecutionError,SystemdSupervisor,WorkspaceLease
from work_grok import CONFIG
from work_worker_config import parse_config,validated_copy
import test_work_grok as grok_fixture
from test_work_executor import FakeSupervisor,lease

SID='00000000-0000-4000-8000-000000000041'


def tool(name,raw,identity='call-1'):
    return {'sessionUpdate':'tool_call','toolCallId':identity,'rawInput':raw,
            '_meta':{'x.ai/tool':{'version':1,'namespace':'grok_build','name':name}}}


def initialize(transport):
    first=json.loads(transport.drain())
    assert first['method']=='initialize'
    transport.feed(wire({'jsonrpc':'2.0','id':'host-initialize','result':{'protocolVersion':1}}))
    new=json.loads(transport.drain());assert new['params']=={'cwd':WORKSPACE,'mcpServers':[]}
    transport.feed(wire({'jsonrpc':'2.0','id':'host-new','result':{'sessionId':SID}}))
    prompt=json.loads(transport.drain());assert prompt['params']['sessionId']==SID
    return first,new,prompt


def update(transport,value,session=SID):
    return transport.feed(wire({'jsonrpc':'2.0','method':'session/update','params':{'sessionId':session,'update':value}}))


def permission(transport,call,identity='permission-1',options=None):
    return transport.feed(wire({'jsonrpc':'2.0','id':identity,'method':'session/request_permission',
        'params':{'sessionId':SID,'toolCall':call,'options':options or [
            {'optionId':'native-allow','name':'Allow once','kind':'allow_once'},
            {'optionId':'native-reject','name':'Reject once','kind':'reject_once'},
            {'optionId':'native-always','name':'Always','kind':'allow_always'}]}}))


class PermissionTest(unittest.TestCase):
    def setUp(self):
        self.temp=TemporaryDirectory();self.addCleanup(self.temp.cleanup)
        self.root=Path(self.temp.name);self.workspace=self.root/'workspace';self.workspace.mkdir()
        (self.workspace/'file').write_text('source')
        self.note=self.root/'approved-plan.md';self.note.write_text('immutable plan')
        self.paths=NativePaths(self.workspace,((STATE+'/approved-plan.md',self.note),))
        self.plan=Permissions('plan',self.paths,progress=True)

    def test_actual_incident_compound_read_only_git_is_admitted(self):
        command='git status --short --branch && echo \\"---BRANCH---\\" && git branch --show-current && echo \\"---HEAD---\\" && git rev-parse HEAD && echo \\"---LOG---\\" && git log -5 --oneline && echo \\"---WORKTREES---\\" && git worktree list'
        self.assertTrue(self.plan.terminal(command))
        self.assertTrue(self.plan.admit(tool('run_terminal_command',{'command':command,'description':'read inventory'})))

    def test_shell_writes_substitutions_outside_cwd_and_unknown_tools_rejected(self):
        for command in ('git status && git push','git branch feature/evil','git commit -am x',
                        'git log --output=/tmp/leak','git -c core.fsmonitor=evil status',
                        'git status; touch x','git status > /tmp/out','git status | cat',
                        'git status && $(id)','git status && `id`','git status &','env git status',
                        'sh -c "git status"','cd /tmp && git status','git status\nrm -rf x',
                        'python3 -c print(1)','curl https://example.com','cargo test'):
            with self.subTest(command=command):self.assertFalse(self.plan.terminal(command))
        self.assertFalse(self.plan.terminal('git status','/tmp'))
        for name in ('MCPTool','mcp__read','spawn_subagent','web_search','unknown'):
            self.assertFalse(self.plan.admit(tool(name,{'path':'file'})))

    def test_shell_expansion_cannot_inject_unchecked_rg_operands(self):
        import shlex
        (self.workspace/'linked-note').symlink_to(self.note)
        command='rg -n -- *'
        lexical=shlex.split(command)
        # Built-ins only: demonstrate expansion without executing rg, reading
        # link contents or launching any native tool/account/model process.
        observed=subprocess.run(['/bin/bash','--noprofile','--norc','-c',
            'set -- rg -n -- *; printf "%s\\n" "$@"'],cwd=self.workspace,
            env={'PATH':'/usr/bin:/bin'},capture_output=True,text=True,check=True).stdout.splitlines()
        self.assertEqual(lexical,['rg','-n','--','*'])
        self.assertIn('linked-note',observed);self.assertNotEqual(observed,lexical)
        for expression in ('*','?','[fl]*','{file,linked-note}','~','@(file)'):
            for quoted in (expression,"'"+expression+"'",'"'+expression+'"'):
                with self.subTest(pattern=quoted):self.assertFalse(self.plan.terminal('rg -n -- '+quoted))
        self.assertTrue(self.plan.terminal('rg -n -- source'))

    def test_native_file_paths_notes_and_symlinks_are_scoped(self):
        self.assertTrue(self.plan.admit(tool('read_file',{'target_file':'file','limit':100})))
        self.assertTrue(self.plan.admit(tool('read_file',{'target_file':STATE+'/approved-plan.md'})))
        for path in (STATE+'/home/.grok/auth.json',STATE+'/prompt.txt','../outside','/etc/passwd','.git/config'):
            self.assertFalse(self.plan.admit(tool('read_file',{'target_file':path})))
        (self.workspace/'link').symlink_to(self.note)
        self.assertFalse(self.plan.admit(tool('read_file',{'target_file':'link'})))
        self.assertFalse(self.plan.admit(tool('read_file',{'target_file':'file','path':'/etc/passwd'})))
        fake=tool('read_file',{'target_file':'file'});fake['_meta']['x.ai/tool']['namespace']='mcp'
        self.assertFalse(self.plan.admit(fake))

    def test_fixed_report_helper_only_and_implementation_capability(self):
        command="python3 "+STATE+"/report_progress.py plan 'Inspect current baseline'"
        self.assertTrue(self.plan.terminal(command))
        self.assertFalse(Permissions('plan',self.paths).terminal(command))
        self.assertFalse(self.plan.terminal(command+' && git push'))
        self.assertFalse(self.plan.terminal('python3 '+STATE+'/other.py'))
        self.assertFalse(self.plan.admit(tool('write',{'file_path':'file','contents':'edit'})))
        implementation=Permissions('implement',self.paths)
        self.assertTrue(implementation.admit(tool('write',{'file_path':'new','contents':'edit'})))
        self.assertTrue(implementation.admit(tool('search_replace',{'path':'file','old_string':'source','new_string':'edit'})))
        self.assertFalse(implementation.admit(tool('write',{'path':'file','file_path':'/etc/outside','contents':'edit'})))
        self.assertTrue(implementation.terminal('cargo test --offline --locked'))
        self.assertFalse(implementation.terminal('cargo test --config core.x=y'))
        self.assertFalse(implementation.terminal('cargo test --manifest-path /tmp/Cargo.toml'))
        self.assertFalse(implementation.terminal('cargo publish'))
        self.assertFalse(implementation.admit(tool('write',{'file_path':STATE+'/home/.grok/config.toml','contents':'x'})))

    def test_git_executable_config_is_not_approved_as_read_only(self):
        config=self.root/'config'
        scoped=Permissions('plan',NativePaths(self.workspace,git_configs=(config,)))
        config.write_text('[core]\n bare = true\n');self.assertTrue(scoped.terminal('git status'))
        for content in ('[core]\n fsmonitor = evil\n','[include]\n path = /tmp/evil\n',
                        '[core] fsmonitor = /tmp/fixture-never-executed\n',
                        '[filter "demo"]\n clean = /tmp/fixture-never-executed\n',
                        '[filter "demo"]\n process = /tmp/fixture-never-executed\n',
                        '[log]\n showSignature = true\n[gpg]\n program = /tmp/fixture-never-executed\n',
                        '[core]\n attributesFile = /tmp/fixture-never-read\n',
                        '[diff "x"]\n textconv = evil\n','[alias]\n status = !evil\n'):
            config.write_text(content);self.assertFalse(scoped.terminal('git status'))
        parsed=subprocess.run(['/usr/bin/git','config','--file',str(config),'--get','alias.status'],
                              capture_output=True,check=True,text=True)
        self.assertEqual(parsed.stdout.strip(),'!evil')
        config.write_text('[core] fsmonitor = /tmp/fixture-never-executed\n')
        parsed=subprocess.run(['/usr/bin/git','config','--file',str(config),'--get','core.fsmonitor'],
                              capture_output=True,check=True,text=True)
        self.assertEqual(parsed.stdout.strip(),'/tmp/fixture-never-executed')
        self.assertFalse(scoped.terminal('git status'))

    def test_git_common_config_is_bound_for_bare_and_legacy_worktrees(self):
        from types import SimpleNamespace
        for common in (self.root/'owned.git',self.root/'canonical/.git'):
            workspace=SimpleNamespace(repository=self.root/'canonical',git_directory=common/'worktrees/task')
            self.assertEqual(git_config_paths(workspace)[0],common/'config')
        with self.assertRaises(ValueError):git_config_paths(SimpleNamespace(git_directory=self.root/'arbitrary'))

    def test_symlink_ancestors_cannot_supply_workspace_or_note_proofs(self):
        real=self.root/'real';real.mkdir();(real/'nested').mkdir();(real/'nested/file').write_text('x')
        link=self.root/'parent-link';link.symlink_to(real,target_is_directory=True)
        self.assertFalse(NativePaths(link/'nested').allows('file'))
        notes=NativePaths(self.workspace,((STATE+'/approved-plan.md',link/'nested/file'),))
        self.assertFalse(notes.allows(STATE+'/approved-plan.md'))

    def test_implementation_test_tools_do_not_enable_publish_or_plan_execution(self):
        implement=Permissions('implement',self.paths)
        for command in ('make ci-preflight','python3 -B -m unittest test_example -q','cargo check --offline',
                        'python3 -B -m unittest discover -s deploy/mission-control/agent-runner -p test_example.py -q',
                        'python3 -B deploy/mission-control/backend-suites.py --scope branch'):
            self.assertTrue(implement.terminal(command));self.assertFalse(self.plan.terminal(command))
        for command in ('make deploy','python3 -B script.py','python3 -B -m unittest /tmp/test.py',
                        'cargo run','cargo test && git push','make ci-preflight NETWORK=on'):
            self.assertFalse(implement.terminal(command))


class ProtocolTest(unittest.TestCase):
    def transport(self):
        return Transport('Bound task',Permissions('plan',NativePaths(Path('/nonexistent'))))

    def test_initialize_session_prompt_no_external_capabilities_or_yolo(self):
        first,new,prompt=initialize(self.transport())
        self.assertFalse(first['params']['clientCapabilities']['terminal'])
        self.assertEqual(first['params']['clientCapabilities']['fs'],{'readTextFile':False,'writeTextFile':False})
        self.assertNotIn('_meta',new['params']);self.assertEqual(new['params']['mcpServers'],[])
        self.assertIn('CONTROLLER PERMISSION BOUNDARY',prompt['params']['prompt'][0]['text'])

    def test_partial_messages_match_ids_and_result_output_only(self):
        transport=self.transport();initialize(transport)
        raw=wire({'jsonrpc':'2.0','method':'session/update','params':{'sessionId':SID,'update':{
            'sessionUpdate':'agent_message_chunk','content':{'type':'text','text':'Public answer'}}}})
        output=b''
        for byte in raw:output+=transport.feed(bytes([byte]))
        self.assertEqual(json.loads(output),{'type':'text','data':'Public answer'})
        self.assertEqual(update(transport,{'sessionUpdate':'user_message_chunk','content':{'type':'text','text':'private prompt echo'}}),b'')
        self.assertEqual(update(transport,{'sessionUpdate':'agent_thought_chunk','content':{'type':'text','text':'private'}}),b'')
        terminal=transport.feed(wire({'jsonrpc':'2.0','id':'host-prompt','result':{'stopReason':'end_turn'}}))
        self.assertEqual(json.loads(terminal)['sessionId'],SID);self.assertIsNone(json.loads(terminal)['usage'])
        transport.eof()
        with self.assertRaises(ValueError):transport.feed(raw)

    def test_denial_is_selected_reject_once_and_turn_can_continue(self):
        transport=self.transport();initialize(transport)
        call=tool('run_terminal_command',{'command':'git push'})
        update(transport,call);permission(transport,call)
        reply=json.loads(transport.drain())
        self.assertEqual(reply['result']['outcome'],{'outcome':'selected','optionId':'native-reject'})
        self.assertIn('Do not use shell',reply['result']['_meta']['homelab/permission'])
        self.assertFalse(transport.complete)
        self.assertIn(b'Useful alternative',update(transport,{'sessionUpdate':'agent_message_chunk','content':{'type':'text','text':'Useful alternative'}}))
        terminal=transport.feed(wire({'jsonrpc':'2.0','id':'host-prompt','result':{'stopReason':'end_turn'}}))
        self.assertEqual(json.loads(terminal)['stopReason'],'end_turn')

    def test_allow_once_only_by_provided_option_identity(self):
        transport=self.transport();initialize(transport)
        call=tool('run_terminal_command',{'command':'git status'})
        update(transport,call);permission(transport,call,options=[{'optionId':'opaque-42','name':'Once','kind':'allow_once'},
            {'optionId':'bad-always','name':'Always','kind':'allow_always'}])
        self.assertEqual(json.loads(transport.drain())['result']['outcome'],{'outcome':'selected','optionId':'opaque-42'})

    def test_no_missing_option_fallback_or_permission_identity_change(self):
        for mutation in ('unbound','changed','no_once','duplicate'):
            transport=self.transport();initialize(transport)
            call=tool('run_terminal_command',{'command':'git status'})
            if mutation!='unbound':update(transport,call)
            if mutation=='changed':call={**call,'rawInput':{'command':'git push'}}
            options=[{'optionId':'x','name':'Always','kind':'allow_always'}] if mutation=='no_once' else None
            if mutation=='duplicate':
                permission(transport,call);transport.drain()
            with self.subTest(mutation=mutation),self.assertRaises(ValueError):permission(transport,call,options=options)

    def test_changed_tool_update_cannot_lend_original_permission_identity(self):
        transport=self.transport();initialize(transport)
        call=tool('run_terminal_command',{'command':'git status'})
        update(transport,call)
        with self.assertRaises(ValueError):update(transport,{'sessionUpdate':'tool_call_update',
            'toolCallId':'call-1','rawInput':{'command':'git push'}})

    def test_status_metadata_can_progress_without_changing_tool_identity(self):
        transport=self.transport();initialize(transport)
        call=tool('run_terminal_command',{'command':'git status'})
        update(transport,call)
        self.assertEqual(update(transport,{'sessionUpdate':'tool_call_update','toolCallId':'call-1',
            'status':'failed','_meta':{'toolNotRun':'cancelled'}}),b'')
        changed={'sessionUpdate':'tool_call_update','toolCallId':'call-1','_meta':{'x.ai/tool':{'name':'write'}}}
        with self.assertRaises(ValueError):update(transport,changed)

    def test_early_notification_requires_returned_session_identity(self):
        transport=self.transport();transport.drain()
        transport.feed(wire({'jsonrpc':'2.0','id':'host-initialize','result':{'protocolVersion':1}}));transport.drain()
        update(transport,{'sessionUpdate':'available_commands','availableCommands':[]})
        transport.feed(wire({'jsonrpc':'2.0','id':'host-new','result':{'sessionId':SID}}))
        self.assertEqual(json.loads(transport.drain())['method'],'session/prompt')

    def test_notification_spend_never_becomes_completed_turn_spend(self):
        transport=self.transport();initialize(transport)
        transport.feed(wire({'jsonrpc':'2.0','method':'_x.ai/session/update','params':{'sessionId':SID,
            'update':{'sessionUpdate':'turn_completed','stop_reason':'end_turn','usage':{'inputTokens':99999}}}}))
        event=json.loads(transport.feed(wire({'jsonrpc':'2.0','id':'host-prompt','result':{'stopReason':'end_turn'}})))
        self.assertIsNone(event['usage'])

    def test_native_permission_cancel_is_distinct_from_user_cancel(self):
        from work_executor import CliParser
        for category,expected in (('PermissionCancelled','blocked_permission'),('UserCancelled','cancelled')):
            transport=self.transport();initialize(transport)
            transport.feed(wire({'jsonrpc':'2.0','method':'_x.ai/session/update','params':{'sessionId':SID,
                'update':{'sessionUpdate':'turn_completed','stop_reason':'cancelled'},'_meta':{'cancellationCategory':category}}}))
            event=transport.feed(wire({'jsonrpc':'2.0','id':'host-prompt','result':{'stopReason':'cancelled'}}))
            from test_work_executor import proof
            parser=CliParser('grok');parser.feed(event)
            self.assertEqual(parser.finish(0,proof()).status,expected)
            parser=CliParser('grok');parser.feed(event)
            self.assertEqual(parser.finish(0,proof(),interrupted=True).status,'cancelled')

    def test_malformed_mismatched_response_session_and_eof_fail_closed(self):
        for raw in (b'{"jsonrpc":"2.0","id":"host-initialize","id":"x","result":{}}\n',
                    wire({'jsonrpc':'2.0','id':'wrong','result':{}}),
                    wire({'jsonrpc':'2.0','id':'host-initialize','result':{'protocolVersion':True}}),
                    b'{"x":NaN}\n',b'[]\n',b'x'*(MAX_FRAME+1)):
            transport=self.transport();transport.drain()
            with self.subTest(raw=raw[:70]),self.assertRaises(ValueError):transport.feed(raw)
        transport=self.transport();initialize(transport)
        with self.assertRaises(ValueError):update(transport,{'sessionUpdate':'plan'},str(uuid4()))
        with self.assertRaises(ValueError):transport.eof()

    def test_wrong_field_types_fail_with_fixed_protocol_error(self):
        for method,params in (
                (None,{'stopReason':[]}),
                ('session/update',{'sessionId':SID,'update':{'sessionUpdate':[]}}),
                ('session/update',{'sessionId':SID,'update':{'sessionUpdate':'tool_call_update','toolCallId':[]}}),
                ('_x.ai/session/update',{'sessionId':SID,'update':{'sessionUpdate':'turn_completed','stop_reason':[]}})):
            transport=self.transport();initialize(transport)
            frame={'jsonrpc':'2.0','id':'host-prompt','result':params} if method is None else {'jsonrpc':'2.0','method':method,'params':params}
            with self.subTest(method=method),self.assertRaises(ValueError):transport.feed(wire(frame))

    def test_actual_acp_full_prompt_cache_projection_and_invalid_totals(self):
        usage=terminal_usage({'inputTokens':10061,'cachedReadTokens':4224,'cacheCreationTokens':0,'outputTokens':808,'totalTokens':10869})
        self.assertEqual(usage,{'input_tokens':5837,'cache_read_input_tokens':4224,
            'cache_creation_input_tokens':0,'output_tokens':808,'total_tokens':10869})
        for usage in ({'inputTokens':1,'cachedReadTokens':2,'cacheCreationTokens':0},
                      {'inputTokens':10,'outputTokens':5,'totalTokens':14}, {'inputTokens':True}):
            with self.assertRaises(ValueError):terminal_usage(usage)

    def test_client_effect_rpc_is_rejected_not_executed(self):
        transport=self.transport();initialize(transport)
        transport.feed(wire({'jsonrpc':'2.0','id':'terminal-x','method':'terminal/create','params':{'sessionId':SID,'command':'id'}}))
        self.assertEqual(json.loads(transport.drain())['error']['code'],-32601)

    def test_bound_native_errors_keep_blocked_auth_quota_and_cancellation_without_raw_prose(self):
        from work_executor import CliParser
        from test_work_executor import proof
        for message,code,status in (('Authentication required PRIVATE',-32000,'blocked_auth'),
                                    ('usage_limit_reached PRIVATE resets in 1h',-32000,'quota_blocked'),
                                    ('PRIVATE',-32800,'cancelled')):
            transport=self.transport();initialize(transport)
            output=transport.feed(wire({'jsonrpc':'2.0','id':'host-prompt','error':{'code':code,'message':message}}))
            self.assertNotIn(b'PRIVATE',output)
            parser=CliParser('grok');parser.feed(output)
            if transport.quota_diagnostic is not None:parser.quota_diagnostic=transport.quota_diagnostic
            result=parser.finish(0,proof());self.assertEqual(result.status,status)
            if status=='quota_blocked':self.assertEqual(result.quota_diagnostic['retry_after_seconds'],3600)

    def test_terminal_native_usage_preserved_not_step_summed_and_true_cancel_kept(self):
        transport=self.transport();initialize(transport)
        transport.feed(wire({'jsonrpc':'2.0','method':'_x.ai/session/update','params':{'sessionId':SID,'update':{
            'sessionUpdate':'turn_completed','stop_reason':'cancelled','usage':{
                'inputTokens':85,'cachedReadTokens':70,'cacheCreationTokens':5,'outputTokens':15,'totalTokens':100}}}}))
        event=json.loads(transport.feed(wire({'jsonrpc':'2.0','id':'host-prompt','result':{'stopReason':'cancelled','_meta':{'usage':{'inputTokens':85,'cachedReadTokens':70,'cacheCreationTokens':5,'outputTokens':15,'totalTokens':100}}}})))
        self.assertEqual(event['stopReason'],'cancelled');self.assertEqual(event['usage']['total_tokens'],100)


class IntegrationTest(unittest.TestCase):
    def test_explicit_selection_preserves_legacy_config_and_argv(self):
        raw=grok_fixture.GrokBoundaryTest().raw('/tmp/acp-config',True);old=parse_config(raw)
        self.assertIsNone(selected_factory(old));self.assertEqual(validated_copy(old).grok_transport,'headless-v1')
        raw['grok_transport']='acp-constrained-v1';new=parse_config(raw)
        self.assertIsInstance(selected_factory(new),Factory);self.assertEqual(validated_copy(new),new)
        for value in ('always-approve',True,{},None):
            raw['grok_transport']=value
            with self.assertRaises(ValueError):parse_config(raw)
        args=argv('/grok','model',implement=False)
        self.assertEqual(args,('/grok','--no-auto-update','--trust','--cwd',WORKSPACE,
            '--no-memory','--no-subagents','--no-plan','--disable-web-search',
            '--permission-mode','default','--tools','read_file,grep,list_dir,run_terminal_cmd',
            '--deny','MCPTool(*)','--sandbox','read-only','agent','--model','model','--no-leader','stdio'))
        self.assertNotIn('--allow',args);self.assertNotIn('dontAsk',args);self.assertNotIn('--always-approve',args)
        self.assertNotIn('--reauth',args);self.assertNotIn('--leader',args)
        self.assertNotIn('g1',' '.join(args).lower())
        self.assertIn('ask = ["Bash"',CONFIG_SUFFIX)
        with self.assertRaises(TypeError):CommandRunnerProvider(new,SystemdSupervisor(new))

    def run_child(self,script,*,stop=None,progress=None,signal_after_spawn=False,scope_proven=True,clock=None):
        with TemporaryDirectory()as root:
            raw=grok_fixture.GrokBoundaryTest().raw(root,True);raw['grok_transport']='acp-constrained-v1';config=parse_config(raw)
            auth=config.account.home/'.grok/auth.json';auth.parent.mkdir(parents=True,mode=0o700)
            auth.write_text('fixture native credential');auth.chmod(0o600)
            value=lease(config,backend='grok');value['agent_profile']='grok'
            supervisor=FakeSupervisor(script);supervisor.enforces_coding_sandbox=True
            if not scope_proven:
                normal_stop=supervisor.stop
                def uncertain_stop(unit):return {**normal_stop(unit),'scope_stopped':False}
                supervisor.stop=uncertain_stop
            def spawn(*args,**kwargs):
                supervisor.process=subprocess.Popen(*args,**kwargs)
                if signal_after_spawn:
                    timer=threading.Timer(.1,stop.set);timer.start();self.addCleanup(timer.join)
                return supervisor.process
            runner=CommandRunnerProvider(config,supervisor,popen=spawn,grok_transport_factory=Factory())
            if clock is not None:runner.clock=clock
            with patch('work_executor.GROK_BINARY_SHA256',hashlib.sha256(Path('/usr/bin/python3').resolve().read_bytes()).hexdigest()):runner.prepare()
            workspace=WorkspaceLease(Path(root)/'owned',Path(root)/'repository',Path(root)/'repository/worktrees/task',value['task_id'],'a'*40);workspace.path.mkdir();workspace.git_directory.mkdir(parents=True)
            outcome=runner.run(value,workspace,stop or threading.Event(),report_progress=progress)
            self.assertIsNotNone(supervisor.process.returncode)
            self.assertEqual((config.state_root/'attempts'/value['attempt_id']/'home/.grok/auth.json').exists(),not scope_proven)
            self.assertEqual(auth.read_text(),'fixture native credential')
            return outcome

    def test_real_fake_acp_server_partial_io_denied_then_result_cleanup(self):
        outcome=self.run_child(FAKE_SERVER)
        self.assertEqual(outcome.status,'awaiting_review');self.assertEqual(outcome.text,'Owned ACP result')
        self.assertEqual(outcome.conversation_id,SID);self.assertEqual(outcome.usage['total_tokens'],100)
        self.assertTrue(outcome.stop_proof['scope_stopped'])

    def test_first_plan_progress_uses_only_the_controller_generated_helper(self):
        reports=[]
        reporting=r'''
import subprocess
call={'sessionUpdate':'tool_call','toolCallId':'call-report','rawInput':{'command':"python3 /run/forge-coding-attempt/report_progress.py plan 'Public plan'"},'_meta':{'x.ai/tool':{'version':1,'namespace':'grok_build','name':'run_terminal_command'}}}
send({'jsonrpc':'2.0','method':'session/update','params':{'sessionId':sid,'update':call}})
send({'jsonrpc':'2.0','id':'ask-report','method':'session/request_permission','params':{'sessionId':sid,'toolCall':call,'options':[{'optionId':'once','name':'Allow','kind':'allow_once'},{'optionId':'deny','name':'Reject','kind':'reject_once'}]}})
assert receive()['result']['outcome']=={'outcome':'selected','optionId':'once'}
subprocess.run(['/usr/bin/python3',__SCRATCH__+'/report_progress.py','plan','Public plan'],check=True)
'''
        script=FAKE_SERVER.replace("c=receive();assert c['method']=='session/prompt'", "c=receive();assert c['method']=='session/prompt'\n"+reporting)
        outcome=self.run_child(script,progress=lambda seq,kind,summary:reports.append({'sequence':seq,'kind':kind,'summary':summary}))
        self.assertEqual(outcome.status,'awaiting_review')
        self.assertEqual([(report['kind'],report['summary'])for report in reports],[('plan','Public plan')])

    def test_mismatched_server_reply_is_failed_and_scope_auth_cleaned(self):
        outcome=self.run_child("import sys,json;sys.stdin.readline();print(json.dumps({'jsonrpc':'2.0','id':'wrong','result':{}}),flush=True);sys.stdin.read()")
        self.assertEqual(outcome.status,'failed');self.assertTrue(outcome.stop_proof['scope_stopped'])

    def test_true_operator_stop_and_deadline_preserve_cleanup_semantics(self):
        outcome=self.run_child('import sys,time;sys.stdin.readline();time.sleep(20)',
                               stop=threading.Event(),signal_after_spawn=True)
        self.assertEqual(outcome.status,'cancelled');self.assertTrue(outcome.stop_proof['scope_stopped'])
        ticks=iter(range(0,10000000,100000))
        outcome=self.run_child('import sys,time;sys.stdin.readline();time.sleep(20)',clock=lambda:next(ticks))
        self.assertEqual(outcome.status,'failed');self.assertIn('wall-time',outcome.summary)

    def test_unverified_scope_retains_credential_for_reconciliation(self):
        outcome=self.run_child(FAKE_SERVER,scope_proven=False)
        self.assertEqual(outcome.status,'needs_reconcile');self.assertFalse(outcome.stop_proof['scope_stopped'])

    def test_supervisor_binds_exact_argv_mode_context_and_immutable_progress_helper(self):
        with TemporaryDirectory() as root:
            raw=grok_fixture.GrokBoundaryTest().raw(root,True);raw['grok_transport']='acp-constrained-v1'
            config=parse_config(raw);supervisor=SystemdSupervisor(config)
            runner=CommandRunnerProvider(config,supervisor,grok_transport_factory=Factory())
            with patch('work_executor.GROK_BINARY_SHA256',hashlib.sha256(Path('/usr/bin/python3').resolve().read_bytes()).hexdigest()):runner.prepare()
            auth=config.account.home/'.grok/auth.json';auth.parent.mkdir(parents=True,mode=0o700)
            auth.write_text('fixture');auth.chmod(0o600)
            value=lease(config,backend='grok');value['agent_profile']='grok'
            workspace=WorkspaceLease(Path(root)/'owned',Path(root)/'repository',Path(root)/'repository/worktrees/task',value['task_id'],'a'*40)
            workspace.path.mkdir();workspace.git_directory.mkdir(parents=True)
            invocation=runner.invocation(value,workspace,progress=True)
            try:
                command=supervisor.argv(value,invocation)
                self.assertIn('report_progress.py',' '.join(command))
                self.assertEqual((invocation.scratch/'home/.grok/config.toml').read_text(),CONFIG+CONFIG_SUFFIX)
                self.assertIn(str(invocation.scratch/'report_progress.py'),str(invocation.transport.permissions.paths.notes))
                for mutated in (replace(invocation,argv=invocation.argv+('--always-approve',)),
                                replace(invocation,implement=True),replace(invocation,transport=None)):
                    with self.assertRaises(ExecutionError):supervisor.argv(value,mutated)
                (invocation.scratch/'report_progress.py').write_text('untrusted replacement')
                with self.assertRaises(ExecutionError):supervisor.argv(value,invocation)
            finally:
                self.assertTrue(invocation.native_auth.cleanup());invocation.native_auth.close()


FAKE_SERVER=r'''
import json,sys
sid="00000000-0000-4000-8000-000000000041"
def receive():return json.loads(sys.stdin.readline())
def send(x):
 data=json.dumps(x)+'\n'
 for part in (data[:5],data[5:]):sys.stdout.write(part);sys.stdout.flush()
def respond(request,result):send({'jsonrpc':'2.0','id':request['id'],'result':result})
a=receive();assert a['method']=='initialize';assert a['params']['clientCapabilities']['terminal']is False
respond(a,{'protocolVersion':1})
b=receive();assert b['params']=={'cwd':'/run/forge-coding-workspace','mcpServers':[]}
respond(b,{'sessionId':sid})
c=receive();assert c['method']=='session/prompt'
call={'sessionUpdate':'tool_call','toolCallId':'call-denied','rawInput':{'command':'git push'},'_meta':{'x.ai/tool':{'version':1,'namespace':'grok_build','name':'run_terminal_command'}}}
send({'jsonrpc':'2.0','method':'session/update','params':{'sessionId':sid,'update':call}})
send({'jsonrpc':'2.0','id':'ask-denied','method':'session/request_permission','params':{'sessionId':sid,'toolCall':call,'options':[{'optionId':'no-once','name':'No','kind':'reject_once'},{'optionId':'yes-once','name':'Yes','kind':'allow_once'}]}})
reply=receive();assert reply['result']['outcome']=={'outcome':'selected','optionId':'no-once'}
send({'jsonrpc':'2.0','method':'session/update','params':{'sessionId':sid,'update':{'sessionUpdate':'agent_message_chunk','content':{'type':'text','text':'Owned ACP result'}}}})
send({'jsonrpc':'2.0','method':'_x.ai/session/update','params':{'sessionId':sid,'update':{'sessionUpdate':'turn_completed','stop_reason':'end_turn','usage':{'inputTokens':85,'cachedReadTokens':70,'cacheCreationTokens':5,'outputTokens':15,'totalTokens':100}}}})
respond(c,{'stopReason':'end_turn','_meta':{'usage':{'inputTokens':85,'cachedReadTokens':70,'cacheCreationTokens':5,'outputTokens':15,'totalTokens':100}}})
assert sys.stdin.read()==''
'''
