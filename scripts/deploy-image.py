#!/usr/bin/env python3
"""Release a tested, immutable gateway image without rebuilding it (Python 3.11+)."""

import argparse
import datetime
import json
import re
import shlex
import shutil
import subprocess
import sys
import time
import tomllib
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
APP = "apiosk-gateway-v2"
REGISTRY = f"registry.fly.io/{APP}"
HEALTH_URL = f"https://{APP}.fly.dev/health"
FLY = "flyctl" if shutil.which("flyctl") else "fly"
# The one supported runtime transition removes the former local listener.
# No payment flag, database setting or secret may change in this pipeline.
RETIRED_EXECUTOR_ENV = {
    "EMBEDDED_EXECUTOR_ORIGIN": "http://127.0.0.1:8080",
    "EMBEDDED_EXECUTOR_PORT": "8080",
}


def run(*args, capture=True, timeout=60):
    result = subprocess.run(args, cwd=ROOT, check=True, text=True,
                            stdout=subprocess.PIPE if capture else None, timeout=timeout)
    return result.stdout.strip() if capture else None


def status():
    return json.loads(run(FLY, "status", "--app", APP, "--json"))


def image_digest(machine):
    reference = machine.get("image_ref", {})
    if reference.get("registry") != "registry.fly.io" or reference.get("repository") != APP:
        raise ValueError("Machine image is outside this app's registry; rollback needs manual review")
    value = reference.get("digest", "")
    if not re.fullmatch(r"sha256:[0-9a-f]{64}", value):
        raise ValueError(f"Machine {machine.get('id')} has no immutable image digest")
    return f"{REGISTRY}@{value}"


def snapshot(state):
    # Explicit allowlist: do not persist machine env, credentials or health output.
    return [{"id": machine["id"], "state": machine["state"],
             "image": image_digest(machine),
             "checks": [{"name": check.get("name"), "status": check.get("status")}
                        for check in machine.get("checks", [])]}
            for machine in state.get("Machines", [])]


def preflight(config, state):
    if config.get("app") != APP or state.get("Name") != APP:
        raise ValueError("Release target must be the existing apiosk-gateway-v2 app")
    if config.get("deploy", {}).get("release_command"):
        raise ValueError("This image-only pipeline does not run release commands or migrations")
    machines = state.get("Machines", [])
    if not machines or not any(m.get("state") == "started" for m in machines):
        raise ValueError("No running gateway Machine; inspect the app before releasing")
    desired = config.get("env", {})
    previous_listener_env = None
    for machine in machines:
        if machine.get("state") not in ("started", "stopped", "suspended"):
            raise ValueError("Another Machine transition is in progress")
        actual = {key: value for key, value in machine.get("config", {}).get("env", {}).items()
                  if not key.startswith("FLY_") and key != "PRIMARY_REGION"}
        listener_env = {key: actual[key] for key in RETIRED_EXECUTOR_ENV if key in actual}
        if previous_listener_env is not None and listener_env != previous_listener_env:
            raise ValueError("Retired executor environment differs across Machines; "
                             "rollback requires a consistent configuration")
        previous_listener_env = listener_env
        retired = {key for key, value in RETIRED_EXECUTOR_ENV.items()
                   if key not in desired and actual.get(key) == value}
        changed = sorted(key for key in desired.keys() | actual.keys()
                         if key not in retired and desired.get(key) != actual.get(key))
        if changed:
            raise ValueError("Runtime environment drift; resolve separately before releasing: "
                             + ", ".join(changed))
    previous = {image_digest(machine) for machine in machines}
    if len(previous) != 1:
        raise ValueError("Machines run different images; finish or recover the prior rollout first")
    return previous.pop()


def deploy_command(image):
    return [FLY, "deploy", "--app", APP, "--config", "fly.toml", "--image", image,
            "--strategy", "rolling", "--ha=false", "--wait-timeout", "5m",
            "--skip-release-command", "--yes"]


def rollback_command(image, config, state):
    command = deploy_command(image)
    actual = state["Machines"][0].get("config", {}).get("env", {})
    for key, value in RETIRED_EXECUTOR_ENV.items():
        if key not in config.get("env", {}) and actual.get(key) == value:
            command.extend(["--env", f"{key}={value}"])
    return command


def get_health():
    with urllib.request.urlopen(HEALTH_URL, timeout=10) as response:
        if response.status != 200:
            raise ValueError(f"Health HTTP status {response.status}")
        return json.load(response)


