"""Model-free, config-owned Gmail port for explicit Muse email operations.

No database, worker registration, model turn, login, OAuth scrape, or retry loop.
Remote email is always untrusted data. The native account retains its credentials.
"""
from contextlib import contextmanager
from dataclasses import asdict, dataclass
import base64
from email.utils import getaddresses
import hashlib
from html.parser import HTMLParser
import json
import os
from pathlib import Path
import re
import selectors
import signal
import stat
import subprocess
import time
from typing import Protocol
from uuid import UUID

VERSION = '0.160.0'
BINARY = Path('/home/lrocha/.codex/packages/standalone/releases/0.160.0-x86_64-unknown-linux-musl/bin/codex')
BINARY_SHA256 = '12eb3e81114588aca3b7998f4f19e8997b056aca08e57a7ca7c8a3ec8c652aad'
SERVER = 'codex_apps'
TOOLS = frozenset({'gmail.get_profile', 'gmail.send_email', 'gmail.search_email_ids', 'gmail.read_email_thread'})
MAX_BODY = 64 * 1024
MAIL_ID = re.compile(r'[A-Za-z0-9_-]{1,128}')
ADDRESS = re.compile(r'[A-Za-z0-9_.+%-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}')


class EmailTransportError(Exception):
    """Sanitized error code; never contains native output, credentials or bodies."""
    def __init__(self, code='unavailable'):
        self.code = code
        super().__init__(code)


class SubmissionUncertain(EmailTransportError):
    def __init__(self, correlation_id):
        self.correlation_id = correlation_id
        super().__init__('submission_uncertain_reconcile_before_resend')


def _address(value):
    if not isinstance(value, str) or not ADDRESS.fullmatch(value):
        raise EmailTransportError('invalid_email_address')
    return value.lower()


def _uuid(value):
    try:
        if not isinstance(value, str) or str(UUID(value)) != value:
            raise ValueError()
    except (ValueError, TypeError, AttributeError):
        raise EmailTransportError('invalid_correlation_id') from None
    return value


def _identifier(value):
    if not isinstance(value, str) or not MAIL_ID.fullmatch(value):
        raise EmailTransportError('invalid_gmail_receipt')
    return value


def _text(value, limit, *, header=False):
    if (not isinstance(value, str) or not value.strip() or len(value.encode('utf-8')) > limit
            or any(ord(c) < 32 and (header or c not in '\n\r\t') or ord(c) == 127 for c in value)):
        raise EmailTransportError('invalid_email_content')
    return value


def _object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError('duplicate field')
        result[key] = value
    return result


def _json(raw):
    try:
        return json.loads(raw, object_pairs_hook=_object,
                          parse_constant=lambda _: (_ for _ in ()).throw(ValueError()))
    except (ValueError, UnicodeError, RecursionError):
        raise EmailTransportError('malformed_native_output') from None


@dataclass(frozen=True)
class GmailTransportConfig:
    expected_sender: str
    recipients: tuple[str, ...]
    binary: Path = BINARY
    home: Path = Path('/home/lrocha')
    timeout_seconds: float = 30
    max_output_bytes: int = 8 * 1024 * 1024

    def __post_init__(self):
        for path in (self.binary, self.home):
            if not isinstance(path, Path) or not path.is_absolute() or '..' in path.parts:
                raise EmailTransportError('invalid_transport_config')
        if (type(self.timeout_seconds) not in (int, float) or not 1 <= self.timeout_seconds <= 60
                or type(self.max_output_bytes) is not int or not 65536 <= self.max_output_bytes <= 16 * 1024 * 1024):
            raise EmailTransportError('invalid_transport_config')
        object.__setattr__(self, 'expected_sender', _address(self.expected_sender))
        if not isinstance(self.recipients, tuple) or not 1 <= len(self.recipients) <= 8:
            raise EmailTransportError('invalid_transport_config')
        recipients = tuple(_address(value) for value in self.recipients)
        if len(set(recipients)) != len(recipients):
            raise EmailTransportError('invalid_transport_config')
        object.__setattr__(self, 'recipients', recipients)


