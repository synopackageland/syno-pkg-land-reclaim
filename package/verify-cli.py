#!/usr/bin/env python3
"""Verify the published CLI lockfile pin and critical installed build inputs."""
import argparse
import hashlib
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def verify(installed=False):
    record = json.loads((ROOT / 'package/cli-source.json').read_text())
    manifest = json.loads((ROOT / 'package.json').read_text())
    lock = json.loads((ROOT / 'package-lock.json').read_text())
    name = '@synopackageland/cli'
    declaration = manifest['devDependencies'][name]
    if declaration not in (record['version'], '~' + record['version']):
        raise RuntimeError('CLI dependency must select the recorded release or its patch range')
    if lock['packages']['']['devDependencies'][name] != declaration:
        raise RuntimeError('CLI declaration and lockfile mismatch')
    entry = lock['packages']['node_modules/' + name]
    if (entry['version'], entry['resolved'], entry['integrity']) != (
        record['version'], record['tarball'], record['integrity']
    ):
        raise RuntimeError('CLI npm version, URL or integrity mismatch')
    # npm ci verifies every archive byte against the lockfile's SHA-512 SRI.
    # Also reject stale/substituted installed build and inspection entry points.
    if not installed:
        return
    package = ROOT / 'node_modules/@synopackageland/cli'
    if json.loads((package / 'package.json').read_text())['version'] != record['version']:
        raise RuntimeError('installed CLI version mismatch; run npm ci')
    for relative, expected in record['verifiedInstalledFiles'].items():
        target = package / relative
        if not target.is_file() or target.is_symlink() or hashlib.sha256(target.read_bytes()).hexdigest() != expected:
            raise RuntimeError('installed CLI differs from published build; run npm ci')


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--installed', action='store_true')
    verify(parser.parse_args().installed)
