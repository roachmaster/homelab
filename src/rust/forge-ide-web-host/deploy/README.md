# Access-controlled HTTPS deployment

This is a single-operator, read-only workbench. Browser pairing remains mandatory
in addition to the TLS proxy's access policy. Sessions last 30 minutes, expire on
restart, and are not durable user accounts. Pair within five minutes of startup;
restart only the web service for a new code. No remote shell, source editing or
background jobs are exposed.

```text
Browser → HTTPS + access policy → local tunnel/proxy → loopback web host
             public origin          original Host       session + CSRF
                                                          ↓ mTLS
                                                    loopback agent
                                                          ↓
                                               allowed source snapshot
```

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

For the web unit also use `RuntimeDirectory=forge-web`,
`RuntimeDirectoryMode=0700` and append
`--pairing-file /run/forge-web/pairing-code` to ExecStart. Do not preserve the
runtime directory across service stops. Read the code through a local authorized
administrative terminal; never store it in a journal, URL, repository, screenshot
or deployment receipt. Restarting the service invalidates the previous session.

## Verify and switch

1. Start the agent and candidate web service on unused loopback ports. Confirm
   listener ownership, restricted file modes and service startup.
2. Fetch every static asset using the configured Host and compare its hash with
   the staged bundle. Wrong Host/Origin, missing session and bad CSRF must fail.
3. Pair locally in a private test client with the configured HTTPS Origin, check
   the Secure/HttpOnly/SameSite cookie, resume, and inspect the real allowed
   snapshot through the actual mTLS agent. Keep session material only in memory.
4. Preserve the existing proxy/service configuration, then switch its origin to
   the already-healthy web service. Verify through that exact local proxy path.
   A TCP bridge preserves Host/Origin; an HTTP proxy must not rewrite them and
   must supply Content-Length for command bodies. Chunked requests are rejected.
5. Check public HTTPS still enforces the existing access policy. Perform a real
   authenticated browser journey when credentials are available; an Access login
   page alone does not prove the downstream authenticated flow. Record this gap
   explicitly when it cannot be exercised.
6. Restart only the web service after test pairing, leaving a fresh operator code.
   Enable the candidate units for boot only after checks pass. Save safe evidence:
   source revision/diff hash, binary/asset hashes, test results, service state,
   source snapshot and credential expiration. Never save session/token contents.

## Rollback

Keep the old origin running until the new route is verified. Restore the saved
proxy origin and restart only the bridge, then confirm its old response before
stopping the candidate. No DNS or tunnel credential change is needed when the
public tunnel still points at the same local bridge. For later Forge releases,
restore both the matching binary/assets release and the matching agent config;
restart the agent then web host, and obtain a new pairing code.

This recipe is operational guidance, not an automatic deployment target or
multi-user production certification. The applications still serve synchronously,
with bounded requests and volatile sessions. The local TLS proxy and host users
remain trusted; the application does not validate Cloudflare Access JWTs itself.
