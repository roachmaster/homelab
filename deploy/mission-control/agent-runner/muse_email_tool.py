#!/usr/bin/env python3
"""Explicit local Muse-email tool. Preview is the default; no background worker."""
import argparse
from dataclasses import asdict
import json
import os
from pathlib import Path
import sys

from muse_email import (EmailIntent, EmailTransportError, GmailTransportConfig,
                        GmailTransportFactory, SubmissionReceipt, SubmissionUncertain, _json)


def _read(path, maximum):
    try:
        with Path(path).open('rb') as stream:
            data = stream.read(maximum + 1)
        if len(data) > maximum:
            raise EmailTransportError('input_file_too_large')
        return data
    except OSError:
        raise EmailTransportError('input_file_unavailable') from None


def load_config(path):
    if path is None:
        raise EmailTransportError('transport_config_required')
    value = _json(_read(path, 32768))
    fields = {'binary','home','expected_sender','recipients','timeout_seconds','max_output_bytes'}
    if not isinstance(value,dict) or not value.keys() <= fields:
        raise EmailTransportError('invalid_transport_config')
    for key in ('binary','home'):
        if key in value:
            if not isinstance(value[key],str): raise EmailTransportError('invalid_transport_config')
            value[key] = Path(value[key])
    if 'recipients' in value:
        if not isinstance(value['recipients'],list): raise EmailTransportError('invalid_transport_config')
        value['recipients'] = tuple(value['recipients'])
    return GmailTransportConfig(**value)


def main(argv=None, *, factory=GmailTransportFactory):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--action',choices=('preview','profile','send','reconcile','read-replies'),default='preview')
    parser.add_argument('--config',help='Trusted transport configuration; never read from email')
    parser.add_argument('--subject')
    parser.add_argument('--body-file')
    parser.add_argument('--correlation-id',help='Canonical UUID used to reconcile uncertain sends')
    parser.add_argument('--recipient',default='roachmaster@muse.ai')
    parser.add_argument('--receipt-file',help='Exact submitted receipt for read-replies')
    args = parser.parse_args(argv)
    try:
        transport = factory(load_config(args.config)).create()
        if args.action == 'profile':
            result = transport.profile()
        elif args.action == 'read-replies':
            if not args.receipt_file: raise EmailTransportError('receipt_file_required')
            value = _json(_read(args.receipt_file, 8192))
            if not isinstance(value,dict) or value.keys() != SubmissionReceipt.__dataclass_fields__.keys():
                raise EmailTransportError('invalid_gmail_receipt')
            result = transport.read_replies(SubmissionReceipt(**value))
        else:
            if not args.subject or not args.body_file or not args.correlation_id:
                raise EmailTransportError('subject_body_file_and_correlation_id_required')
            try: body = _read(args.body_file, 65536).decode('utf-8')
            except UnicodeError: raise EmailTransportError('invalid_email_content') from None
            intent = EmailIntent(args.subject,body,args.correlation_id,args.recipient)
            if args.action == 'preview': result = transport.preview(intent)
            elif args.action == 'send': result = asdict(transport.submit(intent))
            else: result = transport.reconcile(intent)
        print(json.dumps(result,ensure_ascii=False))
        return 0
    except SubmissionUncertain as error:
        print(json.dumps({'status':'uncertain','code':error.code,'correlation_id':error.correlation_id,
                          'automatic_retry':False}),file=sys.stderr)
        return 3
    except EmailTransportError as error:
        print(json.dumps({'status':'error','code':error.code}),file=sys.stderr)
        return 2
    except (OSError,ValueError,TypeError,KeyError):
        print(json.dumps({'status':'error','code':'email_transport_unavailable'}),file=sys.stderr)
        return 2


if __name__ == '__main__':
    raise SystemExit(main())
