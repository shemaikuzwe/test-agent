#!/usr/bin/env bash
set -euo pipefail

[[ $EUID -eq 0 ]] || { echo 'Run with sudo sl-runner upgrade.' >&2; exit 1; }
version=${1:-latest}
repository='shemaikuzwe/test-agent'
installed=/opt/sl-runner/sl-runner
[[ -x $installed ]] || { echo 'Install sl-runner before upgrading.' >&2; exit 1; }
for dependency in curl tar sha256sum runuser systemctl; do
  command -v "$dependency" >/dev/null || { echo "Required command: $dependency" >&2; exit 1; }
done
systemctl cat sl-runner.service >/dev/null
case "$(uname -m)" in
  x86_64) architecture=amd64 ;;
  aarch64|arm64) architecture=arm64 ;;
  *) echo 'Supported architectures: amd64 and arm64.' >&2; exit 1 ;;
esac
if [[ $version == latest ]]; then
  release_url=$(curl -fsSL -o /dev/null -w '%{url_effective}' "https://github.com/$repository/releases/latest")
  version=${release_url##*/}
fi
[[ $version =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
  echo 'Version must use vMAJOR.MINOR.PATCH, or latest.' >&2; exit 1;
}
if [[ $("$installed" --version) == "sl-runner ${version#v}" ]]; then
  systemctl is-active --quiet sl-runner.service
  printf 'sl-runner %s is already installed; service is active.\n' "$version"
  exit 0
fi

staging=$(mktemp -d)
replacement=''
stopped=false
cleanup() {
  local status=$?
  rm -rf -- "$staging"
  [[ -z $replacement ]] || rm -f -- "$replacement"
  if (( status != 0 )) && [[ $stopped == true ]]; then
    echo 'Upgrade failed after stopping the daemon. Inspect the error before restarting sl-runner.service; database migrations may have been applied.' >&2
  fi
}
trap cleanup EXIT
archive="sl-runner-$version-linux-$architecture.tar.gz"
download_url="https://github.com/$repository/releases/download/$version"
printf 'Downloading sl-runner %s for Linux %s...\n' "$version" "$architecture"
curl -fsSL "$download_url/$archive" -o "$staging/$archive"
curl -fsSL "$download_url/SHA256SUMS" -o "$staging/SHA256SUMS"
(
  cd "$staging"
  awk -v archive="$archive" '$2 == archive || $2 == "./" archive' SHA256SUMS > archive.sha256
  [[ $(wc -l < archive.sha256) -eq 1 ]] || {
    echo 'Expected exactly one archive checksum.' >&2; exit 1;
  }
  sha256sum --check archive.sha256
  tar -xzf "$archive" sl-runner/sl-runner
)
binary=$staging/sl-runner/sl-runner
[[ $("$binary" --version) == "sl-runner ${version#v}" ]] || {
  echo 'Downloaded binary version does not match the requested release.' >&2; exit 1;
}
# Stage on the destination filesystem so the final rename is atomic.
replacement=$(mktemp /opt/sl-runner/.sl-runner-upgrade.XXXXXX)
install -m 0755 "$binary" "$replacement"
printf 'Stopping runner and waiting for its current deployment...\n'
systemctl stop sl-runner.service
stopped=true
printf 'Applying pending database migrations...\n'
runuser -u sl-runner -- "$replacement" migrate
mv -f -- "$replacement" "$installed"
replacement=''
systemctl start sl-runner.service
systemctl is-active --quiet sl-runner.service
stopped=false
printf 'Upgraded to sl-runner %s; service is active.\n' "$version"
