#!/usr/bin/env python3
"""Sanitize recorded synoreclaim JSON without changing numeric measurements."""

from __future__ import annotations

import argparse
import binascii
import json
import os
import re
from pathlib import Path
from typing import Any


# Hosts in the internal `.syno` domain, and machine names kept outside the
# repository (scripts/check-internal-names.sh reads the same variables).
INTERNAL_HOST_RE = re.compile(r"\b[a-z0-9](?:[a-z0-9-]*[a-z0-9])?\.syno\b")


def _internal_names() -> list[str]:
    names = os.environ.get("SYNORECLAIM_INTERNAL_NAMES", "").replace(",", " ").split()
    names_file = os.environ.get("SYNORECLAIM_INTERNAL_NAMES_FILE")
    if names_file and os.path.isfile(names_file):
        for line in Path(names_file).read_text().splitlines():
            line = line.split("#", 1)[0].strip()
            if line:
                names.append(line)
    return names


_names = _internal_names()
INTERNAL_NAMES_RE = (
    re.compile(
        r"\b(?:" + "|".join(re.escape(n) for n in sorted(_names, key=len, reverse=True)) + r")\b",
        re.I,
    )
    if _names
    else None
)

PATH_KEYS = {
    "path",
    "paths",
    "mount",
    "volume",
    "target",
    "targets",
    "share",
    "plan_file",
    "state_dir",
    "report_path",
    "report_dir",
    "target_volume",
    "source_path",
    "source",
    "device",
    "device_path",
}
NAME_KEYS = {
    "name",
    "share_name",
    "file_name",
    "filename",
    "folder_name",
    "directory_name",
    "dirname",
    "display_name",
}
USER_KEYS = {"user", "username", "user_name", "requested_by", "owner", "login"}
HOST_KEYS = {"host", "hostname", "host_name", "nas"}
SERIAL_KEYS = {"serial", "serial_number", "serial_no"}
UUID_KEYS = {"uuid", "fsid", "filesystem_uuid", "filesystem_fsid", "fs_uuid"}
RANK_KEYS = {"logical", "physical", "shared", "reclaimable", "held"}
TEXT_KEYS = {
    "description",
    "detail",
    "hint",
    "manage_in",
    "message",
    "note",
    "reason",
    "summary",
    "stderr",
    "title",
    "where_to_fix",
}
PATH_RE = re.compile(r"(?<![A-Za-z0-9:])/(?!/)[^\s\"'<>]+")
UUID_RE = re.compile(
    r"(?i)\b(?:[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}|[0-9a-f]{32})\b"
)
IPV4_RE = re.compile(r"(?<![A-Za-z0-9.])(?:\d{1,3}\.){3}\d{1,3}(?![A-Za-z0-9.])")
VOLUME_RE = re.compile(r"^volume[0-9]+$")


