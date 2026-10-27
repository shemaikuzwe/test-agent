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
submit=("${runner[@]}" /opt/sl-runner/sl-runner submit \
  --deployment-id "$deployment_id" \
  --application-id "$application_id" \
  --image "$image" \
  --hostname sl.local \
  --port 3000 --env-stdin)

# Invalid input must fail without exposing the supplied value in either stream.
if output=$(printf '%s' '{"TOKEN":"secret-must-not-appear"' | "${submit[@]}" 2>&1); then
  printf 'Invalid environment input was accepted\n' >&2
  exit 1
fi
if [[ "$output" != *'"code":"INVALID_ARGUMENT"'* || "$output" == *secret-must-not-appear* ]]; then
  printf 'Invalid environment response was not safe\n' >&2
  exit 1
fi

"${submit[@]}" <<'JSON'
{"NODE_ENV":"production","EMPTY":"","GREETING":"hello\n\"$value=é\""}
JSON
