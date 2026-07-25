#!/bin/zsh
set -euo pipefail

readonly git_subcommand="${1:-}"

case "$git_subcommand" in
  --help | --version | add | am | apply | blame | branch | checkout | cherry-pick | clean | clone | commit | describe | diff | fetch | format-patch | grep | help | init | log | merge | merge-base | mv | notes | pull | push | range-diff | rebase | reflog | remote | reset | restore | revert | rev-list | rev-parse | rm | show | show-ref | sparse-checkout | stash | status | switch | tag | worktree) ;;
  *)
    print -u2 -- "Git subcommand requires normal Codex approval: ${git_subcommand:-<missing>}"
    exit 64
    ;;
esac

if [[ "$git_subcommand" == "push" ]]; then
  for argument in "${@:2}"; do
    case "$argument" in
      --delete | --force | --force=* | --force-with-lease | --force-with-lease=* | --force-if-includes | --mirror | --prune | +* | :* | *:)
        print -u2 -- "Destructive git push requires normal Codex approval: $argument"
        exit 64
        ;;
      -o=* | --push-option=*) ;;
      --*) ;;
      -*)
        short_flags="${argument#-}"
        if [[ "$short_flags" == *[fd]* ]]; then
          print -u2 -- "Destructive git push requires normal Codex approval: $argument"
          exit 64
        fi
        ;;
    esac
  done
fi

export NVM_DIR="${NVM_DIR:-$HOME/.nvm}"

# Git is useful in every governed repository. Load a repository-pinned Node
# version when available, but do not make Git depend on Node or nvm.
readonly repo_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
if [[ -f "$repo_root/.nvmrc" && -s "$NVM_DIR/nvm.sh" ]]; then
  source "$NVM_DIR/nvm.sh"
  nvm use "$(cat "$repo_root/.nvmrc")" >/dev/null
fi

exec git "$@"
