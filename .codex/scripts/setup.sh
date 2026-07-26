#!/usr/bin/env bash

set -Eeuo pipefail

echo "==> Codex setup starting"

# Codex should already run from the checked-out repo, but this keeps it safe.
if git rev-parse --show-toplevel >/dev/null 2>&1; then
  cd "$(git rev-parse --show-toplevel)"
fi

echo "==> Working directory: $(pwd)"

if ! command -v git >/dev/null 2>&1; then
  echo "ERROR: git is required but was not found on PATH"
  exit 1
fi

echo "==> Git: $(git --version)"

node_ready=false
package_manager=""

if [ -f ".nvmrc" ]; then
  # Load nvm explicitly because setup scripts are non-interactive shells.
  export NVM_DIR="${NVM_DIR:-$HOME/.nvm}"
  if [ ! -s "$NVM_DIR/nvm.sh" ]; then
    echo "ERROR: .nvmrc exists but nvm was not found at $NVM_DIR/nvm.sh"
    exit 1
  fi

  . "$NVM_DIR/nvm.sh"
  echo "==> Installing/using Node from .nvmrc: $(cat .nvmrc)"
  nvm install
  nvm use

  # Fresh shells select the Node version for their current worktree. Do not
  # mutate nvm's machine-wide default alias: governed worktrees may pin
  # different versions while sharing the same home directory.
  CODEX_DEV_INIT="$HOME/.codex-agent-dev.sh"
  cat > "$CODEX_DEV_INIT" <<'EOF'
export NVM_DIR="${NVM_DIR:-$HOME/.nvm}"
if [ -s "$NVM_DIR/nvm.sh" ]; then
  . "$NVM_DIR/nvm.sh"
  codex_repo_root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
  if [ -n "$codex_repo_root" ] && [ -f "$codex_repo_root/.nvmrc" ]; then
    nvm use "$(cat "$codex_repo_root/.nvmrc")" --silent >/dev/null 2>&1 || true
  fi
  unset codex_repo_root
fi
if [ -d "/opt/homebrew/bin" ]; then
  export PATH="/opt/homebrew/bin:$PATH"
fi
EOF

  for shell_profile in \
    "$HOME/.profile" \
    "$HOME/.bash_profile" \
    "$HOME/.bashrc" \
    "$HOME/.zprofile" \
    "$HOME/.zshrc"; do
    touch "$shell_profile"
    if ! grep -Fqx '. "$HOME/.codex-agent-dev.sh"' "$shell_profile"; then
      printf '\n%s\n' '. "$HOME/.codex-agent-dev.sh"' >> "$shell_profile"
    fi
  done

  . "$CODEX_DEV_INIT"
  node_ready=true
elif [ -f "package.json" ]; then
  if ! command -v node >/dev/null 2>&1; then
    echo "ERROR: package.json exists but node is not available"
    exit 1
  fi
  node_ready=true
else
  echo "==> No Node project detected; skipping Node setup"
fi

if [ "$node_ready" = true ]; then
  echo "==> Node: $(node --version)"
fi

# Git's post-checkout hook is best-effort because worktree creation can happen
# before Codex exports its harness and transcript markers. Retry conclusively
# now that CODEX_THREAD_ID is available; a missing or split identity must stop
# setup before any development or GitHub write can inherit the wrong actor.
if [ -f ".codex/scripts/ensure-identity.sh" ]; then
  bash .codex/scripts/ensure-identity.sh
fi

# These tools improve the agent experience but are host prerequisites, not
# project dependencies. Report them without mutating the host package manager.
if [ -x "/bin/zsh" ]; then
  echo "==> zsh: $(/bin/zsh --version)"
else
  echo "==> zsh: unavailable; protected zsh command wrappers will be skipped"
fi

if command -v gh >/dev/null 2>&1; then
  echo "==> GitHub CLI: $(gh --version | head -n 1)"
else
  echo "==> GitHub CLI: unavailable; install gh to publish repository changes"
fi

run_package_manager() {
  local manager="$1"
  shift

  case "$manager" in
    npm | bun)
      if ! command -v "$manager" >/dev/null 2>&1; then
        echo "ERROR: package manager '$manager' is selected but unavailable"
        exit 1
      fi
      "$manager" "$@"
      ;;
    pnpm | yarn)
      if command -v "$manager" >/dev/null 2>&1; then
        "$manager" "$@"
      elif command -v corepack >/dev/null 2>&1; then
        corepack "$manager" "$@"
      else
        echo "ERROR: package manager '$manager' is selected but neither it nor corepack is available"
        exit 1
      fi
      ;;
    *)
      echo "ERROR: unsupported package manager '$manager'"
      exit 1
      ;;
  esac
}

