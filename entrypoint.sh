#!/bin/bash
set -euo pipefail

children=()
shutdown() {
  if ((${#children[@]})); then
    kill -TERM "${children[@]}" 2>/dev/null || true
    wait "${children[@]}" 2>/dev/null || true
  fi
}
trap shutdown TERM INT EXIT

if [[ -z "${EXECUTOR_DATABASE_URL:-}" ]]; then
  echo "EXECUTOR_DATABASE_URL is required" >&2
  exit 1
fi

DATABASE_URL="$EXECUTOR_DATABASE_URL" \
  PORT="${EMBEDDED_EXECUTOR_PORT:-8080}" \
  /usr/local/bin/apiosk-payment-executor &
children+=("$!")

# Only two payment paths remain: a USDC payment from the treasury wallet (the
# executor above) and a balance update for ordinary API use. The research
# worker and the settlement reconciliation loop belonged to retired rails and
# are not part of this image.
/usr/local/bin/apiosk-gateway-v2 &
children+=("$!")

wait -n "${children[@]}"
exit 1
