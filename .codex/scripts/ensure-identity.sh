#!/usr/bin/env bash

set -Eeuo pipefail

fail() {
  echo "ERROR: Codex identity setup: $*" >&2
  exit 1
}

command -v git >/dev/null 2>&1 || fail "git is required"

repo_root="$(git rev-parse --show-toplevel 2>/dev/null)" ||
  fail "run the shared Codex environment from a Git worktree"
git_dir="$(git rev-parse --absolute-git-dir 2>/dev/null)" ||
  fail "could not resolve the worktree Git directory"
common_dir="$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null)" ||
  fail "could not resolve the common Git directory"

if [[ "$git_dir" == "$common_dir" ]]; then
  fail "agents must use a linked worktree; refusing to configure a primary checkout"
fi

# The installed location is checked as well as PATH because this script runs in
# whatever shell the environment spawns, and that is usually a non-login,
# non-interactive one -- which reads .zshenv and nothing else. The installer
# registers ~/.local/bin for login shells, so `command -v` finds nothing here
# and the runtime looks missing while its symlink sits in plain view.
setup="${AGENT_BOT_BIN:-agent-bot}"
installed="$HOME/.local/bin/agent-bot"
if command -v "$setup" >/dev/null 2>&1; then
  "$setup" setup-worktree
elif [[ -n "${AGENT_BOT_BIN:-}" ]]; then
  # An explicit override is a decision, not a hint. Falling back to the default
  # install would configure the worktree through a different bot than the one
  # named -- quietly, and with a plausible-looking success message.
  fail "AGENT_BOT_BIN is set to '$AGENT_BOT_BIN' but it is not executable"
elif [[ -x "$installed" ]]; then
  "$installed" setup-worktree
elif [[ -n "${AGENT_BOT_HOME:-}" && -f "$AGENT_BOT_HOME/setup-worktree.mjs" ]]; then
  node "$AGENT_BOT_HOME/setup-worktree.mjs"
else
  fail "agent-bot is not installed; install it from the agent-bot-identity runtime repository"
fi

agent_id="$(git config --worktree --get agentBot.agentId 2>/dev/null || true)"
agent_app="$(git config --worktree --get agentBot.app 2>/dev/null || true)"
author="$(git config --worktree --get user.name 2>/dev/null || true)"
helper="$(git config --worktree --get-all credential.helper 2>/dev/null | tail -n 1 || true)"
hooks="$(git config --worktree --path --get core.hooksPath 2>/dev/null || true)"

[[ -n "$agent_id" ]] || fail "setup completed without an agentBot.agentId"
[[ -n "$agent_app" ]] || fail "setup completed without an agentBot.app pin"
[[ "$author" == "$agent_app[bot]" ]] ||
  fail "Git author $author does not match $agent_app[bot]"
[[ "$helper" == *" $agent_app" ]] ||
  fail "credential helper does not resolve the pinned App $agent_app"
[[ -x "$hooks/prepare-commit-msg" ]] ||
  fail "core.hooksPath does not provide the execution-identity commit hook"

echo "==> Codex identity: $agent_app[bot] as $agent_id"
