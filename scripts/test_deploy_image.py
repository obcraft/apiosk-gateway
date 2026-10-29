"""Release safety tests use synthetic Machines and never access Fly or payments."""

import copy
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("deploy_image", Path(__file__).with_name("deploy-image.py"))
deploy = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(deploy)
REVISION = "a" * 40
OLD = f"{deploy.REGISTRY}@sha256:" + "b" * 64
NEW = f"{deploy.REGISTRY}@sha256:" + "c" * 64


def machine(digest=OLD, machine_id="one", state="started"):
    return {"id": machine_id, "state": state,
            "image_ref": {"registry": "registry.fly.io", "repository": deploy.APP,
                          "digest": digest.split("@", 1)[1]},
            "config": {"env": {"V2_EXECUTION_ENABLED": "1", "FLY_PROCESS_GROUP": "app",
                               "PRIMARY_REGION": "ams"}},
            "checks": [{"name": "servicecheck", "status": "passing", "output": "secret"}]}


class ReleaseTests(unittest.TestCase):
    def setUp(self):
        self.config = {"app": deploy.APP, "env": {"V2_EXECUTION_ENABLED": "1"}}
        self.state = {"Name": deploy.APP, "Machines": [machine()]}
        self.health = {"service": deploy.APP, "protocol_version": "2", "version": REVISION,
                       "payment_executor": "ready"}

    def test_preflight_accepts_existing_configuration_and_digest(self):
        self.assertEqual(deploy.preflight(self.config, self.state), OLD)

    def test_preflight_rejects_payment_flag_drift(self):
        self.config["env"]["V2_EXECUTION_ENABLED"] = "0"
        with self.assertRaisesRegex(ValueError, "V2_EXECUTION_ENABLED"):
            deploy.preflight(self.config, self.state)

    def test_native_cutover_only_removes_known_listener_settings_and_restores_rollback(self):
        self.state["Machines"][0]["config"]["env"].update(deploy.RETIRED_EXECUTOR_ENV)
        self.assertEqual(deploy.preflight(self.config, self.state), OLD)
        rollback = deploy.rollback_command(OLD, self.config, self.state)
        for key, value in deploy.RETIRED_EXECUTOR_ENV.items():
            self.assertIn(f"{key}={value}", rollback)
        self.state["Machines"][0]["config"]["env"]["EMBEDDED_EXECUTOR_ORIGIN"] = "http://localhost:9090"
        with self.assertRaisesRegex(ValueError, "EMBEDDED_EXECUTOR_ORIGIN"):
            deploy.preflight(self.config, self.state)

    def test_preflight_rejects_removing_runtime_variables(self):
        self.config["env"] = {}
        with self.assertRaisesRegex(ValueError, "Runtime environment drift"):
            deploy.preflight(self.config, self.state)

    def test_preflight_rejects_mixed_retired_listener_settings(self):
        original = machine()
        original["config"]["env"].update(deploy.RETIRED_EXECUTOR_ENV)
        same = copy.deepcopy(original)
        same["id"] = "two"
        self.assertEqual(deploy.preflight(
            self.config, {"Name": deploy.APP, "Machines": [original, same]}), OLD)
        variants = [machine(machine_id="two")]
        for key in deploy.RETIRED_EXECUTOR_ENV:
            missing = copy.deepcopy(original)
            missing["id"] = "two"
            del missing["config"]["env"][key]
            variants.append(missing)
        for variant in variants:
            for machines in ([original, variant], [variant, original]):
                with self.subTest(machines=machines), self.assertRaisesRegex(
                        ValueError, "Retired executor environment differs"):
                    deploy.preflight(self.config, {"Name": deploy.APP, "Machines": machines})
        changed = copy.deepcopy(original)
        changed["id"] = "two"
        changed["config"]["env"]["EMBEDDED_EXECUTOR_PORT"] = "9090"
        with self.assertRaisesRegex(ValueError, "Retired executor environment differs"):
            deploy.preflight(self.config, {"Name": deploy.APP, "Machines": [original, changed]})

    def test_preflight_rejects_migrations_and_mixed_releases(self):
        for config, state in [({**self.config, "deploy": {"release_command": "migrate"}}, self.state),
                              (self.config, {**self.state, "Machines": [machine(), machine(NEW, "two")]}),
                              (self.config, {**self.state, "Machines": []})]:
            with self.subTest(config=config, state=state), self.assertRaises(ValueError):
                deploy.preflight(config, state)

    def test_verification_requires_each_machine_digest_health_and_exact_revision(self):
        state = {"Machines": [machine(NEW), machine(NEW, "two")]}
        deploy.verify_rollout(state, self.health, NEW, REVISION, 2)
        variants = []
        stale_image = copy.deepcopy(state)
        stale_image["Machines"][1] = machine(OLD, "two")
        variants.append((stale_image, self.health))
        failed_check = copy.deepcopy(state)
        failed_check["Machines"][1]["checks"][0]["status"] = "critical"
        variants.append((failed_check, self.health))
        variants.append((state, {**self.health, "version": "unknown"}))
        variants.append((state, {**self.health, "payment_executor": "unavailable"}))
        variants.append(({"Machines": [machine(NEW)]}, self.health))
        for candidate, health in variants:
            with self.subTest(state=candidate, health=health), self.assertRaises(ValueError):
                deploy.verify_rollout(candidate, health, NEW, REVISION, 2)

    def test_evidence_omits_runtime_variables_and_health_output(self):
        encoded = json.dumps(deploy.snapshot(self.state))
        self.assertNotIn("V2_EXECUTION_ENABLED", encoded)
        self.assertNotIn("secret", encoded)

    def test_verification_rejects_non_object_health(self):
        state = {"Machines": [machine(NEW)]}
        for health in (None, [], "ok", 200):
            with self.subTest(health=health), self.assertRaisesRegex(ValueError, "JSON object"):
                deploy.verify_rollout(state, health, NEW, REVISION, 1)

    def test_wait_retries_eventual_consistency(self):
        current = {"Machines": [machine(NEW)]}
        with patch.object(deploy, "status", side_effect=[self.state, current]), \
                patch.object(deploy, "get_health", return_value=self.health), \
                patch.object(deploy.time, "sleep"):
            result, health = deploy.wait_until_healthy(NEW, REVISION, 1)
        self.assertEqual(result, current)
        self.assertEqual(health, self.health)

    def test_main_dry_run_is_read_only_and_records_rollback(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / "fly.toml").write_text('app = "apiosk-gateway-v2"\n[env]\nV2_EXECUTION_ENABLED = "1"\n')
            evidence = root / "evidence.json"
            with patch.object(deploy, "ROOT", root), \
                    patch.object(deploy, "run", side_effect=[REVISION, ""]) as runner, \
                    patch.object(deploy, "status", return_value=self.state):
                code = deploy.main(["--image", NEW, "--revision", REVISION,
                                    "--evidence", str(evidence), "--dry-run"])
            self.assertEqual(code, 0)
            self.assertEqual(runner.call_count, 2)
            report = json.loads(evidence.read_text())
            self.assertEqual(report["status"], "dry-run")
            self.assertEqual(report["previous_image"], OLD)

    def test_main_refuses_checkout_mismatch_before_fly(self):
        with tempfile.TemporaryDirectory() as folder, \
                patch.object(deploy, "run", return_value="d" * 40), \
                patch.object(deploy, "status") as read_fly:
            code = deploy.main(["--image", NEW, "--revision", REVISION,
                                "--evidence", str(Path(folder) / "evidence.json")])
        self.assertEqual(code, 1)
        read_fly.assert_not_called()

    def test_main_deploys_once_and_saves_verified_evidence(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / "fly.toml").write_text('app = "apiosk-gateway-v2"\n[env]\nV2_EXECUTION_ENABLED = "1"\n')
            evidence = root / "evidence.json"
            after = {"Machines": [machine(NEW)]}
            with patch.object(deploy, "ROOT", root), \
                    patch.object(deploy, "run", side_effect=[REVISION, "", None]) as runner, \
                    patch.object(deploy, "status", return_value=self.state), \
                    patch.object(deploy, "wait_until_healthy", return_value=(after, self.health)):
                code = deploy.main(["--image", NEW, "--revision", REVISION,
                                    "--evidence", str(evidence)])
            self.assertEqual(code, 0)
            self.assertEqual(runner.call_count, 3)
            self.assertEqual(runner.call_args.args, tuple(deploy.deploy_command(NEW)))
            self.assertEqual(json.loads(evidence.read_text())["status"], "healthy")

    def test_failed_rollout_records_failure_without_automatic_rollback(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / "fly.toml").write_text('app = "apiosk-gateway-v2"\n[env]\nV2_EXECUTION_ENABLED = "1"\n')
            evidence = root / "evidence.json"
            with patch.object(deploy, "ROOT", root), \
                    patch.object(deploy, "run", side_effect=[REVISION, "", None]) as runner, \
                    patch.object(deploy, "status", return_value=self.state), \
                    patch.object(deploy, "wait_until_healthy", side_effect=RuntimeError("unhealthy")):
                code = deploy.main(["--image", NEW, "--revision", REVISION,
                                    "--evidence", str(evidence)])
            self.assertEqual(code, 1)
            self.assertEqual(runner.call_count, 3)
            report = json.loads(evidence.read_text())
            self.assertEqual(report["status"], "failed")
            self.assertIn(OLD, report["rollback_command"])


if __name__ == "__main__":
    unittest.main()