class Sanitizer:
    def __init__(self, documents: list[Any], volume_roots: list[str]) -> None:
        self.volume_roots = {root.rstrip("/") for root in volume_roots if root != "/"}
        self.paths: set[str] = set()
        self.path_prefixes: set[str] = set()
        self.share_paths: set[str] = set()
        self.directory_paths: set[str] = set()
        self.file_paths: set[str] = set()
        self.names: set[str] = set()
        self.usernames: set[str] = set()
        self.hostnames: set[str] = set()
        self.serials: set[str] = set()
        self.uuids: set[str] = set()
        self.hex_uuids: set[str] = set()
        self.job_ids: set[str] = set()
        self.plan_ids: set[str] = set()
        for document in documents:
            self._find_volume_roots(document)
        self.volume_names = self._volume_names()
        for document in documents:
            self._collect(document)

        self.prefix_map = self._numbered(self.share_paths, "share", width=2)
        self.prefix_map.update(
            self._numbered(self.directory_paths - self.prefix_map.keys(), "folder", width=4)
        )
        self.prefix_map.update(
            self._numbered(
                self.file_paths - self.prefix_map.keys(), "file", width=4, suffix=".bin"
            )
        )
        remaining = self.path_prefixes - self.prefix_map.keys()
        self.prefix_map.update(self._numbered(remaining, "entry", width=4))
        self.basename_map: dict[str, str] = {}
        for original, replacement in sorted(self.prefix_map.items()):
            basename = original.rsplit("/", 1)[-1]
            self.basename_map.setdefault(basename, replacement)
        self.name_map = self._numbered(
            self.names - self.basename_map.keys(), "name", width=4
        )
        self.uuid_map = {
            value: f"f0000000-0000-4000-8000-{index:012x}"
            for index, value in enumerate(sorted(self.uuids), start=1)
        }
        self.hex_uuid_map = {
            value: f"{index:032x}"
            for index, value in enumerate(sorted(self.hex_uuids), start=1)
        }
        self.job_map = self._numbered(self.job_ids, "fixture-job")
        self.plan_map = self._numbered(self.plan_ids, "fixture-plan")

    def _find_volume_roots(self, value: Any, key: str = "") -> None:
        if isinstance(value, dict):
            for child_key, child in value.items():
                self._find_volume_roots(child, child_key.lower())
            return
        if isinstance(value, list):
            for child in value:
                self._find_volume_roots(child, key)
            return
        if not isinstance(value, str) or not value.startswith("/"):
            return
        if key == "mount" and value != "/":
            self.volume_roots.add(value.rstrip("/"))
        elif re.match(r"^/volume[0-9]+$", value):
            self.volume_roots.add(value)

    def _volume_names(self) -> dict[str, str]:
        names: dict[str, str] = {}
        next_index = 1
        for root in sorted(self.volume_roots):
            if re.fullmatch(r"/volume[0-9]+", root):
                names[root] = root
            else:
                names[root] = f"/volume{next_index}"
                next_index += 1
        return names

    def _volume_for(self, value: str) -> str | None:
        for root in sorted(self.volume_roots, key=len, reverse=True):
            if value == root or value.startswith(root + "/"):
                return root
        match = re.match(r"^(/volume[0-9]+)(?:/|$)", value)
        return match.group(1) if match else None

    @staticmethod
    def _numbered(
        values: set[str], prefix: str, width: int = 4, suffix: str = ""
    ) -> dict[str, str]:
        return {
            value: f"{prefix}{index:0{width}d}{suffix}"
            for index, value in enumerate(sorted(values), start=1)
        }

    def _collect(self, value: Any, key: str = "", role: str = "") -> None:
        if isinstance(value, dict):
            kind = value.get("kind", value.get("type"))
            if kind in {"file", "files"}:
                role = "file"
            elif kind in {"directory", "dir", "directories", "dirs"}:
                role = "directory"
            for child_key, child in value.items():
                self._collect(child, child_key.lower(), role)
            return
        if isinstance(value, list):
            for child in value:
                self._collect(child, key, role)
            return
        if not isinstance(value, str):
            return
        if key in USER_KEYS and value not in RANK_KEYS:
            self.usernames.add(value)
        if key in HOST_KEYS:
            self.hostnames.add(value)
        if key in SERIAL_KEYS:
            self.serials.add(value)
        if key in PATH_KEYS:
            if value.startswith("/"):
                self.paths.add(value)
                root = self._volume_for(value)
                if root:
                    relative = value[len(root) :].strip("/")
                    parts = relative.split("/") if relative else []
                    prefix = root
                    for depth, part in enumerate(parts, start=1):
                        prefix += "/" + part
                        self.path_prefixes.add(prefix)
                        if depth == 1:
                            self.share_paths.add(prefix)
                    if role == "file":
                        self.file_paths.add(value)
                    elif role == "directory":
                        self.directory_paths.add(value)
            elif value:
                self.names.add(value)
        if key in NAME_KEYS and value:
            self.names.add(value)
        if key in UUID_KEYS or UUID_RE.fullmatch(value):
            self.uuids.add(value)
            if key == "uuid" and re.fullmatch(r"[0-9a-f]+", value, re.I):
                self.hex_uuids.add(value)
        if key == "job_id" and value:
            self.job_ids.add(value)
        if key == "plan_id" and value:
            self.plan_ids.add(value)

    def path(self, value: str) -> str:
        root = self._volume_for(value)
        if root:
            safe_parts = [self.volume_names.get(root, "/volume1").lstrip("/")]
            original_prefix = root
            relative = value[len(root) :].strip("/")
            for part in (part for part in relative.split("/") if part):
                original_prefix += "/" + part
                safe_parts.append(
                    self.prefix_map.get(
                        original_prefix,
                        self.basename_map.get(part, self.name_map.get(part, "entry9999")),
                    )
                )
            return "/" + "/".join(safe_parts)
        parts = value.split("/")
        if len(parts) < 2:
            return value
        root = parts[1]
        if not root:
            return "/fixture"
        safe_root = root if VOLUME_RE.fullmatch(root) else "fixture"
        original_prefix = "/" + root
        safe_parts = [safe_root]
        for part in (part for part in parts[2:] if part):
            original_prefix += "/" + part
            safe_parts.append(
                self.prefix_map.get(
                    original_prefix,
                    self.basename_map.get(part, self.name_map.get(part, "entry9999")),
                )
            )
        return "/" + "/".join(safe_parts)

    def _text(self, value: str, key: str = "") -> str:
        for original in sorted(self.paths, key=len, reverse=True):
            value = value.replace(original, self.path(original))
        if key in TEXT_KEYS:
            text_names = self.basename_map | self.name_map
            for original, replacement in sorted(
                text_names.items(), key=lambda item: len(item[0]), reverse=True
            ):
                if original:
                    value = re.sub(
                        rf"(?<![A-Za-z0-9_.-]){re.escape(original)}(?![A-Za-z0-9_.-])",
                        replacement,
                        value,
                    )
            for originals, replacement in (
                (self.usernames, "admin"),
                (self.hostnames, "nas-fixture"),
                (self.serials, "FIXTURE-SERIAL-001"),
            ):
                for original in sorted(originals, key=len, reverse=True):
                    if original:
                        value = re.sub(
                            rf"(?<![A-Za-z0-9_.-]){re.escape(original)}(?![A-Za-z0-9_.-])",
                            replacement,
                            value,
                            flags=re.I,
                        )
        value = INTERNAL_HOST_RE.sub("nas-fixture", value)
        if INTERNAL_NAMES_RE is not None:
            value = INTERNAL_NAMES_RE.sub("nas-fixture", value)
        value = IPV4_RE.sub(
            lambda match: (
                "192.0.2.1"
                if all(int(part) <= 255 for part in match.group(0).split("."))
                else match.group(0)
            ),
            value,
        )
        value = UUID_RE.sub(lambda match: self.uuid_map.get(match.group(0), "fixture-uuid"), value)

        def replace_path(match: re.Match[str]) -> str:
            token = match.group(0).rstrip(".,;:)]}")
            suffix = match.group(0)[len(token) :]
            return self.path(token) + suffix

        return PATH_RE.sub(replace_path, value)

    def clean(self, value: Any, key: str = "", parent_key: str = "") -> Any:
        if isinstance(value, dict):
            result = {
                child_key: self.clean(child, child_key.lower(), key)
                for child_key, child in value.items()
                if child_key.lower() not in {"path_hex", "_path_hex"}
            }
            if "path_hex" in value and isinstance(value.get("path"), str):
                safe_path = self.clean(value["path"], "path")
                result["path_hex"] = safe_path.encode("utf-8").hex()
            if (
                "job_id" in result
                and "kind" not in result
                and isinstance(result.get("targets"), list)
                and result.get("phase") != "execute"
                and (
                    result.get("scope") is not None
                    or isinstance(result.get("progress"), dict)
                )
            ):
                # Older CLI captures omitted the explicit kind field. Scan
                # jobs have a scope or scan progress; cleanup runs do not.
                result["kind"] = "scan"
            return result
        if isinstance(value, list):
            return [self.clean(child, key, parent_key) for child in value]
        if not isinstance(value, str):
            return value
        if key == "target_volume" or (key == "path" and parent_key == "targets"):
            try:
                decoded_path = binascii.unhexlify(value).decode("utf-8")
            except (binascii.Error, UnicodeDecodeError):
                decoded_path = ""
            if decoded_path.startswith("/"):
                return self.path(decoded_path).encode("utf-8").hex()
        if key in PATH_KEYS and value.startswith("/"):
            return self.path(value)
        if key == "path" and parent_key == "share" and value:
            return self.path("/volume1/" + value.lstrip("/"))
        if key == "requested_by" and value in RANK_KEYS:
            return value
        if key in USER_KEYS:
            return "admin"
        if key in HOST_KEYS:
            return "nas-fixture"
        if key in SERIAL_KEYS:
            return "FIXTURE-SERIAL-001"
        if key == "uuid" and value in self.hex_uuid_map:
            return self.hex_uuid_map[value]
        if key in UUID_KEYS or UUID_RE.fullmatch(value):
            if value in self.hex_uuid_map and "-" not in value:
                return self.hex_uuid_map[value]
            return self.uuid_map.get(value, "f0000000-0000-4000-8000-000000000001")
        if key == "job_id":
            return self.job_map.get(value, "fixture-job-001")
        if key == "plan_id":
            return self.plan_map.get(value, "fixture-plan-001")
        if key in NAME_KEYS:
            return self.basename_map.get(value, self.name_map.get(value, "name9999"))
        if key in PATH_KEYS and value:
            return self.basename_map.get(value, self.name_map.get(value, "name9999"))
        return self._text(value, key)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument(
        "--volume-root",
        action="append",
        default=[],
        help="source Btrfs mount to normalize to /volume1 (repeat for more mounts)",
    )
    parser.add_argument("inputs", nargs="+", type=Path)
    args = parser.parse_args()

    inputs = sorted(args.inputs, key=lambda path: str(path))
    documents = [json.loads(path.read_text(encoding="utf-8")) for path in inputs]
    sanitizer = Sanitizer(documents, args.volume_root)
    args.output_dir.mkdir(parents=True, exist_ok=True)
    for source, document in zip(inputs, documents, strict=True):
        destination = args.output_dir / source.name
        destination.write_text(
            json.dumps(sanitizer.clean(document), ensure_ascii=False, indent=2) + "\n",
            encoding="utf-8",
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
