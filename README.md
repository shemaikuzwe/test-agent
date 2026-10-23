# sl-runner

A Rust runner for platform-managed Scriptylabs Nodes. One project owns a Node;one persistent systemd daemon processes the local deployment queue. Hosting Service invokes `submit` to enqueue work with all
required deployment details.

## Build prerequisites

Diesel requires the system SQLite development library. On Ubuntu/Debian, install
it before building or running with Cargo:

```sh
sudo apt install libsqlite3-dev
```

If linking fails with `unable to find library -lsqlite3`, install this package
and retry the Cargo command.

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --locked --release
```

## Installation

Docker Engine and its CLI must already be installed on the Ubuntu 24.04 or newer
VM. The installer checks for both and does not install Docker.

Install the published release with:

```sh
curl -fsSL https://github.com/shemaikuzwe/test-agent/releases/latest/download/install.sh | sudo bash -s -- --registry 10.43.251.159
```

The current installer configures an HTTP registry and HTTP application routing.
Log in as the runner afterward to pull private Harbor images (see below).


For the local development runner 

```sh
sudo bash scripts/install.sh --registry 10.43.251.159
```

The daemon starts at boot and restarts on failure. It checks the local queue
and processes jobs sequentially. On shutdown, it lets the current job finish;
after a restart, it requeues interrupted jobs.

From an Incus client with access to the VM, enqueue a deployment:

```sh
incus exec <vm-name> -- sudo -u sl-runner sl-runner submit \
  --deployment-id 00000000-0000-4000-8000-000000000002 \
  --application-id 00000000-0000-4000-8000-000000000003 \
  --image registry.example.com/project/app@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa \
  --hostname sl.local \
  --port 3000
```

```sh
systemctl status sl-runner.service
journalctl -u sl-runner.service -f
```

Upgrade to the latest published release, or select a specific version:

```sh
sudo sl-runner upgrade
sudo sl-runner upgrade --version v0.2.0
```

Check whether the installed runner daemon is running (no root required):

```sh
sl-runner health
```

Remove the runner service and binary:

```sh
sudo sl-runner uninstall
```

The database, registry credentials, and runner account are preserved for
reinstallation. To also permanently delete those:

```sh
sudo sl-runner uninstall --purge
```

Both commands stop and disable the daemon, remove its systemd unit and installed
binary, and remove the CLI symlink if it points to that binary. Docker, Swarm,
Caddy, and deployed applications remain running. Purging deletes
`/var/lib/sl-runner`, `/etc/sl-runner`, and the `sl-runner` user and group.

## Swarm execution

Install Docker Engine and its CLI in the project-owned VM before starting the
runner. Initialize an independent single-node Swarm during node provisioning:

```sh
sudo docker swarm init --advertise-addr <vm-ip>
```
## HTTP application routing

The installer deploys one Caddy Docker Proxy service per VM, publishing only
port 80. It creates the external `sl-proxy` overlay network shared by the proxy
and routed apps. Docker socket access lets the proxy discover Swarm service
labels and refresh routes. Proxy configuration is in `caddy-stack.yaml`.

Supply both `--hostname` and `--port` to expose an application over HTTP:

```sh
sudo -u sl-runner sl-runner submit \
  --deployment-id "$(cat /proc/sys/kernel/random/uuid)" \
  --application-id 00000000-0000-4000-8000-000000000002 \
  --image 10.43.251.159/library/me@sha256:7af09b9e96856e36399145a6205f4cf6e42c05e8c2bfdd2404d40fa114075eee \
  --hostname sl.local \
  --port 3000
```

`--port` is the application's internal listening port, not a published VM port.
The app must listen on all container interfaces. Hostname and port are stored in
the queue and become `deploy.labels` for Caddy. Both arguments are required for
all submissions.
Duplicate deployment IDs retain their
original routing settings, so use a new deployment ID when changing routes.

`scripts/test-submit.sh` uses `sl.local` and port 3000 with a fixed application ID. Copy it to the VM and run it as root after upgrading.

For browser testing, add this entry to your PC's `/etc/hosts`:

```text
10.43.251.5 sl.local
```

Then open `http://sl.local`. A DNS-independent check from your PC is:

```sh
curl --noproxy '*' --resolve sl.local:80:10.43.251.5 http://sl.local
```

## Private registry authentication

Log in on the SL-NODE as the runner, using a Harbor project robot account with only
Pull Repository permission:

```sh
sudo -u sl-runner docker --config /etc/sl-runner/docker login 10.43.251.159
```

## Application environment variables

Add repeated `--env KEY=VALUE` arguments to `submit`:

```sh
sl-runner submit ... --env NODE_ENV=production --env 'TOKEN=value=with$dollars' --env EMPTY=
```

## Rollback

Restore a successful deployment from this runner's local history:

```sh
sl-runner rollback --deployment-id <previous-successful-deployment-id>
```

### Publishing new version

```sh

git tag v0.1.0
git push origin v0.1.0

```

## Programmatic command responses

Commands write exactly one JSON result followed by a newline to stdout. Diagnostic
logs go to stderr and retain their tracing format. Parse stdout separately rather
than combining both streams. `daemon` writes its result when it exits; its startup
and operational messages are logs.

Every result has `timestamp` (UTC RFC 3339), `command`, `status`, and `data`.
`status` is `SUCCESS` or `ERROR` and describes the command outcome. Deployment
`state` is separately one of `pending`, `running`, `deployed`, or `failed`.
A successful `status` lookup can therefore return a failed deployment.

Exit codes are `0` for success, `1` for execution/output failures, and `2` for
invalid arguments. `--help` and `--version` remain text and exit successfully.
Argument errors use `command: null` when no recognized command was supplied and
`data: {}`. Their messages omit supplied values, which may contain secrets.

Example successful wait:

```json
{
  "timestamp": "2026-10-22T13:04:47.687650Z",
  "command": "wait",
  "status": "SUCCESS",
  "data": {
    "message": "deployment deployed",
    "deploymentId": "bdd03f0a-55e4-4c9e-96cc-4f47d458ee99",
    "state": "deployed"
  }
}
```

Errors add an `error` object with a stable `code` and informational `message`:

```json
{
  "timestamp": "2026-10-22T13:04:47.687650Z",
  "command": "wait",
  "status": "ERROR",
  "data": {
    "deploymentId": "bdd03f0a-55e4-4c9e-96cc-4f47d458ee99",
    "state": "failed",
    "message": "deployment failed"
  },
  "error": {
    "code": "DEPLOYMENT_FAILED",
    "message": "recorded deployment failure reason"
  }
}
```

| Error code | Meaning |
| --- | --- |
| `INVALID_ARGUMENT` | Invalid CLI arguments or deployment input |
| `DEPLOYMENT_NOT_FOUND` | Deployment ID is absent from local history |
| `DEPLOYMENT_FAILED` | `wait` observed a failed deployment |
| `INVALID_ROLLBACK_TARGET` | Target deployment was not successful |
| `APPLICATION_NOT_FOUND` | Application service is absent from Swarm |
| `DATABASE_ERROR` | State directory, SQLite, or migration operation failed |
| `DOCKER_ERROR` | Docker execution or inspection failed |
| `INTERNAL_ERROR` | Unexpected runner failure |
