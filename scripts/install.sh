#!/usr/bin/env bash
set -euo pipefail

blue='' green='' yellow='' red='' reset=''
if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then
  blue=$'\033[1;34m' green=$'\033[1;32m' yellow=$'\033[1;33m'
  red=$'\033[1;31m' reset=$'\033[0m'
fi

step=0
installation_failed() {
  local status=$?
  printf '%sInstallation failed at step %s/9 (exit %s).%s\n' "$red" "$step" "$status" "$reset" >&2
  exit "$status"
}
trap installation_failed ERR

registry=''
script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
install_dir=$(dirname -- "$script_dir")
while (($#)); do
  case "$1" in
    --registry)
      [[ $# -ge 2 && -n $2 ]] || { echo 'Missing value for --registry' >&2; exit 1; }
      registry=$2
      shift 2 ;;
    *) echo "Unknown argument: $1" >&2; exit 1 ;;
  esac
done

step=1
printf '%s[1/9] Checking installation inputs and host...%s\n' "$blue" "$reset"
[[ $EUID -eq 0 ]] || { echo 'Run as root.' >&2; exit 1; }
[[ -n $registry ]] || { echo 'Required argument: --registry HOST[:PORT]' >&2; exit 1; }
binary=$install_dir/sl-runner
[[ -f $binary ]] || binary=$install_dir/target/release/sl-runner
[[ -f $binary ]] || binary=/opt/sl-runner/sl-runner
config=$install_dir/config.json
[[ -f $config ]] || config=/etc/sl-runner/config.json
for file in "$binary" "$script_dir/../sl-runner.service" "$install_dir/caddy-stack.yaml"; do
  [[ -f $file && -r $file ]] || { echo "Cannot read $file" >&2; exit 1; }
done
[[ -d /run/systemd/system ]] || { echo 'A running systemd is required.' >&2; exit 1; }
# shellcheck source=/dev/null
. /etc/os-release
[[ $ID == ubuntu ]] || { echo 'This installer supports Ubuntu.' >&2; exit 1; }

step=2
printf '%s[2/9] Installing Docker and runtime dependencies...%s\n' "$blue" "$reset"
apt-get update
DEBIAN_FRONTEND=noninteractive apt-get install -y ca-certificates jq openssl libsqlite3-0
# Preserve an existing Docker installation; use Ubuntu packages on fresh VMs.
if ! command -v docker >/dev/null; then
  DEBIAN_FRONTEND=noninteractive apt-get install -y docker.io
else
  printf '%s  Docker is already installed; keeping it.%s\n' "$green" "$reset"
fi

step=3
printf '%s[3/9] Preparing configuration and development certificates...%s\n' "$blue" "$reset"
staging=$(mktemp -d)
trap 'rm -rf -- "$staging"' EXIT
# Stage inputs before replacing installed files, including when upgrading in place.
install -m 0755 "$binary" "$staging/sl-runner"
install -m 0644 "$script_dir/../sl-runner.service" "$staging/sl-runner.service"
if [[ ! -f $config ]]; then
  config=$staging/config.source.json
  jq -n --arg node_id "$(cat /proc/sys/kernel/random/uuid)" '{
    api_url: "https://127.0.0.1:8443/", node_id: $node_id,
    client_cert: "/etc/sl-runner/certs/client.crt",
    client_key: "/etc/sl-runner/certs/client.key",
    server_ca: "/etc/sl-runner/certs/ca.crt",
    state_dir: "/var/lib/sl-runner"
  }' > "$config"
  printf '%s  Generated config with a dummy API; status reporting will fail until configured.%s\n' "$yellow" "$reset"
fi

# Prefer bundled certificates, then the paths in the source configuration.
client_cert=$install_dir/certs/client.crt
client_key=$install_dir/certs/client.key
server_ca=$install_dir/certs/ca.crt
[[ -f $client_cert ]] || client_cert=$(jq -er '.client_cert' "$config")
[[ -f $client_key ]] || client_key=$(jq -er '.client_key' "$config")
[[ -f $server_ca ]] || server_ca=$(jq -er '.server_ca' "$config")
if [[ ! -e $client_cert && ! -e $client_key && ! -e $server_ca ]]; then
  printf '%s  Generating local dev certificates; real API trust must be configured later.%s\n' "$yellow" "$reset"
  node_id=$(jq -er '.node_id' "$config")
  openssl req -x509 -newkey rsa:2048 -noenc -days 365 \
    -subj '/CN=SL Runner Development CA' \
    -addext 'basicConstraints=critical,CA:TRUE' \
    -addext 'keyUsage=critical,keyCertSign,cRLSign' \
    -keyout "$staging/ca.key" -out "$staging/ca.crt" 2>"$staging/openssl.log"
  openssl req -new -newkey rsa:2048 -noenc -subj "/CN=$node_id" \
    -keyout "$staging/client.key" -out "$staging/client.csr" 2>>"$staging/openssl.log"
  printf 'basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=clientAuth\n' > "$staging/client.ext"
  openssl x509 -req -in "$staging/client.csr" -CA "$staging/ca.crt" \
    -CAkey "$staging/ca.key" -CAcreateserial -days 365 \
    -extfile "$staging/client.ext" -out "$staging/client.crt" 2>>"$staging/openssl.log"
else
  for file in "$client_cert" "$client_key" "$server_ca"; do
    [[ -f $file && -r $file ]] || { echo "Incomplete API certificates; cannot read $file" >&2; exit 1; }
  done
  install -m 0600 "$client_cert" "$staging/client.crt"
  install -m 0600 "$client_key" "$staging/client.key"
  install -m 0600 "$server_ca" "$staging/ca.crt"
fi
advertise_addr=$(ip -4 route get 1.1.1.1 | awk '{for (i=1; i<NF; i++) if ($i == "src") {print $(i+1); exit}}')
[[ -n $advertise_addr ]] || { echo 'Cannot determine VM IPv4 address.' >&2; exit 1; }
printf '%s  Detected VM address: %s%s\n' "$green" "$advertise_addr" "$reset"
# Docker validates the registry entry below; reject URLs and paths up front.
[[ $registry != *[[:space:]/@?#]* ]] || {
  echo 'Registry must be HOST[:PORT], without a scheme or path.' >&2; exit 1;
}
jq -e '
  if type != "object" then error("Runner config must be an object") else . end
  | if keys != ["api_url", "client_cert", "client_key", "node_id", "server_ca", "state_dir"]
    then error("Config must contain the fields in config.example.json") else . end
  | .client_cert = "/etc/sl-runner/certs/client.crt"
  | .client_key = "/etc/sl-runner/certs/client.key"
  | .server_ca = "/etc/sl-runner/certs/ca.crt"
  | .state_dir = "/var/lib/sl-runner"
' "$config" > "$staging/config.json"
# The runner validates api_url and node_id when migrations run.
daemon_config=/etc/docker/daemon.json
if [[ ! -f $daemon_config ]]; then
  printf '{}\n' > "$staging/daemon.empty.json"
  daemon_config=$staging/daemon.empty.json
fi
jq -e --arg registry "$registry" '
  if type != "object" then error("Docker config must be an object") else . end
  | if has("insecure-registries") then . else .["insecure-registries"] = [] end
  | if (.["insecure-registries"] | type) != "array"
    then error("Docker insecure-registries must be an array") else . end
  | if (.["insecure-registries"] | index($registry)) == null
    then .["insecure-registries"] += [$registry] else . end
' "$daemon_config" > "$staging/daemon.json"
"$staging/sl-runner" --version
# Validate the merged daemon configuration before replacing it.
dockerd --validate --config-file "$staging/daemon.json"

step=4
printf '%s[4/9] Preparing the runner account...%s\n' "$blue" "$reset"
getent group sl-runner >/dev/null || groupadd --system sl-runner
id sl-runner >/dev/null 2>&1 || useradd --system --gid sl-runner \
  --home-dir /var/lib/sl-runner --shell /usr/sbin/nologin sl-runner
getent group docker >/dev/null || groupadd --system docker
usermod -aG docker sl-runner
step=5
printf '%s[5/9] Configuring the HTTP registry and Docker...%s\n' "$blue" "$reset"
systemctl stop sl-runner.service 2>/dev/null || true
install -d -m 0755 /etc/docker /opt/sl-runner
if ! cmp -s "$staging/daemon.json" /etc/docker/daemon.json; then
  if [[ -f /etc/docker/daemon.json ]]; then
    cp -a /etc/docker/daemon.json /etc/docker/daemon.json.sl-runner.bak
  fi
  install -m 0644 "$staging/daemon.json" /etc/docker/daemon.json
  printf '%s  Docker configuration changed; restarting Docker.%s\n' "$yellow" "$reset"
  systemctl restart docker
fi
systemctl enable --now docker
step=6
printf '%s[6/9] Checking Swarm manager setup...%s\n' "$blue" "$reset"
swarm_state=$(docker info --format '{{.Swarm.LocalNodeState}}')
case "$swarm_state" in
  inactive) docker swarm init --advertise-addr "$advertise_addr" ;;
  active)
    printf '%s  Swarm is already active; checking manager role.%s\n' "$green" "$reset"
    [[ $(docker info --format '{{.Swarm.ControlAvailable}}') == true ]] || {
      echo 'Existing Swarm is a worker; a manager is required.' >&2; exit 1;
    } ;;
  *) echo "Cannot initialize Swarm in state: $swarm_state" >&2; exit 1 ;;
