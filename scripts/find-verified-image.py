#!/usr/bin/env python3
"""Select an existing, successful main CI image; never start a build or deploy."""

import argparse
from datetime import datetime, timezone
import json
import os
import re
import subprocess
import sys
import time
from urllib.parse import urlencode

WORKFLOW = ".github/workflows/gateway.yml"
ACTIVE = {"queued", "in_progress", "waiting", "pending", "requested"}
REQUIRED_STEPS = {
    "Rust checks": {"Run isolated checks", "Verify native runtime with the local database fixture"},
    "Linux image": {"Verify the built image", "Upload the verified image"},
}


class SelectionError(ValueError):
    pass


def positive_id(value, label):
    if type(value) is not int or value <= 0:
        raise SelectionError(f"Invalid {label}; refusing unverified CI metadata.")
    return value


def validate_arguments(repo, revision):
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9-]*/[A-Za-z0-9_][A-Za-z0-9_.-]*", repo):
        raise SelectionError("Repository must be OWNER/REPO.")
    if not re.fullmatch(r"[0-9a-f]{40}", revision):
        raise SelectionError("Revision must be the full 40-character lowercase commit SHA.")


def gh_api(endpoint):
    try:
        result = subprocess.run(
            ["gh", "api", "--hostname", "github.com", "-H", "Accept: application/vnd.github+json",
             "-H", "X-GitHub-Api-Version: 2022-11-28", endpoint],
            check=True, text=True, capture_output=True, timeout=30,
        )
        response = json.loads(result.stdout)
        if not isinstance(response, dict):
            raise SelectionError("GitHub API returned invalid metadata; retry the release.")
        return response
    except (OSError, subprocess.SubprocessError, json.JSONDecodeError) as error:
        raise SelectionError(
            "GitHub API lookup failed; check GH_TOKEN, Actions read access, and GitHub availability."
        ) from error


def pages(api, endpoint, key):
    items = []
    separator = "&" if "?" in endpoint else "?"
    for page in range(1, 11):
        response = api(f"{endpoint}{separator}per_page=100&page={page}")
        if not isinstance(response, dict):
            raise SelectionError(f"Invalid GitHub {key} metadata.")
        batch, total = response.get(key), response.get("total_count")
        if not isinstance(batch, list) or type(total) is not int or not 0 <= total <= 1000:
            raise SelectionError(f"Invalid or incomplete GitHub {key} pagination.")
        if not all(isinstance(item, dict) for item in batch):
            raise SelectionError(f"Invalid GitHub {key} metadata.")
        items.extend(batch)
        if len(items) > total:
            raise SelectionError(f"Inconsistent GitHub {key} pagination; retry the release.")
        if len(items) >= total:
            return items
        if not batch:
            break
    raise SelectionError(f"Incomplete GitHub {key} pagination; retry the release.")


def ensure_main(api, base, revision):
    current = api(f"{base}/git/ref/heads/main")
    if current.get("ref") != "refs/heads/main" or current.get("object", {}).get("sha") != revision:
        raise SelectionError("Requested revision is no longer main. Release the current main revision.")


def validate_run(run, repo, repo_id, workflow_id, revision):
    for key in ("repository", "head_repository"):
        identity = run.get(key)
        if not isinstance(identity, dict):
            identity = {}
        if identity.get("id") != repo_id or str(identity.get("full_name", "")).lower() != repo.lower():
            raise SelectionError(f"CI {key} does not match {repo}; fork or repository mismatch.")
    path = run.get("path", "")
    if not isinstance(path, str) or path not in {WORKFLOW, WORKFLOW + "@main", WORKFLOW + "@refs/heads/main"}:
        raise SelectionError("CI workflow path does not match gateway.yml on main.")
    if run.get("workflow_id") != workflow_id or run.get("head_sha") != revision:
        raise SelectionError("CI workflow identity or full head SHA does not match.")
    if run.get("head_branch") != "main" or run.get("event") not in {"push", "workflow_dispatch"}:
        raise SelectionError("CI must be a main push or workflow_dispatch, never a pull request.")
    for field in ("id", "run_number", "run_attempt"):
        positive_id(run.get(field), f"CI {field}")


def latest_run(api, base, repo, repo_id, workflow_id, revision):
    runs = pages(api, f"{base}/actions/workflows/gateway.yml/runs?{urlencode({'head_sha': revision})}", "workflow_runs")
    # Other branches and PR merge checks cannot authorize production. Do not
    # filter by success: a later failure must invalidate an older green run.
    eligible = [run for run in runs if run.get("head_sha") == revision
                and run.get("head_branch") == "main"
                and run.get("event") in {"push", "workflow_dispatch"}]
    if not eligible:
        raise SelectionError("No main CI run exists for this revision. Run Gateway CI on main first.")
    for run in eligible:
        validate_run(run, repo, repo_id, workflow_id, revision)
    return max(eligible, key=lambda run: (run["run_number"], run["id"], run["run_attempt"]))


def verify_jobs(api, base, run):
    jobs = pages(api, f"{base}/actions/runs/{run['id']}/attempts/{run['run_attempt']}/jobs", "jobs")
    for name, steps in REQUIRED_STEPS.items():
        matches = [job for job in jobs if job.get("name") == name]
        if len(matches) != 1 or matches[0].get("status") != "completed" or matches[0].get("conclusion") != "success":
            raise SelectionError(f"Required CI job '{name}' did not succeed in this attempt. Rerun Gateway CI.")
        job = matches[0]
        if job.get("run_id") != run["id"] or job.get("head_sha") != run["head_sha"]:
            raise SelectionError("CI job provenance does not match the selected run.")
        for step in steps:
            matches = [entry for entry in job.get("steps", []) if entry.get("name") == step]
            if len(matches) != 1 or matches[0].get("status") != "completed" or matches[0].get("conclusion") != "success":
                raise SelectionError(f"Required CI step '{step}' did not succeed. Rerun Gateway CI.")


