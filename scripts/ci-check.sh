#!/usr/bin/env bash
# Offline tests only: opt-in live/model/database tests remain ignored.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

if (($#)); then
  echo "Usage: bash scripts/ci-check.sh" >&2
  exit 2
fi

if ! command -v pdftotext >/dev/null; then
  echo "Missing pdftotext; install poppler-utils (Linux) or poppler (macOS)." >&2
  exit 1
fi

# Replay suites support an explicit live mode and fixture rewriting for
# maintenance. CI always reads the checked-in tape, even in a developer shell.
export REPLAY_TAPE=strict
export REPLAY_BLESS=0
export REPLAY_RECORD=0

# Fetch is the only dependency-network phase. --locked prevents resolution
# drift; --frozen makes the subsequent test builds reuse those exact inputs.
cargo fetch --locked
result=0
echo "Testing the complete gateway workspace (live and database tests remain ignored)"
if ! cargo test --frozen --workspace --no-fail-fast; then
  result=1
fi

# The deploy helper's tests use mocked Fly/HTTP responses, never credentials.
python3 -m unittest discover -s scripts -p 'test_deploy_image.py' -v || result=1
exit "$result"
