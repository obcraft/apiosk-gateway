"""Artifact selection checks use synthetic GitHub API responses only."""

import copy
from datetime import datetime, timezone
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch
from urllib.parse import parse_qs, urlsplit

SPEC = importlib.util.spec_from_file_location("find_verified_image", Path(__file__).with_name("find-verified-image.py"))
selector = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(selector)
REPO = "owner/gateway"
REVISION = "a" * 40
NOW = datetime(2026, 10, 30, tzinfo=timezone.utc)


def run(run_id=100, **changes):
    result = {"id": run_id, "run_number": run_id, "run_attempt": 1,
              "head_sha": REVISION, "head_branch": "main", "event": "push",
              "repository": {"id": 7, "full_name": REPO},
              "head_repository": {"id": 7, "full_name": REPO},
              "workflow_id": 9, "path": selector.WORKFLOW,
              "status": "completed", "conclusion": "success"}
    result.update(changes)
    return result


class GitHub:
    def __init__(self):
        self.repository = {"id": 7, "full_name": REPO}
        self.workflow = {"id": 9, "path": selector.WORKFLOW, "state": "active"}
        self.main = {"ref": "refs/heads/main", "object": {"sha": REVISION}}
        self.runs = [[run()]]
        self.detail = run()
        self.jobs = [[{"name": name, "run_id": 100, "head_sha": REVISION,
                       "status": "completed", "conclusion": "success",
                       "steps": [{"name": step, "status": "completed", "conclusion": "success"}
                                 for step in steps]}
                      for name, steps in selector.REQUIRED_STEPS.items()]]
        self.artifacts = [[{"id": 200, "name": f"gateway-image-{REVISION}", "expired": False,
                            "expires_at": "2026-11-01T00:00:00Z",
                            "workflow_run": {"id": 100, "repository_id": 7, "head_repository_id": 7,
                                             "head_sha": REVISION, "head_branch": "main"}}]]
        self.calls = []
        self.hook = lambda endpoint: None

    def __call__(self, endpoint):
        self.calls.append(endpoint)
        self.hook(endpoint)
        path = urlsplit(endpoint).path
        if path == f"repos/{REPO}":
            response = self.repository
        elif path.endswith("/git/ref/heads/main"):
            response = self.main
        elif path.endswith("/actions/workflows/gateway.yml"):
            response = self.workflow
        elif path.endswith("/runs/100"):
            response = self.detail
        else:
            if path.endswith("/gateway.yml/runs"):
                key, batches = "workflow_runs", self.runs
            elif path.endswith("/jobs"):
                key, batches = "jobs", self.jobs
            elif path.endswith("/artifacts"):
                key, batches = "artifacts", self.artifacts
            else:
                raise AssertionError(f"Unexpected API request: {endpoint}")
            page = int(parse_qs(urlsplit(endpoint).query)["page"][0])
            response = {"total_count": sum(map(len, batches)), key: batches[page - 1]}
        return copy.deepcopy(response)


