# sl-runner

A Rust runner for platform-managed Scriptylabs Nodes. One project owns a Node;
customers do not receive root access. One persistent systemd daemon processes
the local deployment queue. Hosting Service invokes `submit` to enqueue work with all
required deployment details. There is no public agent listener, backend long
polling, or permanent backend connection.

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

## Configuration

Local commands default to `config.json` in the current directory. Copy
`config.example.json` to `config.json` in the project root; this file is gitignored.
Systemd uses `/etc/sl-runner/config.json` explicitly. Supply a unique node
UUID, client certificate/private key, and the trusted hosting API CA bundle.
All configured paths must be absolute. `api_url` must be an HTTPS origin, with
no path, embedded credentials, query, or fragment.

Run `migrate` during installation and after upgrades. It creates the state directory
and applies only pending Diesel migrations. Normal `submit` and `daemon`
invocations do not run migrations. The state directory contains `runner.db` and must be writable by the
runner account.

## Proposed hosting API contract

This endpoint is a proposed contract for the future hosting backend, not
a route already implemented in `sl-api`. JSON uses camelCase. Requests use
HTTPS with client certificates.

Deployment details are supplied through `submit` and persisted in the local queue.
The image must be pinned to a SHA-256 digest and the node ID must match the
configured node. The daemon processes the stored details without fetching them
from the hosting API. Duplicate deployment IDs keep their original queued payload.

`PUT /internal/runner/deployments/{deploymentId}/status` accepts:

```json
{
  "nodeId": "00000000-0000-4000-8000-000000000001",
  "state": "deployed",
  "updatedAt": 1792576800
}
```

## Automated Ubuntu installation

Run `scripts/install.sh` as root inside the runner VM. Supply a Linux runner
binary compatible with the VM. Config and API certificates are optional for dev.
Copy `scripts/install.sh`, `sl-runner.service`, and `caddy-stack.yaml` together, preserving their
relative paths. No Rust toolchain is needed inside the VM.

For the local development runner at `10.43.251.5`, using Harbor over HTTP at
`10.43.251.159`:

```sh
sudo bash scripts/install.sh --registry 10.43.251.159
```

## systemd installation

Create a system account named `sl-runner` with a matching group and no login
shell. Install the release binary and daemon unit. Place the configured client
certificate, private key, and CA bundle in `/etc/sl-runner/certs`, readable by
the `sl-runner` account:

```sh
sudo install -d -m 0755 /opt/sl-runner
sudo install -m 0755 target/release/sl-runner /opt/sl-runner/sl-runner
sudo install -d -o root -g sl-runner -m 0750 /etc/sl-runner/certs
sudo install -o root -g sl-runner -m 0640 config.json /etc/sl-runner/config.json
sudo install -m 0644 sl-runner.service /etc/systemd/system/
sudo install -d -o sl-runner -g sl-runner -m 0700 /var/lib/sl-runner
sudo -u sl-runner /opt/sl-runner/sl-runner --config /etc/sl-runner/config.json migrate
sudo systemctl daemon-reload
sudo systemctl enable --now sl-runner.service
```


The daemon starts at boot and restarts on failure. It checks the local queue
and processes jobs sequentially. On shutdown, it lets the current job finish;
after a restart, it requeues interrupted jobs.

From an Incus client with access to the VM, enqueue a deployment:

```sh
incus exec <vm-name> -- sudo -u sl-runner /opt/sl-runner/sl-runner --config /etc/sl-runner/config.json submit \
  --deployment-id 00000000-0000-4000-8000-000000000002 \
  --node-id 00000000-0000-4000-8000-000000000001 \
  --application-id 00000000-0000-4000-8000-000000000003 \
  --image registry.example.com/project/app@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa \
  --hostname sl.local \
  --port 3000
```

A successful `submit` means the job is queued, not completed. The running daemon
picks it up and sends status updates to the hosting API.

```sh
systemctl status sl-runner.service
journalctl -u sl-runner.service -f
```

For upgrades, stop the daemon, install the new binary, apply migrations, then
start it again:

```sh
sudo systemctl stop sl-runner.service
sudo install -m 0755 target/release/sl-runner /opt/sl-runner/sl-runner
sudo -u sl-runner /opt/sl-runner/sl-runner --config /etc/sl-runner/config.json migrate
sudo systemctl start sl-runner.service
```

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
sudo -u sl-runner /opt/sl-runner/sl-runner --config /etc/sl-runner/config.json submit \
  --deployment-id "$(cat /proc/sys/kernel/random/uuid)" \
  --node-id "$(sudo jq -r .node_id /etc/sl-runner/config.json)" \
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

`scripts/test-submit.sh` uses `sl.local` and port 3000 with fixed application and
configured node IDs. Copy it to the VM and run it as root after upgrading.

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