def find_artifact(api, base, run, repo_id, revision, now):
    name = f"gateway-image-{revision}"
    artifacts = pages(api, f"{base}/actions/runs/{run['id']}/artifacts", "artifacts")
    matches = [artifact for artifact in artifacts if artifact.get("name") == name]
    if len(matches) != 1:
        raise SelectionError("Verified image artifact is missing or ambiguous. Rerun Gateway CI for this revision.")
    artifact = matches[0]
    positive_id(artifact.get("id"), "artifact ID")
    try:
        expiry = datetime.fromisoformat(artifact["expires_at"].replace("Z", "+00:00"))
    except (AttributeError, KeyError, TypeError, ValueError) as error:
        raise SelectionError("Invalid artifact expiry; rerun Gateway CI.") from error
    if artifact.get("expired") is not False or expiry.tzinfo is None or expiry <= now():
        raise SelectionError("Verified image artifact expired. Rerun Gateway CI for this revision.")
    provenance = artifact.get("workflow_run") or {}
    expected = {"id": run["id"], "repository_id": repo_id, "head_repository_id": repo_id,
                "head_branch": "main", "head_sha": revision}
    if any(provenance.get(key) != value for key, value in expected.items()):
        raise SelectionError("Image artifact provenance does not match the selected main CI run.")
    return artifact


def select(repo, revision, wait_seconds=0, poll_seconds=15, *, api=gh_api,
           monotonic=time.monotonic, sleep=time.sleep, now=lambda: datetime.now(timezone.utc)):
    validate_arguments(repo, revision)
    if not 0 <= wait_seconds <= 1200 or not 1 <= poll_seconds <= 60:
        raise SelectionError("Wait must be 0–1200 seconds and polling 1–60 seconds.")
    base = f"repos/{repo}"
    repository = api(base)
    repo_id = positive_id(repository.get("id"), "repository ID")
    if str(repository.get("full_name", "")).lower() != repo.lower():
        raise SelectionError("GitHub repository identity does not match the requested repository.")
    workflow = api(f"{base}/actions/workflows/gateway.yml")
    workflow_id = positive_id(workflow.get("id"), "workflow ID")
    if workflow.get("path") != WORKFLOW or workflow.get("state") != "active":
        raise SelectionError("Gateway CI workflow is missing, disabled, or has the wrong identity.")
    deadline = monotonic() + wait_seconds
    while True:
        ensure_main(api, base, revision)
        latest = latest_run(api, base, repo, repo_id, workflow_id, revision)
        run = api(f"{base}/actions/runs/{latest['id']}")
        validate_run(run, repo, repo_id, workflow_id, revision)
        if run["id"] != latest["id"] or run["run_number"] != latest["run_number"]:
            raise SelectionError("Selected CI run identity changed; retry the release.")
        if run.get("status") == "completed":
            if run.get("conclusion") != "success":
                raise SelectionError(f"Latest main CI run {run['id']} ended {run.get('conclusion')!r}. Rerun Gateway CI; older green runs are not eligible.")
            verify_jobs(api, base, run)
            artifact = find_artifact(api, base, run, repo_id, revision, now)
            # A rerun/new main run while fetching jobs/artifacts invalidates the
            # selection. Releases repeat this check after environment approval.
            fresh = latest_run(api, base, repo, repo_id, workflow_id, revision)
            current = api(f"{base}/actions/runs/{run['id']}")
            validate_run(current, repo, repo_id, workflow_id, revision)
            if (fresh["id"], fresh["run_attempt"]) != (run["id"], run["run_attempt"]) or any(
                    current.get(key) != run.get(key) for key in ("id", "run_attempt", "status", "conclusion")):
                raise SelectionError("CI run or attempt changed during selection; retry the release.")
            ensure_main(api, base, revision)
            return {"run_id": run["id"], "run_attempt": run["run_attempt"],
                    "artifact_id": artifact["id"], "artifact_name": artifact["name"], "revision": revision}
        if run.get("status") not in ACTIVE or run.get("conclusion") is not None:
            raise SelectionError("Latest CI run has an invalid status; refusing to select its image.")
        remaining = deadline - monotonic()
        if remaining <= 0:
            raise SelectionError(f"Timed out waiting for main CI run {run['id']}. Wait for Gateway CI to pass, then retry the release.")
        print(f"Waiting for Gateway CI run {run['id']} ({run['status']})…", file=sys.stderr)
        sleep(min(poll_seconds, remaining))


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", "--repository", required=True)
    parser.add_argument("--revision", required=True)
    parser.add_argument("--wait-seconds", type=int, default=0)
    parser.add_argument("--poll-seconds", type=int, default=15)
    for field in ("run-id", "run-attempt", "artifact-id"):
        parser.add_argument(f"--expected-{field}", type=int)
    args = parser.parse_args(argv)
    try:
        result = select(args.repo, args.revision, args.wait_seconds, args.poll_seconds)
        for field in ("run_id", "run_attempt", "artifact_id"):
            expected = getattr(args, f"expected_{field}")
            if expected is not None and result[field] != expected:
                raise SelectionError(f"Selected {field} changed after approval; start a new release.")
        if output := os.environ.get("GITHUB_OUTPUT"):
            with open(output, "a", encoding="utf-8") as stream:
                for field in ("run_id", "run_attempt", "artifact_id", "artifact_name"):
                    stream.write(f"{field}={result[field]}\n")
        print(json.dumps(result, sort_keys=True))
        return 0
    except (SelectionError, OSError) as error:
        print(f"Image selection failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
