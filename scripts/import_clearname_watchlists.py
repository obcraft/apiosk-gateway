#!/usr/bin/env python3
"""Validate and import Clearname's existing index without downloading lists again."""

import argparse
from collections import Counter
from datetime import datetime, timedelta, timezone
import hashlib
import json
import math
from pathlib import Path
import shutil
import sys
import tempfile


REGISTRY = Path(__file__).resolve().parents[1] / "docs/clearname-watchlists/sources.json"
MAX_INPUT_BYTES = 64 * 1024 * 1024
ENTITY_TYPES = {
    "P": "person", "E": "organization", "persoon": "person",
    "organisatie": "organization", "schip": "vessel", "PEP": "person",
}
# Clearname retains the CSL's source label before " - ". New lists require review;
# checking for the substring "SDN" would incorrectly reject Non-SDN lists.
CSL_LISTS = {
    "Entity List (EL)": "EL",
    "Denied Persons List (DPL)": "DPL",
    "ITAR Debarred (DTC)": "DTC",
    "Sectoral Sanctions Identifications List (SSI)": "SSI",
    "Unverified List (UVL)": "UVL",
    "Nonproliferation Sanctions (ACN)": "ACN",
    "Palestinian Legislative Council List (PLC)": "PLC",
    "Military End User (MEU) List": "MEU",
    "Non-SDN Chinese Military-Industrial Complex Companies List (CMIC)": "CMIC",
    "Non-SDN Menu-Based Sanctions List (NS-MBS List)": "NS-MBS",
    "Capta List (CAP)": "CAP",
}


class ImportFailure(ValueError):
    pass


