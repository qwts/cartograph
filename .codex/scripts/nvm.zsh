#!/bin/zsh
set -euo pipefail

readonly nvm_subcommand="${1:-}"

case "$nvm_subcommand" in
  --help | --version | alias | cache | current | deactivate | help | install | ls | ls-remote | unalias | unload | use | version | version-remote | which) ;;
  *)
    print -u2 -- "NVM subcommand requires normal Codex approval: ${nvm_subcommand:-<missing>}"
    exit 64
    ;;
esac

export NVM_DIR="${NVM_DIR:-$HOME/.nvm}"

if [[ ! -s "$NVM_DIR/nvm.sh" ]]; then
  print -u2 -- "nvm not found at $NVM_DIR/nvm.sh"
  exit 127
fi

source "$NVM_DIR/nvm.sh"
nvm "$@"