@dataclass(frozen=True)
class EmailIntent:
    subject: str
    body: str
    correlation_id: str
    recipient: str = 'roachmaster@muse.ai'

    def __post_init__(self):
        _text(self.subject, 200, header=True)
        _text(self.body, MAX_BODY - 128)
        _uuid(self.correlation_id)
        object.__setattr__(self, 'recipient', _address(self.recipient))

    @property
    def marker(self):
        return 'Forge-Muse:' + self.correlation_id

    @property
    def sent_subject(self):
        return self.subject + ' [' + self.marker + ']'

    @property
    def sent_body(self):
        return self.body.replace('\r\n', '\n') + '\n\nForge correlation: ' + self.correlation_id + '\n'


@dataclass(frozen=True)
class SubmissionReceipt:
    message_id: str
    thread_id: str
    correlation_id: str
    sender: str
    recipient: str
    status: str = 'submitted'

    def __post_init__(self):
        _identifier(self.message_id)
        _identifier(self.thread_id)
        _uuid(self.correlation_id)
        if self.status != 'submitted':
            raise EmailTransportError('invalid_gmail_receipt')
        object.__setattr__(self, 'sender', _address(self.sender))
        object.__setattr__(self, 'recipient', _address(self.recipient))


class GmailSession(Protocol):
    def call(self, tool: str, arguments: dict) -> dict: ...