class SelectionTests(unittest.TestCase):
    def setUp(self):
        self.api = GitHub()

    def select(self, **options):
        return selector.select(REPO, REVISION, api=self.api, now=lambda: NOW, **options)

    def test_success_pins_run_attempt_artifact_and_revision(self):
        self.assertEqual(self.select(), {"run_id": 100, "run_attempt": 1, "artifact_id": 200,
                                        "artifact_name": f"gateway-image-{REVISION}", "revision": REVISION})
        self.assertIn(f"repos/{REPO}/actions/runs/100/attempts/1/jobs?per_page=100&page=1", self.api.calls)

    def test_invalid_repo_revision_and_wait_are_rejected_before_api(self):
        for repo, revision in [("../owner/repo", REVISION), ("owner/repo?x=1", REVISION),
                               ("owner/repo/extra", REVISION), (REPO, "main"), (REPO, "a" * 39),
                               (REPO, "A" * 40), (REPO, "g" * 40)]:
            with self.subTest(repo=repo, revision=revision), self.assertRaises(selector.SelectionError):
                selector.select(repo, revision, api=self.api)
        with self.assertRaises(selector.SelectionError):
            self.select(wait_seconds=3601)
        self.assertEqual(self.api.calls, [])

    def test_wait_and_poll_boundaries(self):
        for wait_seconds, poll_seconds in ((0, 1), (3600, 60)):
            with self.subTest(wait=wait_seconds, poll=poll_seconds):
                self.assertEqual(self.select(wait_seconds=wait_seconds, poll_seconds=poll_seconds)["run_id"], 100)
        for wait_seconds, poll_seconds in ((-1, 15), (3601, 15), (3600, 0), (3600, 61)):
            with self.subTest(wait=wait_seconds, poll=poll_seconds), self.assertRaisesRegex(
                    selector.SelectionError, "0–3600 seconds and polling 1–60"):
                self.select(wait_seconds=wait_seconds, poll_seconds=poll_seconds)

    def test_rejects_stale_main_at_start_and_after_selection(self):
        self.api.main["object"]["sha"] = "b" * 40
        with self.assertRaisesRegex(selector.SelectionError, "no longer main"):
            self.select()
        self.api = GitHub()
        def advance(endpoint):
            if endpoint.endswith("/git/ref/heads/main") and self.api.calls.count(endpoint) == 2:
                self.api.main["object"]["sha"] = "b" * 40
        self.api.hook = advance
        with self.assertRaisesRegex(selector.SelectionError, "no longer main"):
            self.select()

    def test_wrong_sha_branch_and_pr_cannot_authorize_release(self):
        for changes in ({"head_sha": "b" * 40}, {"head_branch": "feature"},
                        {"event": "pull_request"}, {"event": "pull_request_target"}):
            with self.subTest(changes=changes):
                self.api.runs = [[run(**changes)]]
                with self.assertRaisesRegex(selector.SelectionError, "No main CI run"):
                    self.select()

    def test_wrong_repo_or_fork_is_rejected(self):
        for field in ("repository", "head_repository"):
            for identity in ({"id": 8, "full_name": REPO}, {"id": 7, "full_name": "fork/gateway"}):
                with self.subTest(field=field, identity=identity):
                    self.api.runs = [[run(**{field: identity})]]
                    with self.assertRaisesRegex(selector.SelectionError, "repository mismatch"):
                        self.select()

    def test_workflow_identity_and_repository_metadata_are_checked(self):
        for target, field, value in (("workflow", "path", "other.yml"), ("workflow", "state", "disabled_manually"),
                                     ("repository", "full_name", "other/repo")):
            self.api = GitHub()
            getattr(self.api, target)[field] = value
            with self.subTest(target=target, field=field), self.assertRaises(selector.SelectionError):
                self.select()
        for changes in ({"workflow_id": 22}, {"path": ".github/workflows/release.yml"}, {"path": selector.WORKFLOW + "@feature"}):
            self.api = GitHub()
            self.api.runs = [[run(**changes)]]
            with self.subTest(changes=changes), self.assertRaises(selector.SelectionError):
                self.select()

    def test_supports_dispatch_and_main_qualified_workflow_path(self):
        self.api.runs = [[run(event="workflow_dispatch", path=selector.WORKFLOW + "@main")]]
        self.api.detail = self.api.runs[0][0]
        self.assertEqual(self.select()["artifact_id"], 200)

    def test_latest_failure_never_falls_back_to_older_success(self):
        for conclusion in ("failure", "cancelled", "skipped", "neutral", "timed_out", None):
            self.api = GitHub()
            self.api.runs = [[run(99), run(conclusion=conclusion)]]
            self.api.detail["conclusion"] = conclusion
            with self.subTest(conclusion=conclusion), self.assertRaisesRegex(selector.SelectionError, "older green"):
                self.select()

    def test_paginates_runs_jobs_and_artifacts(self):
        self.api.runs = [[run(99, conclusion="failure")], [run()]]
        self.api.jobs = [[self.api.jobs[0][0]], [self.api.jobs[0][1]]]
        self.api.artifacts = [[{"id": 199, "name": "logs"}], self.api.artifacts[0]]
        self.assertEqual(self.select()["artifact_id"], 200)
        self.assertTrue(any("artifacts?per_page=100&page=2" in call for call in self.api.calls))

    def test_required_jobs_and_steps_must_succeed(self):
        for conclusion in ("failure", "skipped", None):
            for kind in ("job", "step"):
                self.api = GitHub()
                target = self.api.jobs[0][0] if kind == "job" else self.api.jobs[0][1]["steps"][0]
                target["conclusion"] = conclusion
                with self.subTest(kind=kind, conclusion=conclusion), self.assertRaisesRegex(selector.SelectionError, "Required CI"):
                    self.select()
        self.api = GitHub()
        self.api.jobs[0].pop()
        with self.assertRaisesRegex(selector.SelectionError, "Required CI job"):
            self.select()

    def test_missing_expired_or_ambiguous_artifact_fails_actionably(self):
        for change in ("missing", "duplicate", "expired", "past"):
            self.api = GitHub()
            if change == "missing":
                self.api.artifacts = [[]]
            elif change == "duplicate":
                self.api.artifacts[0].append(copy.deepcopy(self.api.artifacts[0][0]))
            elif change == "expired":
                self.api.artifacts[0][0]["expired"] = True
            else:
                self.api.artifacts[0][0]["expires_at"] = "2026-10-29T00:00:00Z"
            with self.subTest(change=change), self.assertRaisesRegex(selector.SelectionError, "Rerun Gateway CI"):
                self.select()

    def test_artifact_provenance_and_numeric_identity_are_required(self):
        for field, value in (("id", 99), ("repository_id", 8), ("head_repository_id", 8),
                             ("head_branch", "feature"), ("head_sha", "b" * 40)):
            self.api = GitHub()
            self.api.artifacts[0][0]["workflow_run"][field] = value
            with self.subTest(field=field), self.assertRaisesRegex(selector.SelectionError, "artifact provenance"):
                self.select()
        for artifact_id in ("200", True, -1, None):
            self.api = GitHub()
            self.api.artifacts[0][0]["id"] = artifact_id
            with self.subTest(artifact_id=artifact_id), self.assertRaisesRegex(selector.SelectionError, "artifact ID"):
                self.select()

    def test_waits_only_for_matching_active_run_then_rechecks(self):
        elapsed = [0]
        self.api.detail.update(status="in_progress", conclusion=None)
        def sleep(seconds):
            elapsed[0] += seconds
            self.api.detail.update(status="completed", conclusion="success")
        with patch("sys.stderr", new=io.StringIO()):
            result = self.select(wait_seconds=30, monotonic=lambda: elapsed[0], sleep=sleep)
        self.assertEqual(result["run_id"], 100)
        self.assertEqual(elapsed[0], 15)

    def test_wait_timeout_is_bounded_and_never_selects_older_success(self):
        elapsed = [0]
        self.api.runs = [[run(99), run(status="queued", conclusion=None)]]
        self.api.detail.update(status="queued", conclusion=None)
        def sleep(seconds):
            elapsed[0] += seconds
        with patch("sys.stderr", new=io.StringIO()), self.assertRaisesRegex(selector.SelectionError, "Timed out"):
            self.select(wait_seconds=20, monotonic=lambda: elapsed[0], sleep=sleep)
        self.assertEqual(elapsed[0], 20)

    def test_rerun_or_newer_run_during_selection_is_rejected(self):
        for change in ("rerun", "new-run"):
            self.api = GitHub()
            def mutate(endpoint):
                if "/artifacts?" in endpoint:
                    if change == "rerun":
                        self.api.detail["run_attempt"] = 2
                    else:
                        self.api.runs[0].append(run(101))
            self.api.hook = mutate
            with self.subTest(change=change), self.assertRaisesRegex(selector.SelectionError, "changed during selection"):
                self.select()

    def test_api_failure_does_not_expose_response_or_token(self):
        with patch.object(selector.subprocess, "run", side_effect=subprocess.CalledProcessError(1, "gh", stderr="secret")):
            with self.assertRaisesRegex(selector.SelectionError, "GH_TOKEN") as failure:
                selector.gh_api("repos/owner/gateway")
        self.assertNotIn("secret", str(failure.exception))

    def test_malformed_api_json_fails_closed(self):
        for body in ("not json", "[]", "null"):
            with self.subTest(body=body), patch.object(selector.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, body)):
                with self.assertRaises(selector.SelectionError):
                    selector.gh_api("repos/owner/gateway")

    def test_incomplete_or_invalid_pagination_fails_closed(self):
        for response in ([], {"total_count": 1001, "artifacts": []},
                         {"total_count": 2, "artifacts": []},
                         {"total_count": 1, "artifacts": ["invalid"]},
                         {"total_count": 0, "artifacts": [{"id": 1}]},
                         {"total_count": True, "artifacts": []}):
            with self.subTest(response=response), self.assertRaises(selector.SelectionError):
                selector.pages(lambda _: response, "repos/owner/gateway/artifacts", "artifacts")

    def test_invalid_artifact_expiry_fails_closed(self):
        for expiry in (None, "not a date", "2026-11-01T00:00:00"):
            self.api = GitHub()
            self.api.artifacts[0][0]["expires_at"] = expiry
            with self.subTest(expiry=expiry), self.assertRaises(selector.SelectionError):
                self.select()

    def test_job_provenance_and_unknown_run_status_are_rejected(self):
        self.api.jobs[0][0]["head_sha"] = "b" * 40
        with self.assertRaisesRegex(selector.SelectionError, "job provenance"):
            self.select()
        self.api = GitHub()
        self.api.detail.update(status="unknown", conclusion=None)
        with self.assertRaisesRegex(selector.SelectionError, "invalid status"):
            self.select()

    def test_api_error_never_falls_back_to_another_run(self):
        self.api.runs = [[run(99), run()]]
        def unavailable(endpoint):
            if "/artifacts?" in endpoint:
                raise selector.SelectionError("GitHub unavailable")
        self.api.hook = unavailable
        with self.assertRaisesRegex(selector.SelectionError, "GitHub unavailable"):
            self.select()

    def test_gh_uses_fixed_host_authenticated_api_and_no_shell(self):
        with patch.object(selector.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, '{"id":7}')) as invoke:
            self.assertEqual(selector.gh_api("repos/owner/gateway"), {"id": 7})
        self.assertEqual(invoke.call_args.args[0][:5], ["gh", "api", "--hostname", "github.com", "-H"])
        self.assertNotIn("shell", invoke.call_args.kwargs)

    def test_outputs_and_post_approval_expected_identity(self):
        result = self.select()
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "output"
            with patch.object(selector, "select", return_value=result), patch.dict(os.environ, {"GITHUB_OUTPUT": str(output)}), patch("sys.stdout", new=io.StringIO()) as stdout:
                self.assertEqual(selector.main(["--repo", REPO, "--revision", REVISION, "--expected-run-id", "100", "--expected-run-attempt", "1", "--expected-artifact-id", "200"]), 0)
                self.assertEqual(json.loads(stdout.getvalue()), result)
            self.assertIn("artifact_id=200\n", output.read_text())
            output.unlink()
            with patch.object(selector, "select", return_value=result), patch.dict(os.environ, {"GITHUB_OUTPUT": str(output)}), patch("sys.stderr", new=io.StringIO()):
                self.assertEqual(selector.main(["--repo", REPO, "--revision", REVISION, "--expected-run-attempt", "2"]), 1)
            self.assertFalse(output.exists())


if __name__ == "__main__":
    unittest.main()
