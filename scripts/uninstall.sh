#!/usr/bin/env bash
set -euo pipefail

[[ $EUID -eq 0 ]] || { echo 'Run with sudo sl-runner uninstall.' >&2; exit 1; }
purge=${1:-}
[[ -z $purge || $purge == --purge ]] || { echo "Unknown argument: $purge" >&2; exit 1; }

printf 'Stopping and disabling the runner service...\n'
systemctl stop sl-runner.service
systemctl disable sl-runner.service
rm -f -- /etc/systemd/system/sl-runner.service
systemctl daemon-reload

printf 'Removing the runner binary...\n'
# Preserve a command that has been replaced with a different installation.
if [[ -L /usr/local/bin/sl-runner && $(readlink /usr/local/bin/sl-runner) == /opt/sl-runner/sl-runner ]]; then
  rm -- /usr/local/bin/sl-runner
fi
rm -f -- /opt/sl-runner/sl-runner

if [[ $purge == --purge ]]; then
  printf 'Deleting runner state, registry credentials, and account...\n'
  rm -rf -- /var/lib/sl-runner /etc/sl-runner
  if id sl-runner >/dev/null 2>&1; then
    userdel sl-runner
  fi
  if getent group sl-runner >/dev/null; then
    groupdel sl-runner
  fi
else
  printf 'Saved state, registry credentials, and runner account were preserved.\n'
fi
printf 'Runner uninstalled. Docker, Swarm, Caddy, and deployed applications were preserved.\n'
