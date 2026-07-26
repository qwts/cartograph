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

if [[ -z "${CODEX_THREAD_ID:-}" ]] &&
  { [[ -z "${QWTS_AGENT_TRANSCRIPT_PROVIDER:-}" ]] ||
    [[ -z "${QWTS_AGENT_TRANSCRIPT_ID:-}" ]]; }; then
  fail "no Codex transcript locator is available; refusing a transcript-pending identity"
fi

if [[ -f "$repo_root/tools/agent-bot/setup-worktree.mjs" ]]; then
  playbook_root="$repo_root"
else
  playbook_root="${PLAYBOOK_HOME:-$HOME/Code/playbook-engineering}"
fi

setup="$playbook_root/tools/agent-bot/setup-worktree.mjs"
[[ -f "$setup" ]] ||
  fail "setup-worktree.mjs was not found under $playbook_root; set PLAYBOOK_HOME to the canonical checkout"

node "$setup"

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
