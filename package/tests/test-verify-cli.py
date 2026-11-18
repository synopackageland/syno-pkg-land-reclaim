#!/usr/bin/env python3
"""Verify the pinned CLI against the public registry and installed build inputs."""
import importlib.util
import json
from pathlib import Path
import unittest
import tempfile
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('verify_cli', ROOT / 'package/verify-cli.py')
verify_cli = importlib.util.module_from_spec(spec)
spec.loader.exec_module(verify_cli)


class VerifyCliTests(unittest.TestCase):
    def test_patch_range_preserves_exact_locked_cli_identity(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / 'package').mkdir()
            record = json.loads((ROOT / 'package/cli-source.json').read_text())
            manifest = json.loads((ROOT / 'package.json').read_text())
            lock = json.loads((ROOT / 'package-lock.json').read_text())
            version = record['version']
            for declaration in (version, '~' + version, '^' + version, '~0.51.0'):
                manifest['devDependencies']['@synopackageland/cli'] = declaration
                lock['packages']['']['devDependencies']['@synopackageland/cli'] = declaration
                (root / 'package/cli-source.json').write_text(json.dumps(record))
                (root / 'package.json').write_text(json.dumps(manifest))
                (root / 'package-lock.json').write_text(json.dumps(lock))
                with self.subTest(declaration=declaration), patch.object(verify_cli, 'ROOT', root):
                    if declaration in (version, '~' + version):
                        verify_cli.verify()
                    else:
                        with self.assertRaises(RuntimeError):
                            verify_cli.verify()
            manifest['devDependencies']['@synopackageland/cli'] = '~' + version
            lock['packages']['']['devDependencies']['@synopackageland/cli'] = '~' + version
            lock['packages']['node_modules/@synopackageland/cli']['integrity'] = 'substituted'
            (root / 'package.json').write_text(json.dumps(manifest))
            (root / 'package-lock.json').write_text(json.dumps(lock))
            with patch.object(verify_cli, 'ROOT', root), self.assertRaises(RuntimeError):
                verify_cli.verify()

    def test_published_cli_is_pinned_from_the_public_registry(self):
        record = json.loads((ROOT / 'package/cli-source.json').read_text())
        manifest = json.loads((ROOT / 'package.json').read_text())
        version = manifest['devDependencies']['@synopackageland/cli']
        self.assertRegex(version, r'^~?\d+\.\d+\.\d+$')
        self.assertEqual(record['version'], version.removeprefix('~'))
        version = record['version']
        self.assertEqual(record['tarball'], f'https://registry.npmjs.org/@synopackageland/cli/-/cli-{version}.tgz')
        verify_cli.verify()


if __name__ == '__main__':
    unittest.main()
