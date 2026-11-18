from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from record import add_fixture
from sanitize import Sanitizer


class CaptureRecordingTests(unittest.TestCase):
    def test_specific_params_precede_a_broad_fallback(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            add_fixture(
                root,
                "snapshots.list",
                "volume.json",
                {"completion": "complete"},
                params={"target": "/volume1"},
            )
            add_fixture(
                root,
                "snapshots.list",
                "simulation.json",
                {"completion": "complete"},
                params={"target": "/volume1", "simulate": "id:42"},
            )

            entries = json.loads(
                (root / "snapshots.list" / "index.json").read_text()
            )

        self.assertEqual(
            [entry["file"] for entry in entries],
            ["simulation.json", "volume.json"],
        )

    def test_path_arrays_and_ipv4_values_are_sanitized(self) -> None:
        original = "/volume1/PrivateShare/PrivateFolder"
        sanitizer = Sanitizer(
            [
                {
                    "paths": [original],
                    "message": f"Host 198.51.100.7 referenced {original}",
                }
            ],
            ["/volume1"],
        )

        safe = sanitizer.clean(
            {
                "paths": [original],
                "message": f"Host 198.51.100.7 referenced {original}",
            }
        )

        self.assertEqual(safe["paths"][0], "/volume1/share01/entry0001")
        self.assertIn("192.0.2.1", safe["message"])
        self.assertNotIn("PrivateShare", json.dumps(safe))
        self.assertNotIn("198.51.100.7", json.dumps(safe))

    def test_relative_recycle_share_matches_its_absolute_target(self) -> None:
        sanitizer = Sanitizer(
            [
                {"shares": [{"share": {"path": "PrivateShare/Nested"}}]},
                {"target": {"path": "/volume1/PrivateShare/Nested"}},
            ],
            ["/volume1"],
        )

        share = sanitizer.clean(
            {"shares": [{"share": {"path": "PrivateShare/Nested"}}]}
        )["shares"][0]["share"]["path"]
        target = sanitizer.clean(
            {"target": {"path": "/volume1/PrivateShare/Nested"}}
        )["target"]["path"]

        self.assertEqual(target, share)

    def test_hex_encoded_plan_paths_stay_valid_and_are_sanitized(self) -> None:
        volume = "/volume1"
        target = "/volume1/PrivateShare/PrivateFolder"
        sanitizer = Sanitizer(
            [
                {
                    "paths": [{"path": target}],
                    "plan": {
                        "target_volume": volume.encode().hex(),
                        "targets": [{"path": target.encode().hex()}],
                    },
                }
            ],
            [volume],
        )

        safe = sanitizer.clean(
            {
                "paths": [{"path": target}],
                "plan": {
                    "target_volume": volume.encode().hex(),
                    "targets": [{"path": target.encode().hex()}],
                },
            }
        )

        self.assertEqual(bytes.fromhex(safe["plan"]["target_volume"]).decode(), volume)
        self.assertEqual(
            bytes.fromhex(safe["plan"]["targets"][0]["path"]).decode(),
            safe["paths"][0]["path"],
        )
        self.assertNotIn("PrivateShare", json.dumps(safe))

    def test_old_scan_job_capture_gets_its_schema_kind(self) -> None:
        sanitizer = Sanitizer(
            [
                {
                    "jobs": [
                        {
                            "job_id": "job-123",
                            "scope": None,
                            "phase": "discover",
                            "targets": [],
                            "progress": {},
                        }
                    ]
                }
            ],
            ["/volume1"],
        )

        safe = sanitizer.clean(
            {
                "jobs": [
                    {
                        "job_id": "job-123",
                        "scope": None,
                        "phase": "discover",
                        "targets": [],
                        "progress": {},
                    }
                ]
            }
        )

        self.assertEqual(safe["jobs"][0]["kind"], "scan")


if __name__ == "__main__":
    unittest.main()
