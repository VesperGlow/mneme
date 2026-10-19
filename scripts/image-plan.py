#!/usr/bin/env python3
"""Choose builds from image inputs, comparing against the last successful release."""
import json
import os
from pathlib import Path
import subprocess
import urllib.request

IMAGES = [
    {'image': 'mneme', 'suffix': '-api', 'file': 'Containerfile'},
    {'image': 'mneme-tools', 'suffix': '-tools', 'file': 'services/tools/Containerfile'},
    {'image': 'mneme-models', 'suffix': '-models', 'file': 'services/models/Containerfile'},
]


def selected(paths):
    result = set()
    for path in paths:
        if path == '.dockerignore':
            return {item['image'] for item in IMAGES}
        if path in {'Containerfile', 'Cargo.toml', 'Cargo.lock', 'config/default.toml', 'services/tools/catalog.json'} or path.startswith(('src/', 'web/', 'migrations/')):
            result.add('mneme')
        if path.startswith('services/tools/') or path in {'scripts/smoke-tools.sh', 'scripts/smoke-tools.mjs', 'deploy/quadlet/mneme-tools-seccomp.json'}:
            result.add('mneme-tools')
        if path.startswith('services/models/'):
            result.add('mneme-models')
    return result


def base_revision(event, name, environ):
    if name == 'pull_request':
        return event['pull_request']['base']['sha']
    if name != 'push':
        return None  # Manual runs rebuild every image.
    # Comparing only the push's `before` can miss a failed or cancelled release.
    repository = environ['GITHUB_REPOSITORY']
    url = f'https://api.github.com/repos/{repository}/actions/workflows/check.yml/runs?branch=main&event=push&status=success&per_page=10'
    headers = {'Accept': 'application/vnd.github+json', 'User-Agent': 'mneme-image-plan'}
    if environ.get('GITHUB_TOKEN'):
        headers['Authorization'] = f"Bearer {environ['GITHUB_TOKEN']}"
    try:
        with urllib.request.urlopen(urllib.request.Request(url, headers=headers), timeout=20) as response:
            runs = json.load(response)['workflow_runs']
        for run in runs:
            if run['id'] < int(environ['GITHUB_RUN_ID']):
                return run['head_sha']
    except (OSError, ValueError, KeyError):
        pass
    return None  # Missing history or an API outage must not skip required builds.


def plan(event, name, environ):
    base = base_revision(event, name, environ)
    changed = {item['image'] for item in IMAGES}
    if base:
        try:
            subprocess.run(['git', 'merge-base', '--is-ancestor', base, 'HEAD'], check=True, capture_output=True)
            paths = subprocess.check_output(['git', 'diff', '--name-only', '--no-renames', '-z', base, 'HEAD']).decode().split('\0')
            changed = selected(paths)
        except subprocess.CalledProcessError:
            pass
    return {'include': [dict(item, build=item['image'] in changed) for item in IMAGES]}


def main():
    event = json.loads(Path(os.environ['GITHUB_EVENT_PATH']).read_text())
    matrix = plan(event, os.environ['GITHUB_EVENT_NAME'], os.environ)
    encoded = json.dumps(matrix, separators=(',', ':'))
    with open(os.environ['GITHUB_OUTPUT'], 'a') as output:
        output.write(f'matrix={encoded}\n')
    print(json.dumps(matrix, indent=2))


if __name__ == '__main__':
    main()
