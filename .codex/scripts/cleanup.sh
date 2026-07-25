#!/usr/bin/env bash

set -u

echo "==> Codex cleanup starting"

# Run from repo root when possible.
if git rev-parse --show-toplevel >/dev/null 2>&1; then
  cd "$(git rev-parse --show-toplevel)"
fi

echo "==> Working directory: $(pwd)"

# Remove transient project-local output.
# Do not remove source files, lockfiles, package manifests, or repo config.
echo "==> Removing transient outputs"

rm -rf \
  tmp \
  temp \
  .tmp \
  dist \
  .test-dist \
  coverage \
  playwright-report \
  test-results \
  .eslintcache \
  tsconfig.tsbuildinfo

# Remove OS/editor noise.
find . \
  \( -name ".DS_Store" -o -name "Thumbs.db" -o -name "*.swp" -o -name "*.swo" \) \
  -type f \
  -delete 2>/dev/null || true

echo "==> Codex cleanup complete"

exit 0