esac

step=7
printf '%s[7/9] Installing Caddy HTTP routing on port 80...%s\n' "$blue" "$reset"
if ! docker network inspect sl-proxy >/dev/null 2>&1; then
  docker network create --driver overlay sl-proxy
fi
install -m 0644 "$install_dir/caddy-stack.yaml" /opt/sl-runner/caddy-stack.yaml
docker stack deploy --compose-file /opt/sl-runner/caddy-stack.yaml sl-proxy

step=8
printf '%s[8/9] Installing the runner, certificates, and database migrations...%s\n' "$blue" "$reset"
install -d -o root -g sl-runner -m 0750 /etc/sl-runner /etc/sl-runner/certs
install -d -o sl-runner -g sl-runner -m 0700 /etc/sl-runner/docker /var/lib/sl-runner
install -m 0755 "$staging/sl-runner" /opt/sl-runner/sl-runner
install -o root -g sl-runner -m 0640 "$staging/config.json" /etc/sl-runner/config.json
for file in client.crt client.key ca.crt; do
  install -o root -g sl-runner -m 0640 "$staging/$file" "/etc/sl-runner/certs/$file"
done
install -m 0644 "$staging/sl-runner.service" /etc/systemd/system/sl-runner.service
runuser -u sl-runner -- /opt/sl-runner/sl-runner --config /etc/sl-runner/config.json migrate
step=9
printf '%s[9/9] Starting and checking the runner service...%s\n' "$blue" "$reset"
systemctl daemon-reload
systemctl enable --now sl-runner.service
systemctl is-active --quiet sl-runner.service
printf '\n%sRunner installed; service is active.%s\n' "$green" "$reset"
printf '%sLog in to Harbor before deploying private images:%s\n' "$yellow" "$reset"
printf 'sudo -u sl-runner docker --config /etc/sl-runner/docker login %q\n' "$registry"
