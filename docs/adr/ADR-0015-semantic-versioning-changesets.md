# ADR-0015 — Single-source semantic versioning and Changesets release intent

- **Status:** Accepted
- **Date:** 2026-07-13
- **Deciders:** Cartograph maintainers

## Context

Cartograph is one desktop application represented by four independently editable
version fields: the root and UI npm packages, the Rust workspace packages, and
the Tauri bundle. Independent versioning would make an installer, Git tag,
changelog, and executable metadata disagree. Release automation also needs a
reviewable record of whether each shipped change is a feature, fix, or breaking
change without allowing a bot to infer release intent from commit messages.

## Decision

- The root `package.json` version is canonical because Changesets operates on
  that private package and produces its changelog. The root/UI npm lockfiles,
  `ui/package.json`, `[workspace.package].version`, local workspace package
  entries in `Cargo.lock`, and `src-tauri/tauri.conf.json` are mirrors.
- `npm run version:sync` copies the canonical version into every mirror.
  `npm run version:check` is a non-mutating CI gate that rejects malformed
  SemVer, drift, and missing workspace lock entries.
- A Changeset records release intent in the behavior-changing PR. While the app
  is on `0.x`, behavior/features and breaking changes bump minor; fixes bump
  patch. At `1.0.0`, breaking changes bump major. Non-shipping docs, test, and
  tooling changes may omit a Changeset.
- `npm run changeset:version` is the only supported version-application command:
  it consumes Changesets, generates `CHANGELOG.md`, then synchronizes all
  mirrors. Release automation owns that command; contributors do not hand-edit
  versions, changelogs, or release tags.
- Changesets does not publish Cartograph to npm and does not create tags. The
  reviewed version PR and tag/release workflows own those later transitions.

## Consequences

- Every artifact and version tag can be checked against one value before it is
  published.
- Release intent remains close to the change while version application remains
  a deliberate, reviewable action.
- Adding another version-bearing manifest requires extending the sync/check
  script and its tests in the same PR.
- Until the version-cut workflow is implemented, releases intentionally cannot
  be cut from the new policy alone.

## Alternatives (≤3)

- **Use `tauri.conf.json` as canonical** — rejected because Changesets cannot
  natively version it or generate the application changelog from it.
- **Use the Rust workspace version as canonical** — rejected because Cargo's
  package-release model does not represent desktop release intent or the npm
  lockfiles.
- **Version each layer independently** — rejected because Cartograph ships as
  one application and independently versioned internals would make provenance
  and support reports ambiguous.
