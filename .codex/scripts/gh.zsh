#!/bin/zsh
set -euo pipefail

readonly gh_group="${1:-}"
readonly gh_action="${2:-}"
typeset -i return_code=0

case "$gh_group" in
  --help | --version | browse | help | status) ;;
  auth)
    case "$gh_action" in
      setup-git | status | switch) ;;
      *) return_code=64 ;;
    esac
    ;;
  cache)
    case "$gh_action" in
      list) ;;
      *) return_code=64 ;;
    esac
    ;;
  codespace)
    case "$gh_action" in
      code | cp | jupyter | list | logs | ports | ssh | view) ;;
      *) return_code=64 ;;
    esac
    ;;
  gist)
    case "$gh_action" in
      clone | list | view) ;;
      *) return_code=64 ;;
    esac
    ;;
  issue)
    case "$gh_action" in
      close | comment | create | develop | edit | list | lock | pin | reopen | status | transfer | unlock | unpin | view) ;;
      *) return_code=64 ;;
    esac
    ;;
  label)
    case "$gh_action" in
      list) ;;
      *) return_code=64 ;;
    esac
    ;;
  pr)
    case "$gh_action" in
      checkout | checks | close | comment | create | diff | edit | list | lock | merge | ready | reopen | review | status | unlock | update-branch | view) ;;
      *) return_code=64 ;;
    esac
    ;;
  project)
    case "$gh_action" in
      item-list | list | view) ;;
      *) return_code=64 ;;
    esac
    ;;
  release)
    case "$gh_action" in
      download | list | verify | verify-asset | view) ;;
      *) return_code=64 ;;
    esac
    ;;
  repo)
    case "$gh_action" in
      clone | list | set-default | view) ;;
      *) return_code=64 ;;
    esac
    ;;
  ruleset)
    case "$gh_action" in
      check | list | view) ;;
      *) return_code=64 ;;
    esac
    ;;
  run)
    case "$gh_action" in
      download | list | rerun | view | watch) ;;
      *) return_code=64 ;;
    esac
    ;;
  search)
    case "$gh_action" in
      code | commits | issues | prs | repos) ;;
      *) return_code=64 ;;
    esac
    ;;
  secret)
    case "$gh_action" in
      list) ;;
      *) return_code=64 ;;
    esac
    ;;
  ssh-key)
    case "$gh_action" in
      list) ;;
      *) return_code=64 ;;
    esac
    ;;
  variable)
    case "$gh_action" in
      get | list) ;;
      *) return_code=64 ;;
    esac
    ;;
  workflow)
    case "$gh_action" in
      list | view) ;;
      *) return_code=64 ;;
    esac
    ;;
  *) return_code=64 ;;
esac

if [[ "$return_code" -ne 0 ]]; then
  print -u2 -- "GitHub CLI command requires normal Codex approval: ${gh_group:-<missing>} ${gh_action}"
  exit "$return_code"
fi

if [[ -d "/opt/homebrew/bin" ]]; then
  export PATH="/opt/homebrew/bin:$PATH"
fi

readonly gh_executable="$(command -v gh || true)"

if [[ -z "$gh_executable" ]]; then
  print -u2 -- "GitHub CLI (gh) was not found on PATH"
  exit 127
fi

exec "$gh_executable" "$@"
