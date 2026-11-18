# Forge Studio

Forge Studio accepts a build for an approved project and an exact Git revision.
The private API stores the receipt and state transitions in PostgreSQL. A
separate worker claims each attempt and publishes a digest-addressed preview.
The preview listener serves the recorded bytes on a separate HTTPS origin
using a signed link valid for 15 minutes.

The first profile is `static-web`. It packages a configured subtree of a
trusted local Git repository using the approved revision. The subtree must
already contain `index.html` and any authored or generated assets it loads.
Use relative asset URLs so requests stay inside the signed preview path. The
profile supports linked JavaScript, WebAssembly, CSS and common media and font
files. It accepts up to 256 files and a 16 MiB artifact. The worker reads Git
objects and assembles the archive without extracting repository files or
executing project build scripts.

Run the API, worker, and preview listener as separate local processes. Use
separate OS accounts for the worker and API so neither account can read the
other process's secret files. Give the worker write access and the preview
account group read access to the artifact directory. Configure the web host
to send only requests for projects it authorizes to the private Studio API.
Its configured preview origin must match the Studio HTTPS origin.

Apply `schema.sql` and `roles.sql` to the intended PostgreSQL database. The
second script defines two login-free capability roles. Grant
`forge_studio_api` to the private API's PostgreSQL login and
`forge_studio_worker` to the worker's login. The preview listener does not
connect to the database.

Supply a configuration file with the following fields. Paths and identifiers
are examples. Secret files contain the credential itself. Use 32 to 128
hexadecimal characters for the private API token. The database files
contain libpq connection strings for their respective logins.

```json
{
  "api_port": 18085,
  "preview_port": 18086,
  "api_database_file": "/etc/forge/studio-api.dsn",
  "worker_database_file": "/etc/forge/studio-worker.dsn",
  "api_token_file": "/etc/forge/studio-api.token",
  "preview_key_file": "/etc/forge/studio-preview.key",
  "preview_origin": "https://game-preview.example",
  "artifact_directory": "/var/lib/forge-studio/artifacts",
  "projects": [
    {
      "node": "mission-control",
      "project": "example",
      "repository": "/srv/approved/example",
      "revision": "0000000000000000000000000000000000000000",
      "directory": "web-preview"
    }
  ]
}
```

The API reads its database, bearer token and signing key. The worker reads
its database credential and project configuration. The preview process reads
the signing key and artifact directory. Each listener binds to loopback.

```sh
python3 -B deploy/mission-control/studio/studio.py api --config /etc/forge/studio.json
python3 -B deploy/mission-control/studio/studio.py worker --config /etc/forge/studio.json
python3 -B deploy/mission-control/studio/studio.py preview --config /etc/forge/studio.json
```

Studio accepts the versioned Forge envelope at the private
`POST /v1/dispatch` path. Supported destinations are project list and get,
build submit, get and cancel, and artifact get. A duplicate intent keeps its
original build ID. A new intent may select the revision in the current
catalog. Accepted jobs keep their original revision when the catalog
moves forward. A job whose project or source recipe is no longer configured
fails without releasing a preview.

A cancellation invalidates the attempt before returning its receipt. An
expired worker can make no later state update and may leave an unreferenced
artifact. The queue retries expired attempts up to three total claims.
The first implementation limits retained builds per project to 256 by
default, configurable with `max_builds_per_project`. Artifact retention
and reclamation are operational follow-up.

Run the disposable integration fixture with Git, PostgreSQL tools, and
`psycopg2` installed. It uses its own Unix socket and temporary database.

```sh
python3 -B -m unittest discover -s deploy/mission-control/studio -v
```

The suite can also exercise the Rust web host with the real Studio service.
Build the host's `loopback_fixture` example, the `forge-agent` binary, and
the browser bundle. Set `FORGE_STUDIO_HOST_FIXTURE`,
`FORGE_STUDIO_AGENT`, and `FORGE_STUDIO_ASSETS` to those paths before running
the same test command.
