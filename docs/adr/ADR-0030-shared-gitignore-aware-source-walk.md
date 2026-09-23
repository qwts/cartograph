# ADR-0030 — One shared, `.gitignore`-aware source walk

- **Status:** Accepted for #248
- **Date:** 2026-09-22
- **Deciders:** Cartograph owner (issue direction), Cartograph implementation agent

## Context

Every extractor (TS/JS, Python, Go, Java, Kotlin, Terraform, plugin routing,
ADR recovery) and Preflight/toolchain detection walked the tree with its own
`read_dir` loop and a hard-coded skip list. None consulted `.gitignore`, so a
repository's `build/`, `out/` or `vendor/` output was ingested as first-class
source: wrong facts and wasted extraction time. The walks also disagreed with
each other in small ways: symlink handling and traversal order differed.

## Decision

Add a `source-walk` crate that every walker uses. It applies each directory's
`.gitignore` with git's precedence (innermost file decides, `!` re-includes),
on top of each caller's existing fixed skips. Those skips remain the fallback
for trees without ignore rules. The walk never follows symlinks and returns
sorted repo-relative paths. It uses the `ignore` crate's gitignore matcher
(MIT/Unlicense). It does not use that crate's walker, so traversal, ordering
and symlink policy stay under our control.

Only `.gitignore` files inside the walked root count. Rules apply with or
without a `.git` directory. Ancestor `.gitignore` files, `.git/info/exclude`
and the user's global excludes file are machine-local, so they are never
consulted. The same commit therefore selects the same files on every machine
(US-0014).

The captured lane enumerates through confined no-follow directory handles.
It shares the matching (`IgnoreRules`) rather than the walk, reading each
`.gitignore` through the same handle. Captured and ordinary selections are
therefore identical by construction. Env-file indexing (`events`) deliberately
disregards `.gitignore`, because local `.env` files are conventionally
ignored yet are its input.

## Consequences

Repositories whose `.gitignore` excludes source-looking files produce fewer
facts, and their graph hash changes once. Files a repository tracks despite a
matching ignore rule are excluded too, because the walk reads the filesystem,
not the git index. Symlinked files and directories, previously followed by
some language walkers, are no longer ingested by the ordinary lane (the
captured lane already rejected them). A `.gitignore` larger than 1 MiB fails
the walk rather than being truncated. Matching is case-sensitive everywhere. Unlike git
with `core.ignorecase`, it does not vary by platform.

## Alternatives

- `ignore::WalkBuilder` for the ordinary lane: its defaults (git-only,
  parent/global/exclude files, hidden-file skipping) needed several overrides,
  and the captured lane could not use it, so the two lanes could drift apart.
- Extending each hard-coded skip list: this cannot know a repository's own
  output directories.
- Asking git for the tracked-file list: this does not work for non-git and
  captured trees and adds a process or libgit2 dependency to every adapter.
