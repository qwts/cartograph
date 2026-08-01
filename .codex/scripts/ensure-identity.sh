#!/usr/bin/env bash

set -Eeuo pipefail

fail() {
  echo "ERROR: Codex identity setup: $*" >&2
  exit 1
}

command -v git >/dev/null 2>&1 || fail "git is required"
command -v node >/dev/null 2>&1 || fail "Node.js is required"

repo_root="$(git rev-parse --show-toplevel 2>/dev/null)" ||
  fail "run the shared Codex environment from a Git worktree"
git_dir="$(git rev-parse --absolute-git-dir 2>/dev/null)" ||
  fail "could not resolve the worktree Git directory"
common_dir="$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null)" ||
  fail "could not resolve the common Git directory"

if [[ "$git_dir" == "$common_dir" ]]; then
  fail "agents must use a linked worktree; refusing to configure a primary checkout"
fi

setup="${HOME}/.local/bin/playbook-setup-worktree"
[[ -x "$setup" ]] ||
  fail "playbook-setup-worktree is not installed; run playbook-engineering install from a clean playbook-engineering checkout"

"$setup"

agent_id="$(git config --worktree --get qwts.agentId 2>/dev/null || true)"
agent_app="$(git config --worktree --get qwts.agentApp 2>/dev/null || true)"
author="$(git config --worktree --get user.name 2>/dev/null || true)"
helper="$(git config --worktree --get-all credential.helper 2>/dev/null | tail -n 1 || true)"
hooks="$(git config --worktree --path --get core.hooksPath 2>/dev/null || true)"

[[ -n "$agent_id" ]] || fail "setup completed without a qwts.agentId"
[[ -n "$agent_app" ]] || fail "setup completed without a qwts.agentApp pin"
[[ "$author" == "$agent_app[bot]" ]] ||
  fail "Git author $author does not match $agent_app[bot]"
[[ "$helper" == *" $agent_app" ]] ||
  fail "credential helper does not resolve the pinned App $agent_app"
[[ -x "$hooks/prepare-commit-msg" ]] ||
  fail "core.hooksPath does not provide the execution-identity commit hook"

echo "==> Codex identity: $agent_app[bot] as $agent_id"
