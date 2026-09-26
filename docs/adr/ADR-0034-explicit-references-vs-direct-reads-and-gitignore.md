# ADR-0034 — Explicit references override `.gitignore`; direct reads honor it

- **Status:** Accepted
- **Date:** 2026-09-24
- **Deciders:** Cartograph implementation agent

## Context

The shared `.gitignore`-aware walk (ADR-0030) governs **enumeration**: which
files an extractor discovers on its own by walking the tree. Two extractors
instead read a specific file or directory **directly**, named by something
other than the walk, and neither went through `IgnoreRules`:

1. **Terraform local module expansion** (`crates/iac/src/lib.rs`,
   `expand_local_modules` → `collect_direct_tf_files`). A module block with an
   explicit literal `source = "./vendor/x"` is expanded even when `vendor/` is
   gitignored. `terraform_file_count` (and the Preflight/recovery summary that
   used it) walked the tree independently and did not count those files,
   so the reported file count silently undercounted what extraction actually
   read.
2. **`go.mod`** (`crates/adapters-lang-go/src/lib.rs`, `module_path`). It was
   read unconditionally, even when the ingest root's own `.gitignore`
   excludes it.

Neither is a regression from #248/ADR-0030 — before it, nothing honored
`.gitignore` at all. But the two cases are not the same kind of read, and
conflating them either way produces a wrong answer for one of them.

## Decision

Split by why the read happens, not by extractor:

- **An explicit, source-level reference overrides `.gitignore`.** A Terraform
  `module` block's `source` is something the configuration itself names —
  the author deliberately pulled that directory into the system, the same way
  a source-walk `!`-re-include would. Expansion already ignored `.gitignore`
  for exactly this reason; the fix is on the reporting side; not the
  extraction side: the file count now comes from the same incremental-cache
  file contexts extraction itself parsed or reused (`stats.recomputed_files +
  stats.reused_files` in `extract_tree_incremental`'s Terraform branch),
  so it matches file-for-file by construction instead of a second,
  module-unaware walk trying to reconstruct the same answer.
  `iac::terraform_file_count` (the standalone walk-only count) is unchanged
  and stays as a lower-level utility with its own unit tests; nothing in
  production calls it once this lands.
- **An ambient direct read honors `.gitignore`.** Nothing in the repository
  told the Go adapter to read this specific `go.mod` — it is the ordinary
  place Go convention expects one, exactly like every other file the shared
  walk would have found on its own had `go.mod` not been a special case. An
  ignored `go.mod` is now treated as absent: `module_path` checks the root's
  own `.gitignore` (via `source_walk::IgnoreRules`, root-only per ADR-0030 —
  never an ancestor's) before reading the file, and internal-import proof
  that depends on the module path (AC-0207) falls back to Gap exactly as it
  does when there is no `go.mod` at all.

## Consequences

- A Terraform module explicitly sourced from an otherwise-ignored directory
  is still fully expanded (unchanged) and now correctly counted.
- A repository whose own `.gitignore` excludes `go.mod` gets no Go module
  path; internal-import resolution that needs it degrades to Gap rather than
  silently reading a file the repository asked to have ignored.
- Future direct reads (outside the shared walk) need to make this same call
  explicitly and cite this ADR, rather than defaulting to either behavior.

## Alternatives

1. **Both honor `.gitignore`.** Rejected: it would silently drop resources
   from an explicitly-referenced Terraform module the configuration itself
   pulls in, which is a worse and more surprising gap than an over-broad file
   count.
2. **Both override `.gitignore`.** Rejected for `go.mod`: unlike a Terraform
   module source, nothing marks `go.mod` as deliberately read despite being
   ignored — it would make the Go adapter the one extractor that never
   respects the repository's own ignore rules for its own convention file.