def verify_rollout(state, health, image, revision, expected_count):
    if not isinstance(health, dict):
        raise ValueError("Public health must return a JSON object")
    machines = state.get("Machines", [])
    if len(machines) != expected_count or not any(m.get("state") == "started" for m in machines):
        raise ValueError("Machine count changed or no running gateway Machine")
    for machine in machines:
        if image_digest(machine) != image:
            raise ValueError(f"Machine {machine.get('id')} is not running the requested image")
        if machine.get("state") not in ("started", "stopped", "suspended"):
            raise ValueError("A Machine is still transitioning")
        if machine.get("state") == "started":
            checks = machine.get("checks", [])
            if not checks or any(check.get("status") != "passing" for check in checks):
                raise ValueError(f"Machine {machine.get('id')} has not passed health checks")
    expected = {"service": APP, "protocol_version": "2",
                "payment_executor": "ready", "version": revision}
    if any(health.get(key) != value for key, value in expected.items()):
        raise ValueError("Public health does not confirm the revision and ready payment executor")


def wait_until_healthy(image, revision, expected_count, timeout=180):
    deadline = time.monotonic() + timeout
    last_error = "No health result"
    while time.monotonic() < deadline:
        try:
            state, health = status(), get_health()
            verify_rollout(state, health, image, revision, expected_count)
            return state, {key: health[key] for key in
                           ("service", "protocol_version", "payment_executor", "version")}
        except (ValueError, KeyError, OSError, urllib.error.URLError,
                subprocess.SubprocessError) as error:
            last_error = str(error)
            print(f"Waiting for rollout verification: {last_error}", file=sys.stderr)
            time.sleep(min(5, max(0, deadline - time.monotonic())))
    raise RuntimeError(f"Release verification failed: {last_error}")


def write_evidence(path, evidence):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(evidence, indent=2) + "\n")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", required=True, help="Fly registry image pinned by sha256 digest")
    parser.add_argument("--revision", required=True, help="Full 40-character tested Git revision")
    parser.add_argument("--evidence", type=Path, help="Release evidence JSON destination")
    parser.add_argument("--dry-run", action="store_true", help="Read-only preflight; never deploy")
    args = parser.parse_args(argv)
    if not re.fullmatch(re.escape(REGISTRY) + r"@sha256:[0-9a-f]{64}", args.image):
        parser.error("--image must be an immutable digest in this app's Fly registry")
    if not re.fullmatch(r"[0-9a-f]{40}", args.revision):
        parser.error("--revision must be a full lowercase Git commit SHA")
    evidence_path = args.evidence or ROOT / ".local" / "releases" / f"{args.revision}.json"
    evidence = {"app": APP, "revision": args.revision, "image": args.image,
                "started_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
                "status": "preflight"}
    try:
        if run("git", "rev-parse", "HEAD") != args.revision:
            raise ValueError("Checkout revision must match the tested image revision")
        run("git", "diff", "--exit-code", "HEAD", "--", "fly.toml")
        config = tomllib.loads((ROOT / "fly.toml").read_text())
        before = status()
        previous_image = preflight(config, before)
        evidence.update(previous_image=previous_image, before=snapshot(before),
                        rollback_command=shlex.join(rollback_command(previous_image, config, before)))
        print("Deploy:", shlex.join(deploy_command(args.image)))
        print("Rollback (restores previous listener settings when needed):", evidence["rollback_command"])
        if args.dry_run:
            evidence["status"] = "dry-run"
        else:
            evidence["status"] = "deploying"
            write_evidence(evidence_path, evidence)
            run(*deploy_command(args.image), capture=False, timeout=600)
            after, health = wait_until_healthy(args.image, args.revision, len(before["Machines"]))
            evidence.update(status="healthy", after=snapshot(after), health=health)
            print(f"Verified {args.revision}: every Machine image and gateway/executor health passed.")
        write_evidence(evidence_path, evidence)
        print(f"Release evidence: {evidence_path}")
        return 0
    except (ValueError, KeyError, OSError, RuntimeError, subprocess.SubprocessError) as error:
        evidence.update(status="failed", error=str(error))
        write_evidence(evidence_path, evidence)
        print(f"Release stopped: {error}\nEvidence: {evidence_path}", file=sys.stderr)
        if "rollback_command" in evidence:
            print("Inspect the app; rollback is not automatic:\n" + evidence["rollback_command"],
                  file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
