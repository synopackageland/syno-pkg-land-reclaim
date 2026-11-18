#!/usr/bin/env python3
"""Exercise the SPK command boundary without compiling Rust or contacting a NAS."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class BuildSpkTests(unittest.TestCase):
    def test_npm_forwards_options_and_propagates_a_reproducibility_failure(self):
        for status in (0, 23):
            with self.subTest(cli_status=status), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                (root / 'package').mkdir()
                (root / 'bin').mkdir()
                (root / 'package.json').write_bytes((ROOT / 'package.json').read_bytes())
                (root / 'package/verify-cli.py').write_text(
                    "from pathlib import Path\nPath('trace').write_text('verify\\n')\n")
                prepare = root / 'package/build-root-programs.sh'
                prepare.write_text('#!/bin/sh\nprintf "root\\n" >> trace\nexit "${PREPARE_STATUS:-0}"\n')
                prepare.chmod(0o755)
                cli = root / 'bin/synopkgland'
                cli.write_text('#!/bin/sh\nprintf "%s\\n" "$@" >> trace\nif [ "$1" = build ]; then exit "$CLI_STATUS"; fi\n')
                cli.chmod(0o755)
                result = subprocess.run(
                    ['npm', 'run', 'build:spk', '--', '--json'], cwd=root,
                    env={**os.environ, 'PATH': f"{root / 'bin'}:{os.environ['PATH']}",
                         'CLI_STATUS': str(status)}, capture_output=True, text=True)
                self.assertEqual(result.returncode, status, result.stdout + result.stderr)
                self.assertEqual((root / 'trace').read_text().splitlines(), [
                    'verify', 'root', 'ctl', 'generate', '--check', 'check', '--json', 'build', '--jobs', '3',
                    '--verify-reproducible', 'always', '--json'])
        manifest = json.loads((ROOT / 'dsm-package.yaml').read_text())
        self.assertEqual(manifest['arches'], ['x86_64', 'aarch64', 'armv7'])

    def test_package_check_prepares_all_root_inputs_and_propagates_a_refusal(self):
        for command, status, prepare_status in [('check:package', 0, 0), ('check:package', 17, 0), ('build:spk', 17, 0), ('build:spk', 0, 19)]:
            with self.subTest(command=command, check_status=status, prepare_status=prepare_status), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                (root / 'package').mkdir()
                (root / 'bin').mkdir()
                (root / 'package.json').write_bytes((ROOT / 'package.json').read_bytes())
                (root / 'package/verify-cli.py').write_text("from pathlib import Path\nPath('trace').write_text('verify\\n')\n")
                prepare = root / 'package/build-root-programs.sh'
                prepare.write_text('#!/bin/sh\nprintf "root\\n" >> trace\nexit "${PREPARE_STATUS:-0}"\n')
                prepare.chmod(0o755)
                cli = root / 'bin/synopkgland'
                cli.write_text('#!/bin/sh\nprintf "%s\\n" "$@" >> trace\nexit "$CHECK_STATUS"\n')
                cli.chmod(0o755)
                result = subprocess.run(['npm', 'run', command], cwd=root,
                    env={**os.environ, 'PATH': f"{root / 'bin'}:{os.environ['PATH']}", 'CHECK_STATUS': str(status), 'PREPARE_STATUS': str(prepare_status)},
                    capture_output=True, text=True)
                self.assertEqual(result.returncode, prepare_status or status, result.stdout + result.stderr)
                expected = ['verify', 'root'] + ([] if prepare_status else ['ctl', 'generate', '--check'] + ([] if status else ['check', '--json']))
                self.assertEqual((root / 'trace').read_text().splitlines(), expected)

    def test_reproducibility_rebuild_removes_the_rust_app_cache(self):
        manifest = json.loads((ROOT / 'dsm-package.yaml').read_text())
        self.assertIn('app/target', manifest['reproducible']['clean'])

    def test_docker_package_gate_uses_the_same_build_entry_point(self):
        inner = (ROOT / 'scripts/gate/inner.sh').read_text()
        package = inner.split('package() {', 1)[1].split('\n}', 1)[0]
        self.assertIn('npm run build:spk -- --json', package)
        self.assertNotIn('synopkgland build', package)
        self.assertNotIn('for arch in', package)
        self.assertNotIn('synopkgland check', package)
        self.assertNotIn('package/build-root-programs.sh', package)
        self.assertIn('synopkgland inspect', package)
        self.assertIn('package/tests/test-build-spk.py', package)


if __name__ == '__main__':
    unittest.main()