if [ "$node_ready" = true ] && [ -f "package.json" ]; then
  declared_manager_spec="$(
    node -e 'const value = require("./package.json").packageManager || ""; process.stdout.write(value)'
  )"
  declared_manager="${declared_manager_spec%%@*}"
  lock_manager=""
  lock_manager_count=0

  if [ -f "package-lock.json" ] || [ -f "npm-shrinkwrap.json" ]; then
    lock_manager="npm"
    lock_manager_count=$((lock_manager_count + 1))
  fi
  if [ -f "pnpm-lock.yaml" ]; then
    lock_manager="pnpm"
    lock_manager_count=$((lock_manager_count + 1))
  fi
  if [ -f "yarn.lock" ]; then
    lock_manager="yarn"
    lock_manager_count=$((lock_manager_count + 1))
  fi
  if [ -f "bun.lock" ] || [ -f "bun.lockb" ]; then
    lock_manager="bun"
    lock_manager_count=$((lock_manager_count + 1))
  fi

  if [ "$lock_manager_count" -gt 1 ]; then
    echo "ERROR: multiple package-manager lockfiles found; keep one or declare the intended manager"
    exit 1
  fi

  if [ -n "$declared_manager" ]; then
    case "$declared_manager" in
      npm | pnpm | yarn | bun) ;;
      *)
        echo "ERROR: unsupported packageManager declaration '$declared_manager_spec'"
        exit 1
        ;;
    esac
    if [ -n "$lock_manager" ] && [ "$declared_manager" != "$lock_manager" ]; then
      echo "ERROR: packageManager '$declared_manager' conflicts with the $lock_manager lockfile"
      exit 1
    fi
    package_manager="$declared_manager"
  elif [ -n "$lock_manager" ]; then
    package_manager="$lock_manager"
  else
    package_manager="npm"
  fi

  package_manager_version="$(run_package_manager "$package_manager" --version)"
  echo "==> Package manager: $package_manager ($package_manager_version)"
  case "$package_manager" in
    npm)
      if [ -f "package-lock.json" ] || [ -f "npm-shrinkwrap.json" ]; then
        echo "==> Installing dependencies with npm ci"
        run_package_manager npm ci --no-audit --no-fund
      else
        echo "==> Installing dependencies with npm install"
        run_package_manager npm install --no-audit --no-fund
      fi
      ;;
    pnpm)
      if [ -f "pnpm-lock.yaml" ]; then
        echo "==> Installing dependencies with pnpm --frozen-lockfile"
        run_package_manager pnpm install --frozen-lockfile
      else
        echo "==> Installing dependencies with pnpm"
        run_package_manager pnpm install
      fi
      ;;
    yarn)
      yarn_major="${declared_manager_spec#yarn@}"
      yarn_major="${yarn_major%%.*}"
      if [ -f "yarn.lock" ] && { [ -f ".yarnrc.yml" ] || { [ -n "$yarn_major" ] && [ "$yarn_major" -ge 2 ] 2>/dev/null; }; }; then
        echo "==> Installing dependencies with yarn --immutable"
        run_package_manager yarn install --immutable
      elif [ -f "yarn.lock" ]; then
        echo "==> Installing dependencies with yarn --frozen-lockfile"
        run_package_manager yarn install --frozen-lockfile
      else
        echo "==> Installing dependencies with yarn"
        run_package_manager yarn install
      fi
      ;;
    bun)
      if [ -f "bun.lock" ] || [ -f "bun.lockb" ]; then
        echo "==> Installing dependencies with bun --frozen-lockfile"
        run_package_manager bun install --frozen-lockfile
      else
        echo "==> Installing dependencies with bun"
        run_package_manager bun install
      fi
      ;;
  esac
else
  echo "==> No package.json found; skipping dependency install"
fi

# Browser automation support, only when Playwright is present.
if [ "$node_ready" = true ] && grep -qiE '"@playwright/test"|"playwright"' package.json; then
  echo "==> Playwright detected; installing Chromium (with system deps for fresh containers)"
  if [ -x "node_modules/.bin/playwright" ]; then
    node_modules/.bin/playwright install --with-deps chromium || node_modules/.bin/playwright install chromium || true
  else
    echo "==> Playwright executable unavailable after install; skipping browser setup"
  fi
fi

# Initial build is deliberately FATAL: a broken build should fail setup loudly instead of
# wasting the session.
if [ "$node_ready" = true ] && node -e 'process.exit(require("./package.json").scripts?.build ? 0 : 1)'; then
  echo "==> build script detected; running initial build"
  run_package_manager "$package_manager" run build
fi

echo "==> Codex setup complete"
