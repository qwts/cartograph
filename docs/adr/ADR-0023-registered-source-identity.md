# ADR-0023 — Durable source registration before production capture receipts

- **Status:** Accepted for the staged implementation in #392
- **Date:** 2026-09-10
- **Deciders:** Cartograph owner direction; implementation agent within SPEC-01

## Context

Local intake derives identity from directory basenames, while local-mirror clones
derive overwrite destinations from that same lossy identity. Different sources
can share graph/cache/findings ownership or replace each other's managed trees.
Operational roots live in graph facts, and source navigation can fall back to the
first repository. SPEC-04 requires a host source identity before production capture
adoption; neither registration nor matching a path can prove original producer use.

## Decision

Introduce a versioned host registry on the durable SQLite/WAL state spine. Generate
opaque source IDs with existing SQLite `randomblob(16)` and uniqueness/retry rather
than a new dependency. Keep source ID, repository key, operational root and display
metadata separate. Canonical exact local roots and normalized managed origins use
atomic get-or-register; distinct roots remain distinct even when Git/content match.
Use a private schema-version table rather than the shared state's `user_version`.

Register managed ownership before cloning and derive destinations only within a
private host-owned slot. Extend clone publication with attempt-owned temporary
paths and persistent per-source file locks. Pinned Rust 1.96.1 already provides
`File::try_lock` and `try_lock_shared` (stable since 1.89); no lock dependency is
needed. Hold an exclusive guard across clone, parse, enrichment and publication,
and shared guards across managed-source reads. Acquire planned source IDs in order,
reuse existing guards and fail busy without waiting cycles. Never unlink live lock
files, silently fall back unlocked or overwrite an arbitrary existing directory.

Reserve the complete sorted OS lock set before initializing or validating managed
checkouts. A failed lock acquisition leaves readiness unchanged. Under those same
handles, atomically persist all planned writes unavailable before promoting the
reservations into validated guards; any later slot or checkout validation failure
leaves those writes unavailable, including for previously ready sources.
Leave interrupted attempts unavailable until an explicit successful recovery. OS handle closure releases the
lock after a crash; a stored marker is neither a lease nor proof of readiness.
The lock coordinates Cartograph operations and does not freeze external writers.
Keep all ownership markers outside checkout/source content.

Use the registry across every intake, source reader and ADR relink. Remove UI root
fallback and caller-root source-read authority. Remove operational roots from Repo
facts, preserving display through an operational DTO. Bind new retryable jobs with
`ingest-source-v1:<source_id>`; refuse legacy path-kind replay before mutating its
row. Rebuild graph schema 3 as schema 4 and retire generated legacy findings once,
while preserving historical decisions, jobs, metrics and immutable staged records.

ADR source reads release the graph mutex after taking a coherent snapshot. The
host applies the resulting fact patch only if the complete snapshot still matches
inside one SQLite IMMEDIATE transaction. Stale inputs write nothing; patch errors
roll back. This closes the cross-process validation/write gap for ADR publication
without claiming atomicity for the preceding extraction/load operation.

## Consequences

Same-basename sources no longer share operational ownership, and managed clones
cannot swap while participating recovery reads use them. Restart retains logical
identity; missing roots stay unavailable. Moved roots are new registrations and
same-path directory replacement is not detectable physical identity continuity.
This requires a derived-graph rebuild and fresh registration/recovery for legacy
sources. Root-free Repo hashing addresses only part of #342: host-local opaque IDs
do not imply cross-installation or relocation-invariant graph hashes.

Registration adds no source-byte proof, source-content writes, confidence upgrade,
verified task basis or curated-context activation. SPEC-04 receipts/input closure
and retention remain #385. Unsupported lock/storage behavior fails closed; shared
state with concurrently running older application versions is outside this migration.
Existing unconditional startup job recovery can still interrupt another process's
live job record; its separate ownership/lifecycle fix (#393) is not supplied by source locks.

## Alternatives

1. Hash a basename, path, remote or Git common directory: rejected because these
   respectively collide, couple identity to location, or merge distinct worktrees.
2. Use only a process mutex or registry in-progress flag: rejected because another
   process can replace a clone and crash-stale flags are not an operation lock.
3. Put an identity file in each repository and ship receipts simultaneously:
   rejected because it writes target content and obscures the bounded prerequisite.
