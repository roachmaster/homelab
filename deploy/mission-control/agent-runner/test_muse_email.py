"""Owned fake protocol/process tests: no model or real Gmail calls."""
from contextlib import contextmanager, redirect_stderr, redirect_stdout
from dataclasses import asdict
import hashlib
import io
import json
import os
from pathlib import Path
import signal
import subprocess
from tempfile import TemporaryDirectory
import time
import unittest
from unittest.mock import patch
from uuid import uuid4

from muse_email import (CodexAppsSession, EmailIntent, EmailTransportError, GmailTransportConfig,
 GmailTransportFactory, SubmissionReceipt, SubmissionUncertain)
import muse_email_tool
SENDER='owner@example.test'
RECIPIENT='roachmaster@muse.ai'
CORRELATION='00000000-0000-4000-8000-000000000001'
THREAD='mailthread1'
MESSAGE='mail1'
def config(**kw):return GmailTransportConfig(expected_sender=SENDER,recipients=(RECIPIENT,),**kw)
def intent(body='Please acknowledge this test only.'):
 return EmailIntent('Connectivity',body,CORRELATION)
def result(data):return {'isError':False,'structuredContent':data}
def message(who, *, identifier='reply1',body='Received. <script>plain text</script>',subject=None):
 return {'id':identifier,'thread_id':THREAD,'payload':{'mime_type':'text/plain','headers':[
 {'name':'From','value':who},{'name':'To','value':SENDER if who==RECIPIENT else RECIPIENT},
 {'name':'Subject','value':subject or intent().sent_subject}],'body':{'content':body}}}
class Session:
 def __init__(self,responses):self.responses=responses;self.calls=[];self.closed=False
 def __enter__(self):return self
 def __exit__(self,*_):self.closed=True
 def call(self,tool,args):
  self.calls.append((tool,args))
  value=self.responses[tool]
  if isinstance(value,BaseException):raise value
  return value(args)if callable(value)else value
