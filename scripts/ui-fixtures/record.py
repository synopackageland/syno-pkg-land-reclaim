#!/usr/bin/env python3
"""Rebuild UI fixtures from the saved CLI goldens and optional local captures."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any

from sanitize import Sanitizer


GOLDENS = (
    "cache.json",
    "history.json",
    "inspect.json",
    "job-result.json",
    "jobs.json",
    "overview.json",
    "recycle.json",
    "scan.json",
    "scan-partial.json",
    "scan-volume.json",
    "session.json",
    "simulate-delete.json",
    "snapshots.json",
    "top-held.json",
    "top-volume.json",
    "top.json",
    "volumes.json",
    "watch-free.json",
    "webapi-invalid-param.json",
    "why-not-freed.json",
)

CAPTURES = (
    "volumes-current.json",
    "overview-volume.json",
    "history-current.json",
    "jobs-current.json",
)


def read(path: Path) -> Any:
    return json.loads(path.read_text(encoding="utf-8"))


def write_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps(value, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )


def add_fixture(
    root: Path,
    method: str,
    filename: str,
    document: Any,
    *,
    params: dict[str, Any] | None = None,
    scenario: str | None = None,
) -> None:
    folder = root / method
    write_json(folder / filename, document)
    index_path = folder / "index.json"
    entries = read(index_path) if index_path.exists() else []
    entry: dict[str, Any] = {"file": filename}
    if params is not None:
        entry["params"] = params
    if scenario is not None:
        entry["scenario"] = scenario

    for position, previous in enumerate(entries):
        if previous.get("file") == filename and previous.get("scenario") == scenario:
            entries[position] = entry
            break
    else:
        if params is not None:
            # More specific recordings must win over broad fallbacks.
            specificity = len(params)
            position = next(
                (
                    position
                    for position, previous in enumerate(entries)
                    if previous.get("scenario") == scenario
                    and len(previous.get("params", {})) < specificity
                ),
                len(entries),
            )
            entries.insert(position, entry)
        else:
            entries.append(entry)
    write_json(index_path, entries)


def add_error(
    root: Path, method: str, scenario: str, error: dict[str, Any]
) -> None:
    folder = root / method
    folder.mkdir(parents=True, exist_ok=True)
    index_path = folder / "index.json"
    entries = read(index_path) if index_path.exists() else []
    entry = {"scenario": scenario, "error": error}
    for position, previous in enumerate(entries):
        if previous.get("scenario") == scenario and "error" in previous:
            entries[position] = entry
            break
    else:
        entries.append(entry)
    write_json(index_path, entries)


def path_string(value: Any) -> str:
    if isinstance(value, str):
        return value
    if isinstance(value, dict) and isinstance(value.get("path"), str):
        return value["path"]
    raise ValueError(f"expected a path string, got {value!r}")


def main() -> int:
    repo = Path(__file__).resolve().parents[2]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--captures",
        type=Path,
        required=True,
        help="private directory of saved, read-only NAS CLI outputs (never copied raw)",
    )
    args = parser.parse_args()

    sources: dict[str, Path] = {
        name: repo / "tests/golden/json" / name for name in GOLDENS
    }
    for name in CAPTURES:
        candidate = args.captures / name
        if candidate.is_file():
            sources[name] = candidate

    manifest_path = args.captures / "manifest.json"
    capture_records = read(manifest_path) if manifest_path.is_file() else []
    if not isinstance(capture_records, list):
        raise ValueError("capture manifest must be a JSON array")
    capture_documents: dict[str, Any] = {}
    capture_inputs: list[Any] = []
    for record in capture_records:
        source = record.get("source")
        if not isinstance(source, str) or Path(source).is_absolute() or ".." in Path(source).parts:
            raise ValueError(f"invalid capture source: {source!r}")
        source_path = args.captures / source
        if not source_path.is_file() or source_path.is_symlink():
            raise ValueError(f"missing or unsafe capture source: {source_path}")
        capture_documents[source] = read(source_path)
        capture_inputs.append(capture_documents[source])
        if "params" in record:
            capture_inputs.append({"params": record["params"]})

    documents = {name: read(path) for name, path in sources.items()}
    sanitizer = Sanitizer(list(documents.values()), ["/fixture/mnt"])
    safe = {name: sanitizer.clean(doc) for name, doc in documents.items()}
    fixtures = repo / "ui/fixtures"

    capture_safe: dict[str, Any] = {}
    capture_params: list[dict[str, Any] | None] = []
    if capture_records:
        capture_sanitizer = Sanitizer(capture_inputs, ["/volume1"])
        capture_safe = {
            source: capture_sanitizer.clean(document)
            for source, document in capture_documents.items()
        }
        sanitized_dir = args.captures / "sanitized"
        for source, document in capture_safe.items():
            write_json(sanitized_dir / source, document)
        for record in capture_records:
            capture_params.append(
                capture_sanitizer.clean(record["params"])
                if "params" in record
                else None
            )

    # These replace the scaffold's seed documents with the exact saved CLI
    # goldens. Existing running/paused scenario fixtures and indexes are kept.
    add_fixture(fixtures, "session.get", "admin.json", safe["session.json"])
    add_fixture(fixtures, "volumes.list", "volume1.json", safe["volumes.json"])
    add_fixture(fixtures, "jobs.list", "finished.json", safe["jobs.json"])

    add_fixture(fixtures, "history.list", "recorded.json", safe["history.json"])
    add_fixture(
        fixtures,
        "overview.get",
        "volume1.json",
        safe["overview.json"],
        params={"volume": "/volume1"},
    )

    if "volumes-current.json" in safe:
        add_fixture(
            fixtures,
            "volumes.list",
            "no-report.json",
            safe["volumes-current.json"],
            scenario="no_report",
        )
    if "overview-volume.json" in safe and not safe["overview-volume.json"].get(
        "results"
    ):
        add_fixture(
            fixtures,
            "overview.get",
            "no-report.json",
            safe["overview-volume.json"],
            params={"volume": "/volume1"},
            scenario="no_report",
        )

    scan = safe["scan.json"]
    partial_scan = safe["scan-partial.json"]
    volume_scan = safe["scan-volume.json"]
    add_fixture(
        fixtures,
        "job.result",
        "completed.json",
        scan,
        params={"job_id": scan["job_id"]},
    )
    add_fixture(
        fixtures,
        "job.result",
        "volume-scan.json",
        volume_scan,
        params={"job_id": volume_scan["job_id"]},
    )
    add_fixture(
        fixtures,
        "job.result",
        "partial.json",
        partial_scan,
        params={"job_id": partial_scan["job_id"]},
    )
    add_fixture(
        fixtures,
        "job.result",
        "partial.json",
        partial_scan,
        params={"job_id": partial_scan["job_id"]},
        scenario="partial",
    )

    no_result = safe["job-result.json"]
    add_fixture(
        fixtures,
        "job.result",
        "no-result.json",
        no_result,
        params={"job_id": no_result["job_id"]},
        scenario="error",
    )

    top_sources = (
        ("volume1-all.json", safe["top-volume.json"]),
        ("share-held.json", safe["top-held.json"]),
        ("share-files.json", safe["top.json"]),
    )
    for filename, document in top_sources:
        target = path_string(document["results"][0]["target"])
        add_fixture(
            fixtures,
            "top.list",
            filename,
            document,
            params={
                "target": target,
                "kind": document["kind"],
            },
        )
    if safe["webapi-invalid-param.json"].get("outcome") == "invalid_param":
        # The saved webapi refusal exits 2; SPEC 2.2 maps that to DSM code 4001.
        add_error(
            fixtures,
            "top.list",
            "invalid_param",
            {"code": 4001, "reason": "invalid_param", "exit": 2},
        )

    inspect = safe["inspect.json"]
    add_fixture(
        fixtures,
        "inspect.get",
        "partial.json",
        inspect,
    )
    add_fixture(
        fixtures,
        "inspect.get",
        "partial.json",
        inspect,
        scenario="partial",
    )

    simulation = safe["simulate-delete.json"]
    simulation_paths = [
        item["path"] for item in simulation["results"][0]["selection"]
    ]
    add_fixture(
        fixtures,
        "simulate.get",
        "files.json",
        simulation,
        params={"paths": simulation_paths},
    )

    snapshots = safe["snapshots.json"]
    add_fixture(
        fixtures,
        "snapshots.list",
        "share.json",
        snapshots,
    )

    recycle = safe["recycle.json"]
    add_fixture(
        fixtures,
        "recycle.list",
        "share.json",
        recycle,
    )

    add_fixture(
        fixtures,
        "why_not_freed.get",
        "volume1.json",
        safe["why-not-freed.json"],
        params={"volume": "/volume1"},
    )
    add_fixture(
        fixtures,
        "why_not_freed.get",
        "volume1.json",
        safe["why-not-freed.json"],
        params={"volume": "/volume1"},
        scenario="partial",
    )

    cache = safe["cache.json"]
    add_fixture(
        fixtures,
        "cache.status",
        "volume1.json",
        cache,
        params={"volume": path_string(cache["filesystems"][0]["mount"])},
    )

    watch = safe["watch-free.json"]
    add_fixture(
        fixtures,
        "watch_free.sample",
        "volume1.json",
        watch,
        params={"volume": path_string(watch["volume"])},
    )

    if "history-current.json" in safe:
        add_fixture(
            fixtures,
            "history.list",
            "no-report.json",
            safe["history-current.json"],
            scenario="no_report",
        )
    if "jobs-current.json" in safe:
        add_fixture(
            fixtures,
            "jobs.list",
            "no-report.json",
            safe["jobs-current.json"],
            scenario="no_report",
        )

    for record, params in zip(capture_records, capture_params, strict=True):
        if record.get("raw_only"):
            continue
        method = record.get("method")
        filename = record.get("file")
        source = record["source"]
        if (
            not isinstance(method, str)
            or Path(method).name != method
            or not isinstance(filename, str)
            or Path(filename).name != filename
        ):
            raise ValueError(f"invalid fixture destination in capture: {record!r}")
        scenario = record.get("scenario")
        if "error" in record:
            add_error(
                fixtures,
                method,
                scenario,
                capture_sanitizer.clean(record["error"]),
            )
        else:
            add_fixture(
                fixtures,
                method,
                filename,
                capture_safe[source],
                params=params,
                scenario=scenario,
            )

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