class CodexAppsSession:
    """One owned native stdio process; exact metadata/tool RPC allowlist only."""
    def __init__(self, config, *, process_factory=subprocess.Popen, monotonic=time.monotonic,
                 signal_group=os.killpg, cleanup_delay=time.sleep):
        self.config, self._spawn, self._clock = config, process_factory, monotonic
        self._signal, self._delay = signal_group, cleanup_delay
        self.process = self.selected = None
        self._descriptor = None
        self._buffer = bytearray()
        self._seen = self._request_id = 0
        self.thread_id = None

    def __enter__(self):
        if os.geteuid() == 0:
            raise EmailTransportError('native_account_requires_nonroot')
        try:
            self._deadline = self._clock() + self.config.timeout_seconds
            self._descriptor = os.open(self.config.binary, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC | os.O_NONBLOCK)
            info = os.fstat(self._descriptor)
            if (not stat.S_ISREG(info.st_mode) or not info.st_mode & 0o111
                    or info.st_mode & 0o022 or not 0 < info.st_size <= 512 * 1024 * 1024):
                raise EmailTransportError('native_version_mismatch')
            digest = hashlib.sha256()
            seen = 0
            while chunk := os.read(self._descriptor, 1024 * 1024):
                seen += len(chunk)
                if seen > 512 * 1024 * 1024:
                    raise EmailTransportError('native_version_mismatch')
                if self._clock() >= self._deadline:
                    raise EmailTransportError('native_timeout')
                digest.update(chunk)
            if digest.hexdigest() != BINARY_SHA256:
                raise EmailTransportError('native_version_mismatch')
            os.lseek(self._descriptor, 0, os.SEEK_SET)
            environment = {'HOME': str(self.config.home), 'CODEX_HOME': str(self.config.home / '.codex'),
                           'PATH': '/usr/local/bin:/usr/bin:/bin', 'LANG': 'C.UTF-8', 'LC_ALL': 'C.UTF-8'}
            self.process = self._spawn(
                [str(self.config.binary), '--no-daemon', 'app-server', '--stdio', '-c', 'analytics.enabled=false'],
                executable=f'/proc/self/fd/{self._descriptor}', pass_fds=(self._descriptor,),
                stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                close_fds=True, start_new_session=True, env=environment, cwd='/tmp')
            self.selected = selectors.DefaultSelector()
            self.selected.register(self.process.stdout, selectors.EVENT_READ, 'stdout')
            self.selected.register(self.process.stderr, selectors.EVENT_READ, 'stderr')
            self._rpc('initialize', {'clientInfo': {'name': 'forge_muse_email', 'version': '1'},
                      'capabilities': {'experimentalApi': True, 'explicitGatewayOauth': True}})
            self._write({'method': 'initialized'})
            started = self._rpc('thread/start', {'ephemeral': True, 'cwd': '/tmp',
                'approvalPolicy': 'never', 'approvalsReviewer': 'user', 'sandbox': 'read-only',
                'environments': [], 'experimentalRawEvents': False, 'allowProviderModelFallback': False})
            thread = started.get('thread')
            if not isinstance(thread,dict):
                raise EmailTransportError('malformed_native_output')
            self.thread_id = _uuid(thread.get('id'))
            return self
        except BaseException:
            self.__exit__(None, None, None)
            raise

    def _write(self, value):
        pending = memoryview(json.dumps(value, separators=(',', ':')).encode() + b'\n')
        registered = False
        try:
            os.set_blocking(self.process.stdin.fileno(), False)
            self.selected.register(self.process.stdin, selectors.EVENT_WRITE, 'stdin')
            registered = True
            while pending:
                remaining = self._deadline - self._clock()
                if remaining <= 0:
                    raise EmailTransportError('native_timeout')
                ready = self.selected.select(remaining)
                if not ready:
                    raise EmailTransportError('native_timeout')
                for key, _ in ready:
                    if key.data == 'stdin':
                        try:
                            pending = pending[os.write(key.fd, pending):]
                        except BlockingIOError:
                            continue
                    else:
                        self._drain(key)
        except (OSError, ValueError):
            raise EmailTransportError('native_transport_closed') from None
        finally:
            if registered:
                self.selected.unregister(self.process.stdin)

    def _drain(self, key):
        try:
            chunk = os.read(key.fd, 65536)
        except BlockingIOError:
            return
        self._seen += len(chunk)
        if self._seen > self.config.max_output_bytes:
            raise EmailTransportError('native_output_limit')
        if not chunk:
            self.selected.unregister(key.fileobj)
            if key.data == 'stdout':
                raise EmailTransportError('native_transport_closed')
        elif key.data == 'stdout':
            self._buffer.extend(chunk)
        # Stderr is drained and discarded, never logged or forwarded.

    def _read(self):
        while True:
            position = self._buffer.find(b'\n')
            if position >= 0:
                line = bytes(self._buffer[:position])
                del self._buffer[:position + 1]
                if line.strip():
                    value = _json(line)
                    if not isinstance(value, dict):
                        raise EmailTransportError('malformed_native_output')
                    return value
                continue
            remaining = self._deadline - self._clock()
            if remaining <= 0:
                raise EmailTransportError('native_timeout')
            ready = self.selected.select(remaining)
            if not ready:
                raise EmailTransportError('native_timeout')
            for key, _ in ready:
                self._drain(key)

    def _rpc(self, method, params):
        if method not in {'initialize', 'thread/start', 'mcpServer/tool/call'}:
            raise EmailTransportError('rpc_not_allowed')
        self._request_id += 1
        request_id = self._request_id
        self._write({'id': request_id, 'method': method, 'params': params})
        while True:
            value = self._read()
            if 'method' in value:
                if 'id' in value:
                    # No native request may trigger approvals, login, commands,
                    # model turns or another tool through this transport.
                    self._write({'id': value['id'], 'error': {'code': -32601,
                        'message': 'This email transport does not perform native client actions'}})
                continue
            if value.get('id') != request_id:
                raise EmailTransportError('native_response_identity_mismatch')
            if 'error' in value:
                raise EmailTransportError('native_rpc_failed')
            if not isinstance(value.get('result'), dict):
                raise EmailTransportError('malformed_native_output')
            return value['result']

    def call(self, tool, arguments):
        if tool not in TOOLS or not isinstance(arguments, dict):
            raise EmailTransportError('tool_not_allowed')
        return self._rpc('mcpServer/tool/call', {'threadId': self.thread_id, 'server': SERVER,
                         'tool': tool, 'arguments': arguments})

    def __exit__(self, *_):
        failed = False
        if self.process is not None:
            try:
                try:
                    self.process.stdin.close()
                except OSError:
                    pass
                # Do not reap the leader before both group signals: its PID
                # reserves this owned group identity against PID reuse.
                if (type(self.process.pid) is not int or self.process.pid <= 1
                        or self.process.pid in (os.getpid(), os.getpgrp())):
                    raise OSError('invalid owned process identity')
                for signum in (signal.SIGTERM, signal.SIGKILL):
                    try:
                        self._signal(self.process.pid, signum)
                    except ProcessLookupError:
                        pass
                    if signum == signal.SIGTERM:
                        self._delay(.05)
                self.process.wait(timeout=2)
            except (OSError, subprocess.SubprocessError):
                failed = True
            finally:
                for stream in (self.process.stdin, self.process.stdout, self.process.stderr):
                    try:
                        stream.close()
                    except OSError:
                        pass
                self.process = None
        if self.selected is not None:
            self.selected.close()
            self.selected = None
        if self._descriptor is not None:
            os.close(self._descriptor)
            self._descriptor = None
        if failed:
            raise EmailTransportError('native_cleanup_unverified')


