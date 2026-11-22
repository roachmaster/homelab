#!/usr/bin/env python3
"""Preview-by-default CLI bridge translating verified Jenkins webhooks into Forge dispatch calls."""

import argparse
import json
import os
from pathlib import Path
import sys

from jenkins_codec import JenkinsCodecError, MAX_WEBHOOK_BYTES, translate_jenkins_webhook
from jenkins_config import load_jenkins_config
from jenkins_emitter import HttpDispatchJenkinsEmitter, JenkinsEmitterError

MAX_SECRET_BYTES = 4096


def _read_bounded(path: str, maximum: int, error_code: str) -> bytes:
    try:
        with Path(path).open("rb") as stream:
            data = stream.read(maximum + 1)
    except OSError:
        raise JenkinsCodecError(error_code, f"Unable to read {path}") from None
    if len(data) == 0 or len(data) > maximum:
        raise JenkinsCodecError(error_code, "Input file empty or exceeds size limit")
    return data


def _resolve_target(args):
    if args.config:
        cfg = load_jenkins_config(args.config)
        if args.project not in cfg.allowed_projects:
            raise JenkinsCodecError("denied", "Project not allowlisted in Jenkins config")
        instance_cfg = cfg.allowed_instances.get(args.instance)
        if instance_cfg is None:
            raise JenkinsCodecError("denied", "Instance not allowlisted in Jenkins config")
        return args.project, args.instance, instance_cfg.origin, instance_cfg.hmac_secret_env
    if not args.origin:
        raise JenkinsCodecError("invalid_configuration", "Either --config or --origin is required")
    return args.project, args.instance, args.origin, None


def _resolve_hmac_secret(args, config_secret_env: str | None) -> bytes | None:
    if args.hmac_secret_file:
        return _read_bounded(args.hmac_secret_file, MAX_SECRET_BYTES, "secret_file_unavailable").strip()
    env_name = args.hmac_secret_env or config_secret_env
    if env_name:
        raw = os.environ.get(env_name)
        if raw is None or not raw.strip():
            raise JenkinsCodecError("missing_hmac_secret", "Configured HMAC secret env var is unset")
        return raw.strip().encode("utf-8")
    return None


def main(argv=None, *, emitter_factory=HttpDispatchJenkinsEmitter) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--action", choices=("preview", "emit"), default="preview")
    parser.add_argument("--config", help="Absolute path to trusted Jenkins JSON configuration")
    parser.add_argument("--project", required=True, help="Trusted Forge project alias")
    parser.add_argument("--instance", required=True, help="Trusted Jenkins instance alias")
    parser.add_argument("--origin", help="Trusted Jenkins https:// origin when --config is omitted")
    parser.add_argument("--payload-file", required=True, help="Path to raw Jenkins webhook JSON body")
    parser.add_argument("--delivery-id", help="Optional caller/header delivery idempotency key")
    parser.add_argument("--signature", help="Optional X-Jenkins-Signature header value (sha256=<hex>)")
    parser.add_argument("--hmac-secret-file", help="Path to file containing shared HMAC secret")
    parser.add_argument("--hmac-secret-env", help="Env var name containing shared HMAC secret")
    parser.add_argument("--occurred-at", help="Optional ISO-8601 UTC timestamp override")
    parser.add_argument("--runner-host", default="127.0.0.1")
    parser.add_argument("--runner-port", type=int, default=18083)
    parser.add_argument("--token-file", help="Path to runner bearer token file (required for --action emit)")
    parser.add_argument("--request-id", help="Optional Forge-Request-Id correlation identifier")
    args = parser.parse_args(argv)

    try:
        project_alias, jenkins_instance, expected_origin, config_secret_env = _resolve_target(args)
        raw_body = _read_bounded(args.payload_file, MAX_WEBHOOK_BYTES, "payload_file_unavailable")
        hmac_secret = _resolve_hmac_secret(args, config_secret_env)
        if args.signature is not None and hmac_secret is None:
            raise JenkinsCodecError(
                "missing_hmac_secret",
                "Signature provided without configured HMAC secret",
            )
        event = translate_jenkins_webhook(
            raw_body,
            project_alias=project_alias,
            jenkins_instance=jenkins_instance,
            expected_origin=expected_origin,
            delivery_id=args.delivery_id,
            signature_header=args.signature,
            hmac_secret=hmac_secret,
            occurred_at=args.occurred_at,
        )
        if args.action == "preview":
            result = {
                "action": "preview",
                "destination": "ci.jenkins.event.emit",
                "payload_sha256": event.payload_sha256(),
                "event": event.semantic_payload(),
            }
        else:
            if not args.token_file:
                raise JenkinsEmitterError("token_file_required")
            token = _read_bounded(args.token_file, MAX_SECRET_BYTES, "token_file_unavailable").decode("utf-8").strip()
            emitter = emitter_factory(
                token=token,
                host=args.runner_host,
                port=args.runner_port,
            )
            receipt = emitter.emit(event, request_id=args.request_id)
            result = {"action": "emit", "receipt": receipt}
        print(json.dumps(result, ensure_ascii=False))
        return 0
    except JenkinsCodecError as error:
        print(json.dumps({"status": "error", "code": error.code}), file=sys.stderr)
        return 2
    except JenkinsEmitterError as error:
        payload = {"status": "error", "code": error.code}
        if error.status is not None:
            payload["http_status"] = error.status
        print(json.dumps(payload), file=sys.stderr)
        return 3
    except (ValueError, TypeError, UnicodeError):
        print(json.dumps({"status": "error", "code": "invalid_request"}), file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
