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
# worker (with its payment-hold reconciliation every pass) and the settlement
# reconciliation loop belong to retired rails and stay off unless enabled.
if [[ "${RESEARCH_WORKER_DISABLED:-true}" != "true" ]]; then
  DATABASE_URL="$EXECUTOR_DATABASE_URL" \
    PORT="${EMBEDDED_RESEARCH_WORKER_PORT:-8081}" \
    RESEARCH_WORKER_ENABLED=1 \
    /usr/local/bin/apiosk-payment-executor &
  children+=("$!")
fi

if [[ "${RECONCILIATION_DISABLED:-true}" != "true" ]]; then
  (
    while true; do
      DATABASE_URL="$EXECUTOR_DATABASE_URL" \
        /usr/local/bin/apiosk-reconciliation-worker || true
      sleep "${RECONCILIATION_POLL_SECONDS:-300}"
    done
  ) &
  children+=("$!")
fi

/usr/local/bin/apiosk-gateway-v2 &
children+=("$!")

wait -n "${children[@]}"
exit 1
