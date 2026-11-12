# Access-controlled HTTPS deployment

This is a single-operator IDE with chat and a read-only workbench. For an authenticated TLS proxy such
as Cloudflare Access, set `"session_auth": "trusted_proxy"` and the exact
`public_origin`. Chat opens automatically after proxy sign-in. The Workbench button opens the
source tools. Sessions last 30 minutes and are automatically
re-established on reload after expiry or restart. They are not durable or isolated
user accounts. No remote shell, source editing or background jobs are exposed.

```text
Browser → HTTPS + access policy → local tunnel/proxy → loopback web host
             public origin          original Host       session + CSRF
                                                          ↓ mTLS
                                                    loopback agent
                                                          ↓
                                               allowed source snapshot
```

For inference, add the [private chat relay configuration](../README.md#chat-relay)
to the web host and run the llama.cpp adapter on Forge GPU. If the services run
on different hosts, use a loopback-only SSH forward from the web host to the
adapter and keep its bearer token in an owner-only file outside served assets.

## Build and stage

Read the root and workspace READMEs. Use a current stable Rust toolchain with the
WASM target, fetch locked dependencies, then run workspace tests, `make ui-check`
and `make ui-qa`. Browser QA requires sandbox-enabled Chrome and the pinned npm
dependencies; localhost browser acceptance is separate from deployed HTTPS
verification. Source must remain unchanged during the final fingerprinted run.

Build `forge-agent` and `forge-ide-web-host` with Cargo `--release --locked`, and
copy the generated asset bundle **including its manifest** as one unit. The host
verifies the manifest; never regenerate individual live files. Keep immutable
release directories containing binaries, assets, source snapshot and hashes.
Copy only tracked source files into the inspection snapshot, excluding operational
configuration and credentials. Point the agent at the real snapshot directory:
the final project root must not be a symlink. Keep an old release for rollback.

## Configure

Provision a private CA and separate server/client certificates, DER-encoded X.509
and PKCS#8 keys, with serverAuth/clientAuth usages. The server certificate must
match its configured server name. Trust that CA only in the two applications;
there is no need to modify OS/browser trust stores. Record expiration and rotate
before expiry. Do not reuse the Cloudflare tunnel token for mTLS or browser auth.

Use the [agent configuration](../../forge-agent/README.md) with a loopback bind,
one peer leaf and explicit project aliases. Use the [web configuration](../README.md)
with its own loopback bind, matching node/incarnation and exact `public_origin`.
Restrict the peer and browser catalog to the same intended projects. Never expose
a home directory or filesystem root. Inspection can reveal source metadata and
is not secret redaction; only `.git` and `target` are excluded by default.

Run the two services under separate unprivileged system users. Root-owned
configuration directories should be readable only by the corresponding group;
private keys must not be readable by other services or users. Grant read-only
access to the release. Useful systemd settings include:

```ini
NoNewPrivileges=yes
PrivateTmp=yes
PrivateDevices=yes
ProtectSystem=strict
ProtectHome=yes
ProtectKernelTunables=yes
ProtectKernelModules=yes
ProtectControlGroups=yes
RestrictSUIDSGID=yes
RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX
CapabilityBoundingSet=
UMask=0077
Restart=on-failure
RestartSec=2
```

Trusted-proxy mode does not need `--pairing-file` or an operator pairing command.
The backend must remain loopback-only and the upstream proxy must enforce access
on every route, including `/forge/v1/dispatch`. Do not substitute client-supplied
identity headers for the proxy policy. The host trusts local processes; it does
not independently validate Access JWTs.

For local deployments that retain default pairing mode, use a private systemd
runtime directory and `--pairing-file` as described in the host README. Restarting
either mode invalidates the previous session.

## Verify and switch

1. Start the agent and candidate web service on unused loopback ports. Confirm
   listener ownership, restricted file modes and service startup.
2. Fetch every static asset using the configured Host and compare its hash with
   the staged bundle. Wrong Host/Origin, inspection without a session and bad CSRF
   must fail.
3. In trusted-proxy mode, send an initial Resume through the local proxy with
   the exact HTTPS Origin and no session cookie. Verify automatic admission and
   a Secure/HttpOnly/SameSite cookie, then inspect the allowed snapshot through
   the real mTLS agent. Inspect without a session or with bad CSRF must fail.
   Verify automatic browser connection with no Pair request or code entry.
   Keep session material only in memory.
4. Preserve the existing proxy/service configuration, then switch its origin to
   the already-healthy web service. Verify through that exact local proxy path.
   A TCP bridge preserves Host/Origin; an HTTP proxy must not rewrite them and
   must supply Content-Length for command bodies. Chunked requests are rejected.
5. Check public HTTPS still enforces the existing access policy. Perform a real
   authenticated browser journey when credentials are available; an Access login
   page alone does not prove the downstream authenticated flow. Record this gap
   explicitly when it cannot be exercised.
6. Restart the web service after verification to discard test sessions. Confirm
   a fresh Resume reconnects automatically without generating a pairing file.
   Enable the candidate units for boot only after checks pass. Save safe evidence:
   source revision/diff hash, binary/asset hashes, test results, service state,
   source snapshot and credential expiration. Never save session/token contents.

## Rollback

Keep the old origin running until the new route is verified. Restore the saved
proxy origin and restart only the bridge, then confirm its old response before
stopping the candidate. No DNS or tunnel credential change is needed when the
public tunnel still points at the same local bridge. For later Forge releases,
restore both the matching binary/assets release and the matching agent config;
restart the agent then web host. Trusted-proxy sessions reconnect on reload.

This recipe is operational guidance, not an automatic deployment target or
multi-user production certification. The applications still serve synchronously,
with bounded requests and volatile sessions. The local TLS proxy and host users
remain trusted; the application does not validate Cloudflare Access JWTs itself.
