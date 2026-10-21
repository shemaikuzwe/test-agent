#!/usr/bin/env bash
set -euo pipefail

image=10.43.251.159/library/me@sha256:7af09b9e96856e36399145a6205f4cf6e42c05e8c2bfdd2404d40fa114075eee
# Keep the application ID fixed so repeated submissions update the same service.
application_id=00000000-0000-4000-8000-000000000002
deployment_id=$(cat /proc/sys/kernel/random/uuid)

printf 'Deployment: %s\nService: sl-%s_app\n' \
  "$deployment_id" "$application_id"
runner=()
if [[ $EUID -eq 0 ]]; then
  runner=(runuser -u sl-runner --)
fi
"${runner[@]}" /opt/sl-runner/sl-runner submit \
  --deployment-id "$deployment_id" \
  --application-id "$application_id" \
  --image "$image" \
  --hostname sl.local \
  --port 3000
