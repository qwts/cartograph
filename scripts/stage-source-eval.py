#!/usr/bin/env python3
"""Stage selected immutable Git blobs without executing target project code.

Usage: stage-source-eval.py CHECKOUT INPUT_MANIFEST NEW_DESTINATION
The manifest specifies repo, commit_sha and source_files; expected labels/tests
belong outside this input. Sources come from git show at the exact commit,
never from uncommitted target changes or followed target symlinks.
"""

import hashlib
import json
from pathlib import Path, PurePosixPath
import re
import subprocess
import sys


def main() -> None:
    if len(sys.argv) != 4:
        raise SystemExit(__doc__)
    checkout, manifest_path, destination = map(Path, sys.argv[1:])
    manifest = json.loads(manifest_path.read_text())
    commit = manifest["commit_sha"]
    if not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise SystemExit("manifest must pin a full Git commit SHA")
    paths = manifest["source_files"]
    if not paths or len(paths) != len(set(paths)):
        raise SystemExit("manifest requires unique production source paths")
    for name in paths:
        path = PurePosixPath(name)
        if path.is_absolute() or ".." in path.parts or str(path) != name:
            raise SystemExit("source path must be normalized and repository-relative")
        if path.suffix not in {".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs"}:
            raise SystemExit("input must contain TypeScript/JavaScript source files")
        if re.search(r"(?:\.spec|\.test)\.[^.]+$", name) or "__tests__" in path.parts:
            raise SystemExit("upstream tests belong outside extraction input")
    # Validate every selected object before creating the staging destination.
    blobs = {}
    for name in sorted(paths):
        result = subprocess.run(
            ["git", "-C", str(checkout), "show", f"{commit}:{name}"],
            check=True, capture_output=True,
        )
        blobs[name] = result.stdout
    destination.mkdir(parents=True, exist_ok=False)
    hashes = {}
    for name, data in blobs.items():
        target = destination / name
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data)
        hashes[name] = hashlib.sha256(data).hexdigest()
    print(json.dumps({
        "repo": manifest["repo"], "commit_sha": commit,
        "manifest_sha256": hashlib.sha256(manifest_path.read_bytes()).hexdigest(),
        "files_sha256": hashes,
    }, sort_keys=True, indent=2))


if __name__ == "__main__":
    main()
