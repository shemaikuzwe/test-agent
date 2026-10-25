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
loki_url=''
version=''
script_dir=''
install_dir=''
# Bash has no source path when the script is piped through stdin.
if [[ -n ${BASH_SOURCE[0]:-} ]]; then
  script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
  install_dir=$(dirname -- "$script_dir")
fi
while (($#)); do
  case "$1" in
    --version)
      [[ $# -ge 2 && -n $2 ]] || { echo 'Missing value for --version' >&2; exit 1; }
      version=$2
      shift 2 ;;
    --registry)
      [[ $# -ge 2 && -n $2 ]] || { echo 'Missing value for --registry' >&2; exit 1; }
      registry=$2
      shift 2 ;;
    --loki-url)
      [[ $# -ge 2 && -n $2 ]] || { echo 'Missing value for --loki-url' >&2; exit 1; }
      loki_url=$2
      [[ $loki_url =~ ^https?://[^[:space:]]+$ ]] || {
        echo 'Loki URL must be an HTTP or HTTPS push endpoint, without whitespace.' >&2; exit 1;
      }
      shift 2 ;;
    *) echo "Unknown argument: $1" >&2; exit 1 ;;
  esac
done

step=1
printf '%s[1/9] Checking installation inputs and host...%s\n' "$blue" "$reset"
[[ $EUID -eq 0 ]] || { echo 'Run as root.' >&2; exit 1; }
[[ -n $registry ]] || { echo 'Required argument: --registry HOST[:PORT]' >&2; exit 1; }
[[ -d /run/systemd/system ]] || { echo 'A running systemd is required.' >&2; exit 1; }
# shellcheck source=/dev/null
. /etc/os-release
[[ $ID == ubuntu ]] || { echo 'This installer supports Ubuntu.' >&2; exit 1; }
for dependency in docker dockerd; do
  command -v "$dependency" >/dev/null || {
    echo 'Docker Engine and its CLI must be installed before running this installer.' >&2; exit 1;
  }
done

staging=$(mktemp -d)
trap 'rm -rf -- "$staging"' EXIT
binary=$install_dir/sl-runner
[[ -f $binary ]] || binary=$install_dir/target/release/sl-runner
[[ -f $binary ]] || binary=/opt/sl-runner/sl-runner
if [[ -n $version || -z $install_dir || ! -f $binary || ! -f $install_dir/sl-runner.service || ! -f $install_dir/caddy-stack.yaml ]]; then
  repository='shemaikuzwe/test-agent'
  version=${version:-latest}
  case "$(uname -m)" in
    x86_64) architecture=amd64 ;;
    aarch64|arm64) architecture=arm64 ;;
    *) echo 'Supported architectures: amd64 and arm64.' >&2; exit 1 ;;
  esac
  for dependency in curl tar sha256sum; do
    command -v "$dependency" >/dev/null || { echo "Required command: $dependency" >&2; exit 1; }
  done
  if [[ $version == latest ]]; then
    release_url=$(curl -fsSL -o /dev/null -w '%{url_effective}' "https://github.com/$repository/releases/latest")
    version=${release_url##*/}
  fi
  [[ $version =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
    echo 'Version must use vMAJOR.MINOR.PATCH, or latest.' >&2; exit 1;
  }
  archive="sl-runner-$version-linux-$architecture.tar.gz"
  download_url="https://github.com/$repository/releases/download/$version"
  printf 'Downloading sl-runner %s for Linux %s...\n' "$version" "$architecture"
  curl -fsSL "$download_url/$archive" -o "$staging/$archive"
  curl -fsSL "$download_url/SHA256SUMS" -o "$staging/SHA256SUMS"
  mkdir "$staging/release"
  (
    cd "$staging"
    awk -v archive="$archive" '$2 == archive || $2 == "./" archive' SHA256SUMS > archive.sha256
    [[ $(wc -l < archive.sha256) -eq 1 ]] || {
      echo 'Expected exactly one archive checksum.' >&2; exit 1;
    }
    sha256sum --check archive.sha256
    tar -xzf "$archive" -C release
  )
  install_dir=$staging/release/sl-runner
  script_dir=$install_dir/scripts
  binary=$install_dir/sl-runner
fi
for file in "$binary" "$script_dir/../sl-runner.service" "$install_dir/caddy-stack.yaml"; do
  [[ -f $file && -r $file ]] || { echo "Cannot read $file" >&2; exit 1; }
done
if [[ -n $loki_url ]]; then
  for file in "$install_dir/alloy-stack.yaml" "$install_dir/runtime.alloy"; do
    [[ -f $file && -r $file ]] || { echo "Cannot read $file" >&2; exit 1; }
  done
fi

step=2
printf '%s[2/9] Installing runtime dependencies...%s\n' "$blue" "$reset"
apt-get update
DEBIAN_FRONTEND=noninteractive apt-get install -y ca-certificates jq libsqlite3-0

step=3
printf '%s[3/9] Preparing runner files...%s\n' "$blue" "$reset"
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

if [[ -n $loki_url ]]; then
  printf '%s  Installing Alloy runtime log collection...%s\n' "$blue" "$reset"
  install -m 0644 "$install_dir/alloy-stack.yaml" /opt/sl-runner/alloy-stack.yaml
  install -m 0644 "$install_dir/runtime.alloy" /opt/sl-runner/runtime.alloy
  LOKI_PUSH_URL="$loki_url" docker stack deploy --compose-file /opt/sl-runner/alloy-stack.yaml sl-logs
fi

step=8
printf '%s[8/9] Installing the runner and database migrations...%s\n' "$blue" "$reset"
install -d -o root -g sl-runner -m 0750 /etc/sl-runner
install -d -o sl-runner -g sl-runner -m 0700 /etc/sl-runner/docker /var/lib/sl-runner
install -m 0755 "$staging/sl-runner" /opt/sl-runner/sl-runner
install -d -m 0755 /usr/local/bin
ln -sfn /opt/sl-runner/sl-runner /usr/local/bin/sl-runner
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