def _data(result):
    if not isinstance(result, dict) or result.get('isError') is not False:
        raise EmailTransportError('gmail_tool_failed')
    value = result.get('structuredContent')
    if value is None:
        blocks = result.get('content')
        if not isinstance(blocks, list) or len(blocks) != 1 or not isinstance(blocks[0],dict) or blocks[0].get('type') != 'text':
            raise EmailTransportError('malformed_gmail_result')
        value = _json(blocks[0].get('text', ''))
    if not isinstance(value, dict):
        raise EmailTransportError('malformed_gmail_result')
    return value


def _headers(message):
    payload = message.get('payload')
    headers = payload.get('headers') if isinstance(payload, dict) else None
    if not isinstance(headers, list) or len(headers) > 128:
        raise EmailTransportError('malformed_email_headers')
    selected = {}
    for item in headers:
        if not isinstance(item, dict) or not isinstance(item.get('name'), str) or not isinstance(item.get('value'), str):
            raise EmailTransportError('malformed_email_headers')
        name = item['name'].lower()
        if name in {'from', 'to', 'subject'}:
            if name in selected or len(item['value'].encode()) > 4096:
                raise EmailTransportError('malformed_email_headers')
            selected[name] = item['value']
    return selected


def _one_sender(value):
    addresses = getaddresses([value])
    if len(addresses) != 1:
        raise EmailTransportError('malformed_email_sender')
    return _address(addresses[0][1])


class _PlainHTML(HTMLParser):
    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.hidden = 0
        self.parts = []
    def handle_starttag(self, tag, attrs):
        if tag in {'script', 'style', 'head', 'template'}: self.hidden += 1
        if not self.hidden and tag in {'p', 'br', 'div', 'li'}: self.parts.append('\n')
    def handle_endtag(self, tag):
        if tag in {'script', 'style', 'head', 'template'} and self.hidden: self.hidden -= 1
    def handle_data(self, data):
        if not self.hidden: self.parts.append(data)