class TransportTest(unittest.TestCase):
 def make(self,**responses):
  session=Session({'gmail.get_profile':result({'email':SENDER}),**responses})
  return GmailTransportFactory(config(),session_factory=lambda _:session).create(),session
 def test_preview_factory_and_config_have_zero_effects(self):
  with patch('subprocess.Popen',side_effect=AssertionError('no process')),patch('pathlib.Path.open',side_effect=AssertionError('no file')):
   p=GmailTransportFactory(config()).create();v=p.preview(intent())
  self.assertEqual(v['status'],'dry_run');self.assertEqual(v['correlation_id'],CORRELATION)
  self.assertNotIn('body',v)
 def test_explicit_send_returns_real_ids_and_fixed_plain_payload_once(self):
  p,s=self.make(**{'gmail.send_email':result({'id':MESSAGE,'thread_id':THREAD})})
  receipt=p.submit(intent());self.assertEqual(receipt.status,'submitted');self.assertEqual(receipt.message_id,MESSAGE);self.assertTrue(s.closed)
  self.assertEqual([x[0]for x in s.calls],['gmail.get_profile','gmail.send_email'])
  args=s.calls[-1][1];self.assertEqual(args['to'],RECIPIENT);self.assertEqual(args['payload']['body']['content'],intent().sent_body)
  self.assertEqual(args['response_fields'],['id','thread_id']);self.assertNotIn('attachment_files',args)
 def test_wrong_sender_and_unallowlisted_recipient_stop_before_send(self):
  p,s=self.make(**{'gmail.get_profile':result({'email':'other@example.test'})})
  with self.assertRaises(EmailTransportError):p.submit(intent())
  self.assertEqual(len(s.calls),1)
  with self.assertRaises(EmailTransportError):p.preview(EmailIntent('x','body',CORRELATION,'other@example.test'))
 def test_uncertain_or_malformed_send_never_retries(self):
  for outcome in (EmailTransportError('native_timeout'),KeyboardInterrupt(),result({'id':MESSAGE}),{'isError':True}):
   p,s=self.make(**{'gmail.send_email':outcome})
   with self.assertRaises(SubmissionUncertain):p.submit(intent())
   self.assertEqual(sum(tool=='gmail.send_email'for tool,_ in s.calls),1)
 def test_exact_thread_sender_and_correlation_replies_are_untrusted(self):
  rows=[message(SENDER,identifier=MESSAGE),message(RECIPIENT),message('attacker@example.test',identifier='evil')]
  p,s=self.make(**{'gmail.read_email_thread':result({'id':THREAD,'messages':rows})})
  receipt=SubmissionReceipt(MESSAGE,THREAD,CORRELATION,SENDER,RECIPIENT)
  v=p.read_replies(receipt);self.assertEqual(len(v['replies']),1);self.assertTrue(v['replies'][0]['untrusted']);self.assertFalse(v['approval']);self.assertFalse(v['execution_authority'])
  self.assertEqual(s.calls[-1][1],{'thread_id':THREAD,'max_messages':20})
  p,s=self.make(**{'gmail.read_email_thread':result({'id':'wrong','messages':rows})})
  with self.assertRaises(EmailTransportError):p.read_replies(receipt)
 def test_html_is_text_only_and_script_never_becomes_content(self):
  row=message(RECIPIENT);row['payload']['mime_type']='text/html';row['payload']['body']={'content':'<p>Hello</p><script>steal()</script><img src="https://no-fetch.invalid"><b>Goodbye</b>'}
  rows=[message(SENDER,identifier=MESSAGE),row];p,s=self.make(**{'gmail.read_email_thread':result({'id':THREAD,'messages':rows})})
  v=p.read_replies(SubmissionReceipt(MESSAGE,THREAD,CORRELATION,SENDER,RECIPIENT));text=v['replies'][0]['body']
  self.assertIn('Hello',text);self.assertNotIn('steal',text);self.assertNotIn('<',text);self.assertNotIn('https://',text)
 def test_missing_sent_anchor_and_cross_sender_receipt_rejected(self):
  p,s=self.make(**{'gmail.read_email_thread':result({'id':THREAD,'messages':[message(RECIPIENT)]})})
  with self.assertRaises(EmailTransportError):p.read_replies(SubmissionReceipt(MESSAGE,THREAD,CORRELATION,SENDER,RECIPIENT))
  with self.assertRaises(EmailTransportError):p.read_replies(SubmissionReceipt(MESSAGE,THREAD,CORRELATION,'other@example.test',RECIPIENT))
 def test_reconciliation_uses_exact_marker_and_never_sends(self):
  sent=message(SENDER,identifier=MESSAGE,body=intent().sent_body)
  p,s=self.make(**{'gmail.search_email_ids':result({'messages':[{'id':MESSAGE}]}),'gmail.read_email_thread':result({'id':THREAD,'messages':[sent]})})
  v=p.reconcile(intent());self.assertEqual(v['status'],'submitted');self.assertTrue(v['reconciled']);self.assertEqual(v['receipt']['message_id'],MESSAGE)
  self.assertNotIn('gmail.send_email',[x[0]for x in s.calls]);self.assertIn('in:sent',s.calls[1][1]['query']);self.assertEqual(s.calls[1][1]['label_ids'],['SENT'])
  p,s=self.make(**{'gmail.search_email_ids':result({'messages':[]})});self.assertFalse(p.reconcile(intent())['automatic_resend'])
 def test_partial_sent_page_is_unresolved_not_unique(self):
  p,s=self.make(**{'gmail.search_email_ids':result({'messages':[],'next_page_token':'more'})})
  with self.assertRaises(EmailTransportError):p.reconcile(intent())

FAKE='''#!/usr/bin/python3
import json,sys,time,os
MODE=__MODE__
for line in sys.stdin:
 v=json.loads(line)
 if 'id'not in v:continue
 method=v['method'];p=v.get('params',{})
 if method=='initialize':r={}
 elif method=='thread/start':r={'thread':{'id':'00000000-0000-4000-8000-000000000002'}}
 elif method=='mcpServer/tool/call':
  tool=p['tool']
  if tool=='gmail.get_profile':r={'isError':False,'structuredContent':{'email':'owner@example.test'}}
  else:r={'isError':False,'structuredContent':{'id':'mail1','thread_id':'mailthread1'}}
 else:raise RuntimeError('Unexpected RPC')
 if MODE=='malformed':print('not json',flush=True);continue
 if MODE=='flood':print('x'*100000,flush=True);continue
 if MODE=='wrong_id':v['id']+=100
 if MODE=='boolean_id' and method=='initialize':v['id']=True
 if MODE=='float_id':v['id']=float(v['id'])
 print(json.dumps({'id':v['id'],'result':r}),flush=True)
 if MODE=='stall_input' and method=='mcpServer/tool/call':
  while True:time.sleep(1)
'''
class ConnectorShapeTest(unittest.TestCase):
 def test_verified_multipart_content_and_null_leaf_parts(self):
  import base64
  from muse_email import _body
  plain={'mime_type':'text/plain','filename':'','parts':None,'body':{'size':5,'base64_url_content':None,'content':'Hello','attachment_id':None}}
  html={'mime_type':'text/html','filename':'','parts':None,'body':{'size':12,'base64_url_content':None,'content':'<b>Hello</b>','attachment_id':None}}
  payload={'mime_type':'multipart/alternative','parts':[plain,html],'body':{'content':None,'base64_url_content':None}}
  self.assertEqual(_body(payload),'Hello')
  plain['body']={'base64_url_content':base64.urlsafe_b64encode(b'Encoded hello').decode().rstrip('='),'content':None}
  self.assertEqual(_body(plain),'Encoded hello')
  plain['filename']='untrusted.txt';self.assertEqual(_body(plain),'')
 def test_canonical_email_field_only_and_ambiguous_profiles_denied(self):
  for value in ({'email_address':SENDER},{'emailAddress':SENDER},{'email':SENDER,'email_address':SENDER}):
   session=Session({'gmail.get_profile':result(value)})
   p=GmailTransportFactory(config(),session_factory=lambda _:session).create()
   with self.assertRaises(EmailTransportError):p.profile()
 def test_ambiguous_encoded_content_is_rejected(self):
  from muse_email import _body
  with self.assertRaises(EmailTransportError):_body({'mime_type':'text/plain','body':{'content':'one','base64_url_content':'dHdv'}})

