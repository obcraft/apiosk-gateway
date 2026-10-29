#!/bin/bash
set -euo pipefail

# Provider and payment handlers are linked into this server. exec makes the
# gateway PID 1 so Fly's signals reach it directly; no supervisor or sidecar.
exec /usr/local/bin/apiosk-gateway-v2 "$@"