def _body(payload):
    plain, html = [], []
    budget = [0, 0]
    def visit(part, depth):
        if not isinstance(part, dict) or depth > 8:
            raise EmailTransportError('email_content_limit')
        budget[0] += 1
        if budget[0] > 64: raise EmailTransportError('email_content_limit')
        if part.get('filename') or part.get('content_disposition') == 'attachment':
            return
        mime = part.get('mime_type')
        body = part.get('body')
        if mime in {'text/plain', 'text/html'} and isinstance(body, dict):
            content = body.get('content')
            encoded = body.get('base64_url_content')
            if content is not None and encoded is not None:
                raise EmailTransportError('ambiguous_email_content')
            if content is None and isinstance(encoded, str):
                try:
                    raw = encoded;content = base64.b64decode(raw + '='*((-len(raw))%4), altchars=b'-_', validate=True).decode('utf-8')
                except (ValueError, UnicodeError):
                    raise EmailTransportError('malformed_email_content') from None
            if content is not None:
                if not isinstance(content, str): raise EmailTransportError('malformed_email_content')
                budget[1] += len(content.encode())
                if budget[1] > MAX_BODY: raise EmailTransportError('email_content_limit')
                (plain if mime == 'text/plain' else html).append(content)
        parts = part.get('parts')
        if parts is None: parts = []
        if not isinstance(parts, list): raise EmailTransportError('malformed_email_content')
        for child in parts: visit(child, depth + 1)
    visit(payload, 0)
    text = '\n'.join(plain)
    if not text and html:
        parser = _PlainHTML();parser.feed('\n'.join(html));parser.close();text = ''.join(parser.parts)
    return ''.join(c for c in text.replace('\r\n','\n') if c in '\n\t' or ord(c) >= 32 and ord(c) != 127).strip()