class NativeProtocolTest(unittest.TestCase):
 def run_fixture(self,mode,operation=lambda p:p.profile(),**kw):
  with TemporaryDirectory()as directory:
   path=Path(directory)/'codex-fixture';path.write_text(FAKE.replace('__MODE__',repr(mode)));path.chmod(0o700)
   c=config(binary=path,home=Path(directory),timeout_seconds=1,**kw);owned=[]
   def spawn(*a,**k):
    self.assertEqual(k['env']['CODEX_HOME'],str(Path(directory)/'.codex'));self.assertNotIn('OPENAI_API_KEY',k['env']);self.assertTrue(k['start_new_session'])
    p=subprocess.Popen(*a,**k);owned.append(p);return p
   factory=lambda c:CodexAppsSession(c,process_factory=spawn)
   try:
    with patch('muse_email.BINARY_SHA256',hashlib.sha256(path.read_bytes()).hexdigest()):
     return operation(GmailTransportFactory(c,session_factory=factory).create())
   finally:
    for p in owned:self.assertIsNotNone(p.returncode)
 def test_direct_profile_and_send_protocol_no_turn_or_inventory(self):
  self.assertEqual(self.run_fixture('normal')['status'],'authenticated')
  receipt=self.run_fixture('normal',lambda p:p.submit(intent()));self.assertEqual(receipt.thread_id,THREAD)
 def test_wrong_ids_malformed_output_and_limits_fail_closed(self):
  for mode in ('wrong_id','boolean_id','float_id','malformed','flood'):
   with self.subTest(mode=mode),self.assertRaises(EmailTransportError):self.run_fixture(mode,max_output_bytes=65536)
 def test_stalled_large_stdin_send_times_out_and_reaps(self):
  start=time.monotonic()
  with self.assertRaises(SubmissionUncertain):self.run_fixture('stall_input',lambda p:p.submit(intent('x'*65000)))
  self.assertLess(time.monotonic()-start,4)
 def test_arbitrary_rpc_tool_and_root_are_rejected(self):
  session=CodexAppsSession(config())
  with self.assertRaises(EmailTransportError):session.call('gmail.delete_emails',{})
  with self.assertRaises(EmailTransportError):session._rpc('turn/start',{})
  with patch('os.geteuid',return_value=0),self.assertRaises(EmailTransportError):session.__enter__()

class CliTest(unittest.TestCase):
 def test_default_is_preview_and_send_requires_explicit_action(self):
  with TemporaryDirectory()as directory:
   d=Path(directory);(d/'body').write_text('Hello');(d/'cfg').write_text(json.dumps({'expected_sender':SENDER,'recipients':[RECIPIENT]}))
   p=GmailTransportFactory(config(),session_factory=lambda _:(_ for _ in ()).throw(AssertionError('no send'))).create()
   class Factory:
    def __init__(self,c):pass
    def create(self):return p
   output=io.StringIO()
   with redirect_stdout(output):code=muse_email_tool.main(['--config',str(d/'cfg'),'--subject','test','--body-file',str(d/'body'),'--correlation-id',CORRELATION],factory=Factory)
   self.assertEqual(code,0);self.assertEqual(json.loads(output.getvalue())['status'],'dry_run')
