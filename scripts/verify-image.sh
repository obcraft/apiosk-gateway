#!/usr/bin/env bash
# Inspect the actual release image without credentials or network access.
set -euo pipefail

if (($# != 2)) || [[ ! "$2" =~ ^[0-9a-f]{40}$ ]]; then
  echo "Usage: bash scripts/verify-image.sh IMAGE FULL_GIT_SHA" >&2
  exit 2
fi
image=$1
revision=$2

metadata=$(docker image inspect "$image")
python3 -c '
import json, sys
image = json.load(sys.stdin)[0]
assert (image["Os"], image["Architecture"]) == ("linux", "amd64"), "Expected linux/amd64 image"
config = image["Config"]
assert config["User"] == "10001:10001", "Runtime must use the existing unprivileged UID/GID"
env = dict(item.split("=", 1) for item in config.get("Env", []))
assert env.get("GATEWAY_GIT_SHA") == sys.argv[1], "Image revision does not match the tested commit"
assert config.get("Cmd") == ["apiosk-gateway-v2-entrypoint"], "Unexpected runtime command"
assert not config.get("Entrypoint"), "Unexpected runtime entrypoint"
for name in env:
    assert not any(word in name for word in ("SECRET", "TOKEN", "PASSWORD", "PRIVATE_KEY", "DATABASE_URL", "API_KEY")), "Credential variable embedded in image: " + name
' "$revision" <<< "$metadata"

docker run --rm --platform linux/amd64 --network none --read-only --entrypoint /bin/bash "$image" -euc '
  test "$(id -u)" = 10001
  test "$(id -g)" = 10001
  for binary in apiosk-gateway-v2 check-brightdata-linkedin; do
    test -x "/usr/local/bin/$binary"
    dependencies=$(ldd "/usr/local/bin/$binary" 2>&1)
    case "$dependencies" in *"not found"*) echo "$dependencies" >&2; exit 1;; esac
  done
  test ! -e /usr/local/bin/apiosk-payment-executor
  test -s /etc/ssl/certs/ca-certificates.crt
  test -s /etc/ssl/certs/supabase-prod-ca-2021.crt
  command -v pdftotext >/dev/null
  bash -n /usr/local/bin/apiosk-gateway-v2-entrypoint
  for tool in cargo rustc; do
    if command -v "$tool" >/dev/null; then
      echo "Build tool leaked into runtime: $tool" >&2
      exit 1
    fi
  done
  for path in /build /executor /usr/local/cargo /usr/local/rustup /.env /app/.env; do
    test ! -e "$path"
  done
'

# The one server must fail before initialization when required configuration
# is absent. The container cannot reach a database/provider, even if that
# failure contract regresses.
python3 - "$image" <<'PY'
import subprocess
import sys
import uuid

name = "apiosk-image-check-" + uuid.uuid4().hex
try:
    result = subprocess.run(
        ["docker", "run", "--platform", "linux/amd64", "--name", name, "--rm",
         "--network", "none", "--read-only", sys.argv[1]],
        capture_output=True, text=True, timeout=30,
    )
    output = result.stdout + result.stderr
    if result.returncode != 1 or "missing DATABASE_URL" not in output:
        raise SystemExit(f"Unexpected unconfigured startup (exit {result.returncode}): {output}")
except subprocess.TimeoutExpired:
    raise SystemExit("Unconfigured startup did not terminate within 30 seconds")
finally:
    subprocess.run(["docker", "rm", "--force", name], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
PY
echo "Verified linux/amd64 runtime image for $revision; no services or paid requests started."