class GmailTransport:
    """Injected high-level email port. There is no public arbitrary tool call."""
    def __init__(self, config, session_factory):
        self.config, self._sessions = config, session_factory

    def _intent(self, intent):
        if not isinstance(intent, EmailIntent) or intent.recipient not in self.config.recipients:
            raise EmailTransportError('recipient_not_allowed')

    def preview(self, intent):
        self._intent(intent)
        return {'status':'dry_run','sender':self.config.expected_sender,'recipient':intent.recipient,
                'subject':intent.sent_subject,'correlation_id':intent.correlation_id,
                'body_bytes':len(intent.sent_body.encode()),'body_sha256':hashlib.sha256(intent.sent_body.encode()).hexdigest()}

    def _authenticate(self, session):
        profile = _data(session.call('gmail.get_profile', {}))
        # Native structured Gmail profile has one canonical `email` field.
        # Alias-only or ambiguous objects are not accepted as account proof.
        if ("email_address" in profile or "emailAddress" in profile
                or _address(profile.get("email")) != self.config.expected_sender):
            raise EmailTransportError('authenticated_sender_mismatch')
        return self.config.expected_sender

    def profile(self):
        with self._sessions(self.config) as session:
            return {'status':'authenticated','sender':self._authenticate(session)}

    def submit(self, intent):
        self._intent(intent)
        sent = False
        try:
            with self._sessions(self.config) as session:
                self._authenticate(session)
                sent = True
                result = _data(session.call('gmail.send_email', {
                    'to':intent.recipient,'from_address':self.config.expected_sender,
                    'subject':intent.sent_subject,
                    'payload':{'mime_type':'text/plain','charset':'UTF-8','body':{'content':intent.sent_body}},
                    'response_fields':['id','thread_id']}))
                receipt = SubmissionReceipt(result.get('id'),result.get('thread_id'),intent.correlation_id,
                                            self.config.expected_sender,intent.recipient)
            return receipt
        except BaseException:
            if sent: raise SubmissionUncertain(intent.correlation_id) from None
            raise

    def _receipt(self, receipt):
        if (not isinstance(receipt, SubmissionReceipt) or receipt.sender != self.config.expected_sender
                or receipt.recipient not in self.config.recipients):
            raise EmailTransportError('receipt_scope_mismatch')

    @staticmethod
    def _messages(result, thread_id=None):
        data = _data(result)
        actual = _identifier(data.get('id', data.get('thread_id')))
        if thread_id is not None and actual != thread_id:
            raise EmailTransportError('gmail_thread_mismatch')
        rows = data.get('messages')
        if not isinstance(rows,list) or len(rows)>20:
            raise EmailTransportError('email_content_limit')
        for row in rows:
            if (not isinstance(row,dict) or _identifier(row.get('thread_id',actual)) != actual):
                raise EmailTransportError('gmail_thread_mismatch')
            _identifier(row.get('id'))
        return actual, rows

    def read_replies(self, receipt):
        self._receipt(receipt)
        with self._sessions(self.config) as session:
            self._authenticate(session)
            _, rows = self._messages(session.call('gmail.read_email_thread', {'thread_id':receipt.thread_id,'max_messages':20}),receipt.thread_id)
            anchors = [row for row in rows if row['id'] == receipt.message_id]
            if len(anchors) != 1:
                raise EmailTransportError('sent_anchor_missing_or_truncated')
            anchor = _headers(anchors[0])
            if (_one_sender(anchor.get('from','')) != receipt.sender
                    or {_address(address) for _,address in getaddresses([anchor.get('to','')])} != {receipt.recipient}
                    or 'Forge-Muse:'+receipt.correlation_id not in anchor.get('subject','')):
                raise EmailTransportError('sent_anchor_scope_mismatch')
            replies = []
            for row in rows:
                headers = _headers(row)
                sender = _one_sender(headers.get('from',''))
                if sender != receipt.recipient or row['id'] == receipt.message_id:
                    continue
                recipients = {_address(address) for _,address in getaddresses([headers.get('to','')])}
                if self.config.expected_sender not in recipients:
                    raise EmailTransportError('reply_recipient_mismatch')
                if 'Forge-Muse:'+receipt.correlation_id not in headers.get('subject',''):
                    raise EmailTransportError('reply_correlation_mismatch')
                replies.append({'message_id':row['id'],'thread_id':receipt.thread_id,'sender':sender,
                                'body':_body(row['payload']),'untrusted':True,'correlation_id':receipt.correlation_id})
            return {'status':'replies_read','thread_id':receipt.thread_id,'replies':replies,
                    'approval':False,'execution_authority':False}

    def reconcile(self, intent):
        self._intent(intent)
        with self._sessions(self.config) as session:
            self._authenticate(session)
            found = _data(session.call('gmail.search_email_ids', {'query':
                'in:sent from:'+self.config.expected_sender+' to:'+intent.recipient+' subject:"'+intent.marker+'"',
                'label_ids':['SENT'],'max_results':10}))
            entries = found.get('messages',found.get('emails',found.get('ids')))
            if found.get('next_page_token') or found.get('nextPageToken') or isinstance(entries,list) and len(entries)>=10:
                raise EmailTransportError('sent_search_incomplete_do_not_resend')
            if not isinstance(entries,list):
                raise EmailTransportError('malformed_gmail_search')
            matches = []
            for entry in entries:
                message_id = _identifier(entry.get('id') if isinstance(entry,dict) else entry)
                thread_id, rows = self._messages(session.call('gmail.read_email_thread', {'message_id':message_id,'max_messages':20}))
                for row in rows:
                    if row['id'] != message_id: continue
                    headers = _headers(row)
                    if (_one_sender(headers.get('from','')) == self.config.expected_sender
                            and {_address(address) for _,address in getaddresses([headers.get('to','')])} == {intent.recipient}
                            and headers.get('subject') == intent.sent_subject
                            and _body(row['payload']) == intent.sent_body.strip()):
                        matches.append(SubmissionReceipt(message_id,thread_id,intent.correlation_id,self.config.expected_sender,intent.recipient))
            if len(matches)>1: raise EmailTransportError('ambiguous_sent_matches_do_not_resend')
            if matches: return {'status':'submitted','reconciled':True,'receipt':asdict(matches[0])}
            return {'status':'not_found','correlation_id':intent.correlation_id,'automatic_resend':False}


class GmailTransportFactory:
    """Effect-free selection; each operation acquires/releases its own session."""
    def __init__(self, config, *, session_factory=CodexAppsSession):
        if not isinstance(config,GmailTransportConfig) or not callable(session_factory):
            raise TypeError('Gmail transport requires typed configuration and session factory')
        self.config, self._session_factory = config, session_factory
    def create(self):
        return GmailTransport(self.config,self._session_factory)
