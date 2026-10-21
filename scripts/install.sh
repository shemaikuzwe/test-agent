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
DEBIAN_FRONTEND=noninteractive apt-get install -y ca-certificates jq libsqlite3-0
# Preserve an existing Docker installation; use Ubuntu packages on fresh VMs.
if ! command -v docker >/dev/null; then
  DEBIAN_FRONTEND=noninteractive apt-get install -y docker.io
else
  printf '%s  Docker is already installed; keeping it.%s\n' "$green" "$reset"
fi

step=3
printf '%s[3/9] Preparing runner files...%s\n' "$blue" "$reset"
staging=$(mktemp -d)
trap 'rm -rf -- "$staging"' EXIT
# Stage inputs before replacing installed files, including when upgrading in place.
install -m 0755 "$binary" "$staging/sl-runner"
install -m 0644 "$script_dir/../sl-runner.service" "$staging/sl-runner.service"
advertise_addr=$(ip -4 route get 1.1.1.1 | awk '{for (i=1; i<NF; i++) if ($i == "src") {print $(i+1); exit}}')
[[ -n $advertise_addr ]] || { echo 'Cannot determine VM IPv4 address.' >&2; exit 1; }
printf '%s  Detected VM address: %s%s\n' "$green" "$advertise_addr" "$reset"
# Docker validates the registry entry below; reject URLs and paths up front.
[[ $registry != *[[:space:]/@?#]* ]] || {
  echo 'Registry must be HOST[:PORT], without a scheme or path.' >&2; exit 1;
}
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
printf '%s[8/9] Installing the runner and database migrations...%s\n' "$blue" "$reset"
install -d -o root -g sl-runner -m 0750 /etc/sl-runner
install -d -o sl-runner -g sl-runner -m 0700 /etc/sl-runner/docker /var/lib/sl-runner
install -m 0755 "$staging/sl-runner" /opt/sl-runner/sl-runner
install -m 0644 "$staging/sl-runner.service" /etc/systemd/system/sl-runner.service
runuser -u sl-runner -- /opt/sl-runner/sl-runner migrate
step=9
printf '%s[9/9] Starting and checking the runner service...%s\n' "$blue" "$reset"
systemctl daemon-reload
systemctl enable --now sl-runner.service
systemctl is-active --quiet sl-runner.service
printf '\n%sRunner installed; service is active.%s\n' "$green" "$reset"
printf '%sLog in to Harbor before deploying private images:%s\n' "$yellow" "$reset"
printf 'sudo -u sl-runner docker --config /etc/sl-runner/docker login %q\n' "$registry"
