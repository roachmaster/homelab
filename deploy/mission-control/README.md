# Mission Control service lifecycle

Mission Control uses native systemd services. The [Forge IDE system inventory](../../docs/architecture/forge-ide-system-inventory.yaml) is the master list of services, TCP listeners, dependencies, and proposed components. Forge GPU continues to serve private SYCL inference.

## Inspect the deployment

Run from the Homelab checkout. The script uses Python 3 and the installed PyYAML module.

```bash
deploy/mission-control/services.py list
deploy/mission-control/services.py status
deploy/mission-control/services.py status forge-ide-web-host
```

`list` reads the inventory. `status` checks systemd and local TCP listeners on Mission Control.

## Install, update, or remove a service

Prepare a reviewed bundle containing the unit file and the drop-ins registered for that service. Put the release binaries, assets, and configuration at the paths referenced by the unit before running the script. For the Forge IDE web host, use this bundle layout.

```text
bundle/
  homelab-web.service
  homelab-web.service.d/
    40-chat-release.conf
```

Use the component ID from the inventory.

```bash
sudo deploy/mission-control/services.py install forge-ide-web-host /path/to/bundle
sudo deploy/mission-control/services.py update forge-ide-web-host /path/to/bundle
sudo deploy/mission-control/services.py delete forge-ide-web-host
```

- `install` installs the unit, enables it at boot, and starts it. Repeating the command with the same bundle keeps the running service in place.
- `update` verifies the bundle and restarts an active service when its unit files change. The script checks its registered TCP listeners after startup. On a reported failure, it attempts to restore the previous unit files and service state.
- `delete` stops and disables the unit and removes its registered unit files. Releases, application data, and other drop-ins stay on disk.

The initial managed units are the Forge IDE web host, its source agent, and the private inference tunnel. The web host requires the source agent, so removing the source agent also affects the IDE. Record a new service and its listener in the inventory before using the lifecycle script for it.

## PostgreSQL 16

Run these scripts on Mission Control. Setup uses Ubuntu packages to provision
the native `16/main` cluster, enables it at boot, and applies the Forge agent
migrations. Run setup again to apply newly registered migrations and verify the
installation. The cluster starts after the data mount becomes available.

```bash
sudo deploy/mission-control/setup-postgres.sh
sudo deploy/mission-control/setup-postgres.sh --check
sudo deploy/mission-control/backup-postgres.sh
```

The data directory is `/mnt/data/forge-homelab/postgres`. Clients connect through
the Unix socket at `/var/run/postgresql/.s.PGSQL.5432`. The agent database and
runtime role are both named `forge_agent`. The matching Linux account uses peer
authentication. The schema belongs to the separate `forge_agent_owner` role.

The backup script publishes a local logical backup under
`/mnt/data/forge-homelab/postgres-backups` after restoring it into a disposable
database and checking the schema. The archive contains the Forge agent database.
Setup recreates its cluster and role definitions. Copy backups off the data
volume for recovery from disk loss.

See the [SQL directory](sql/README.md) for migrations and management queries.

## Forge agent runner

The [runner deployment guide](agent-runner/README.md) covers the native service,
IDE connection, durable run status, and event replay API.

## Later provisioning

The eventual K3s work can use the same service inventory. Forge GPU inference can draft the provisioning files, and Mission Control can validate and stage them for deployment.
