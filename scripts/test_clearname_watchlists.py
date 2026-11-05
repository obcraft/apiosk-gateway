import copy
from datetime import datetime, timedelta, timezone
import json
from pathlib import Path
import tempfile
import unittest

from import_clearname_watchlists import ImportFailure, REGISTRY, digest, import_snapshot, normalize


NOW = datetime(2026, 11, 5, 22, 0, tzinfo=timezone.utc)


def fixture():
    sources = json.loads(REGISTRY.read_text())["sources"]
    return {
        "generated": "2026-11-05T13:54Z",
        "entries": [{"s": source["code"], "id": "same-id", "t": "PEP" if source["code"] == "EP" else "persoon",
                     "n": ["Example Name", "Alias مثال", "Alias пример"],
                     "p": "Entity List (EL)" if source["code"] == "CSL" else "Example programme", "x": ""}
                    for source in sources],
    }


def artifacts(data):
    return normalize(json.dumps(data, ensure_ascii=False).encode(), now=NOW)


class ClearnameImportTests(unittest.TestCase):
    def test_counts_provenance_aliases_and_categories(self):
        sha, index_bytes, manifest_bytes = artifacts(fixture())
        index, manifest = json.loads(index_bytes), json.loads(manifest_bytes)
        self.assertEqual(manifest["input_sha256"], sha)
        self.assertEqual(manifest["index_sha256"], digest(index_bytes))
        self.assertEqual(manifest["record_count"], 11)
        self.assertEqual(manifest["name_count"], 33)
        self.assertEqual(manifest["csl_underlying_lists"], {"EL": 1})
        self.assertEqual(index["snapshot_generated_at"], "2026-11-05T13:54:00Z")
        self.assertEqual({s["code"]: s["category"] for s in manifest["sources"]}["EP"], "limited_pep")
        self.assertEqual({s["code"]: s["category"] for s in manifest["sources"]}["WB"], "debarment")
        self.assertIn("Alias مثال", index["records"][0]["names"])
        self.assertIn("Alias пример", index["records"][0]["names"])
        # Same ID in different sources is deliberately not an entity deduplication key.
        self.assertEqual(len({r["source_record_id"] for r in index["records"]}), 1)

    def test_rejects_missing_source_instead_of_silent_partial_success(self):
        data = fixture()
        data["entries"] = [entry for entry in data["entries"] if entry["s"] != "NL"]
        with self.assertRaisesRegex(ImportFailure, "NL"):
            artifacts(data)

    def test_rejects_duplicate_records_but_deduplicates_identical_names(self):
        data = fixture()
        data["entries"][0]["n"].append(" Example Name ")
        self.assertEqual(json.loads(artifacts(data)[2])["name_count"], 33)
        data["entries"].append(copy.deepcopy(data["entries"][0]))
        with self.assertRaisesRegex(ImportFailure, "repeats"):
            artifacts(data)

    def test_rejects_sdn_in_csl_but_preserves_non_sdn(self):
        data = fixture()
        entry = next(e for e in data["entries"] if e["s"] == "CSL")
        for label in ["Specially Designated Nationals (SDN)", "SDN", "Unreviewed new list"]:
            entry["p"] = label
            with self.subTest(label=label), self.assertRaisesRegex(ImportFailure, "SDN is excluded"):
                artifacts(data)
        entry["p"] = "Non-SDN Menu-Based Sanctions List (NS-MBS List)"
        self.assertEqual(json.loads(artifacts(data)[2])["csl_underlying_lists"], {"NS-MBS": 1})

    def test_rejects_malformed_identity_and_unknown_provenance(self):
        for field, value in [("s", "UNKNOWN"), ("id", " "), ("id", 42), ("t", "unknown"),
                             ("t", "PEP"), ("n", []), ("n", [" "]), ("n", "Name"), ("x", None)]:
            data = fixture()
            data["entries"][0][field] = value
            with self.subTest(field=field, value=value), self.assertRaises(ImportFailure):
                artifacts(data)

    def test_normalizes_existing_entity_type_codes(self):
        for value, expected in [("P", "person"), ("E", "organization"), ("schip", "vessel")]:
            data = fixture()
            data["entries"][0]["t"] = value
            records = json.loads(artifacts(data)[1])["records"]
            self.assertEqual(next(r for r in records if r["source_code"] == "EU")["entity_type"], expected)

    def test_rejects_stale_future_and_unzoned_snapshots(self):
        for generated in ["2026-10-01T13:54Z", "2026-11-06T13:54Z", "2026-11-05T13:54", "bad"]:
            data = fixture()
            data["generated"] = generated
            with self.subTest(generated=generated), self.assertRaises(ImportFailure):
                artifacts(data)
        raw = json.dumps(fixture()).encode()
        for max_age in [0, -1, float("nan"), float("inf")]:
            with self.subTest(max_age=max_age), self.assertRaises(ImportFailure):
                normalize(raw, now=NOW, max_age_hours=max_age)

    def test_invalid_json_and_root_shape(self):
        for raw in [b"broken", b"\xff", b"[]", b'{"entries":{}}']:
            with self.subTest(raw=raw), self.assertRaises(ImportFailure):
                normalize(raw, now=NOW)

    def test_atomic_publication_idempotency_and_failed_import_preserves_current(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, output = root / "input.json", root / "output"
            source.write_text(json.dumps(fixture()))
            manifest = import_snapshot(source, output, now=NOW)
            current = (output / "current.json").read_bytes()
            pointer = json.loads(current)
            manifest_bytes = (output / pointer["snapshot"] / "manifest.json").read_bytes()
            self.assertEqual(pointer["manifest_sha256"], digest(manifest_bytes))
            self.assertEqual(import_snapshot(source, output, now=NOW + timedelta(hours=1)), manifest)
            self.assertEqual((output / "current.json").read_bytes(), current)
            incomplete = fixture()
            incomplete["entries"].pop()
            source.write_text(json.dumps(incomplete))
            with self.assertRaises(ImportFailure):
                import_snapshot(source, output, now=NOW)
            self.assertEqual((output / "current.json").read_bytes(), current)
            source.write_text(json.dumps(fixture()))
            (output / pointer["snapshot"] / "index.json").write_text("corrupted")
            with self.assertRaisesRegex(ImportFailure, "artifacts differ"):
                import_snapshot(source, output, now=NOW)
            self.assertEqual((output / "current.json").read_bytes(), current)
            self.assertFalse(list(output.glob(".import-*")))
            self.assertFalse(list(output.glob(".current-*")))


if __name__ == "__main__":
    unittest.main()