def encode(value):
    return (json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")) + "\n").encode()


def digest(data):
    return hashlib.sha256(data).hexdigest()


def text_field(value, field, *, required=False):
    if not isinstance(value, str) or (required and not value.strip()):
        raise ImportFailure(f"{field} must be {'a nonempty' if required else 'a'} string")
    return value.strip()


def normalize(raw, *, now=None, max_age_hours=48):
    """Return deterministic artifacts; reject incomplete or unrecognized input."""
    if not math.isfinite(max_age_hours) or max_age_hours <= 0:
        raise ImportFailure("max_age_hours must be finite and positive")
    if len(raw) > MAX_INPUT_BYTES:
        raise ImportFailure("Clearname index exceeds the 64 MiB input limit")
    try:
        data = json.loads(raw)
    except (ValueError, UnicodeError) as error:
        raise ImportFailure("Clearname index is not valid UTF-8 JSON") from error
    if not isinstance(data, dict) or not isinstance(data.get("entries"), list):
        raise ImportFailure("Expected a Clearname object with an entries array")
    generated = text_field(data.get("generated"), "generated", required=True)
    try:
        generated_at = datetime.fromisoformat(generated.replace("Z", "+00:00"))
    except ValueError as error:
        raise ImportFailure("generated must be an ISO 8601 timestamp") from error
    if generated_at.tzinfo is None:
        raise ImportFailure("generated must include a timezone")
    now = now or datetime.now(timezone.utc)
    if generated_at > now + timedelta(minutes=5):
        raise ImportFailure("Snapshot generated time is in the future")
    if now - generated_at > timedelta(hours=max_age_hours):
        raise ImportFailure("Snapshot is stale; refresh Clearname before importing")

    registry = json.loads(REGISTRY.read_text())
    sources = {source["code"]: source for source in registry["sources"]}
    counts, name_counts, csl_counts = Counter(), Counter(), Counter()
    seen, records = set(), []
    for position, entry in enumerate(data["entries"]):
        if not isinstance(entry, dict):
            raise ImportFailure(f"Entry {position} must be an object")
        source = text_field(entry.get("s"), f"entry {position} source", required=True)
        if source not in sources:
            raise ImportFailure(f"Entry {position} has an unknown source code")
        record_id = text_field(entry.get("id"), f"entry {position} id", required=True)
        key = (source, record_id)
        if key in seen:
            raise ImportFailure(f"Entry {position} repeats a source/record ID")
        seen.add(key)
        entity_type = text_field(entry.get("t"), f"entry {position} type", required=True)
        if entity_type not in ENTITY_TYPES:
            raise ImportFailure(f"Entry {position} has an unknown entity type")
        if (source == "EP") != (entity_type == "PEP"):
            raise ImportFailure(f"Entry {position} has inconsistent PEP provenance")
        names = entry.get("n")
        if not isinstance(names, list) or not names:
            raise ImportFailure(f"Entry {position} must contain names")
        names = list(dict.fromkeys(
            text_field(name, f"entry {position} name", required=True) for name in names
        ))
        programme = text_field(entry.get("p"), f"entry {position} programme")
        notes = text_field(entry.get("x"), f"entry {position} notes")
        record = {
            "source_code": source, "source_record_id": record_id,
            "entity_type": ENTITY_TYPES[entity_type], "names": names,
            "programme": programme, "notes": notes,
        }
        if source == "CSL":
            if programme not in CSL_LISTS:
                raise ImportFailure(f"Entry {position} has an unsupported CSL list (SDN is excluded)")
            record["underlying_list"] = CSL_LISTS[programme]
            csl_counts[record["underlying_list"]] += 1
        records.append(record)
        counts[source] += 1
        name_counts[source] += len(names)
    missing = sorted(set(sources) - set(counts))
    if missing:
        raise ImportFailure("Missing or empty required sources: " + ", ".join(missing))

    input_sha = digest(raw)
    generated_utc = generated_at.astimezone(timezone.utc).isoformat().replace("+00:00", "Z")
    records.sort(key=lambda record: (record["source_code"], record["source_record_id"]))
    index = encode({
        "schema_version": 1, "input_sha256": input_sha,
        "snapshot_generated_at": generated_utc, "records": records,
    })
    manifest = encode({
        "schema_version": 1, "importer": "clearname-watchlists-v1",
        "upstream_repository": "obcraft/clearname-watchlist-check",
        "input_sha256": input_sha, "index_sha256": digest(index),
        "snapshot_generated_at": generated_utc,
        "record_count": len(records), "name_count": sum(name_counts.values()),
        "sources": [dict(source, record_count=counts[source["code"]],
                         name_count=name_counts[source["code"]]) for source in registry["sources"]],
        "csl_underlying_lists": dict(sorted(csl_counts.items())),
        "limitations": registry["limitations"],
    })
    return input_sha, index, manifest


def import_snapshot(input_path, output_dir, *, now=None, max_age_hours=48):
    # Validate fully before creating any new artifacts or replacing the pointer.
    with Path(input_path).open("rb") as source:
        raw = source.read(MAX_INPUT_BYTES + 1)
    input_sha, index, manifest = normalize(raw, now=now, max_age_hours=max_age_hours)
    output_dir = Path(output_dir)
    output_dir.mkdir(parents=True, exist_ok=True)
    snapshot_dir = output_dir / input_sha
    expected = {"index.json": index, "manifest.json": manifest}
    if snapshot_dir.exists():
        if any(not (snapshot_dir / name).is_file() or (snapshot_dir / name).read_bytes() != body
               for name, body in expected.items()):
            raise ImportFailure("Existing snapshot artifacts differ; current snapshot was preserved")
    else:
        staging = Path(tempfile.mkdtemp(prefix=".import-", dir=output_dir))
        try:
            for name, body in expected.items():
                (staging / name).write_bytes(body)
            staging.rename(snapshot_dir)
        finally:
            if staging.exists():
                shutil.rmtree(staging)
    pointer = encode({"schema_version": 1, "snapshot": input_sha,
                      "manifest_sha256": digest(manifest)})
    with tempfile.NamedTemporaryFile(dir=output_dir, prefix=".current-", delete=False) as file:
        temporary_pointer = Path(file.name)
        try:
            file.write(pointer)
            file.flush()
            temporary_pointer.replace(output_dir / "current.json")
        finally:
            temporary_pointer.unlink(missing_ok=True)
    return json.loads(manifest)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("input", type=Path, help="Existing Clearname sanctions.json snapshot")
    parser.add_argument("--output", type=Path, required=True, help="Local artifact directory")
    parser.add_argument("--max-age-hours", type=float, default=48,
                        help="Maximum snapshot generation age (default: 48 hours)")
    args = parser.parse_args()
    try:
        manifest = import_snapshot(args.input, args.output, max_age_hours=args.max_age_hours)
    except (ImportFailure, OSError) as error:
        print(f"Import failed: {error}", file=sys.stderr)
        return 1
    print(json.dumps({key: manifest[key] for key in (
        "input_sha256", "snapshot_generated_at", "record_count", "name_count", "sources",
        "csl_underlying_lists",
    )}, ensure_ascii=False, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
