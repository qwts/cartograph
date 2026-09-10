# SPEC-05 — Registered source identity and host-owned root lookup

Status: implementation in progress for #392, prerequisite of #385.
Decision: [ADR-0023](adr/ADR-0023-registered-source-identity.md). SPEC-04 capture and SPEC-03 proposal contracts remain separate.

## Meaning and delivery boundary

A registration identifies a logical source admitted by this Cartograph installation.
It separates the host-owned source ID, recovered repository namespace, display name
and operational filesystem root. It does not identify immutable bytes or establish
that a producer consumed a particular file, commit or complete input set.

The same registered root retains identity across restart. Different roots with the
same basename, including linked worktrees, remain separate. Canonical path aliases
resolved by the existing `paths::canonicalize` helper share one registration. Do
not merge by remote, Git common directory, HEAD, file contents or directory name.
A moved root receives a new registration. Replacing a directory at the same path
retains its logical registration; this is not proof of physical-instance continuity.

No ID file, ownership marker or other registry metadata is written into target
source content or Git metadata. Random IDs live in private application state and
may appear in graph/reference namespaces; they are identifiers, not credentials.

## Durable registry

The host stores a versioned registry in `state.db`, independently of `graph.db` and
`proposals.sqlite`. A private metadata table owns its schema/migration version;
do not use SQLite's global `user_version` for this shared state database.
Unsupported versions or corrupt/conflicting records fail closed, without resetting
the durable spine, reconstructing an identity from graph props or using a basename.

An owned `RegisteredSource` contains `source_id`, unique `repo_key`, source kind,
canonical root binding, operational display name, and normalized origin for a
managed clone. Source ID and locator bindings are immutable in this slice. Managed
availability is mutable operational state and never enters source or fact identity.
Local paths use exact UTF-8 after app canonicalization; unsupported representations
fail rather than using `to_string_lossy`. No extra case/Unicode folding is applied
to local paths. Missing roots retain their record but are unavailable for reads.

Generate `src_<32 lowercase hex>` IDs with SQLite `randomblob(16)`, enforced by
unique constraints and bounded collision retry. A short immediate transaction
implements get-or-register; two connections registering one canonical root converge
on one row. A local lookup checks existing root bindings first, so preflight of a
managed checkout returns its managed registration. No transaction spans extraction,
clone/network work or a source lock wait. Registration failure prevents publication
of dependent findings, jobs, caches or graph facts under an invented identity.

Direct local and `file://` managed sources use `local/<source_id>` repository keys.
Supported GitHub origins use one canonical registered `owner/name` key; normalize
equivalent supported URL forms and ASCII casing consistently. A local checkout of
that remote remains a separate local source. File origins use their canonical full
local origin path, never their basename; a managed clone and its origin directory
are different registrations. Reject unsupported or credential-bearing URL forms.

## Managed destinations and operation guards

Reserve managed identity and its destination before cloning. The host derives a
slot at `<app-data>/sources/<source_id>` from a validated source ID and a
registry installation ID. The fixed checkout child is `checkout`; ownership
metadata binds registry/source IDs outside it. Locks live at
`<app-data>/source-locks/<source_id>.lock`. The checkout, attempt directories and ownership metadata are distinct
children of that slot; ownership metadata must remain outside the checkout.
Neither webview arguments, repository content nor `parse_repo_url`'s display
identity can select an arbitrary overwrite destination. A new slot that already
exists unexpectedly is a conflict. Replacement is allowed only for this registry's
exact existing managed checkout, after validating its owned parent and rejecting
symlink/non-directory substitutions. Never replace a directly registered local root.

Use a persistent lock file per source in a separate private host lock directory.
Open a fresh read/write, non-truncating handle and call `File::try_lock` for an
exclusive operation or `try_lock_shared` for a read. `WouldBlock` returns an explicit
busy result; unsupported locking or I/O failure returns an explicit failure. There
is no blocking retry loop, age-based takeover or unlocked fallback. Do not unlink,
rename or recreate lock files during operation, graph clearing or job cleanup.

An exclusive managed operation guard begins before any clone/destination mutation
and remains alive through clone publication, parser use, root-dependent framework
and configuration enrichment, ADR reads and publication of that recovery's facts.
Do not drop it as soon as `clone_repo` returns. Direct recovery/preflight against a
managed root also acquires a source guard for its entire root-dependent read pass;
shared guards prevent clone replacement while those reads are in progress. The
guard does not prevent an editor or another nonparticipating process from writing.

Plan the required source guard set before taking graph/cache/state mutexes. Acquire
distinct source IDs in sorted order using try-only locking, release all acquired
guards on failure, and reuse a borrowed existing guard for nested work. A helper
must never shared-lock a handle/source it already holds exclusively. ADR relinking
currently reads all Repo roots: plan and guard the available roots it will read
before its graph mutation phase; unavailable roots fail the operation explicitly until reconnected; they are never read through a substitute.
If the required root set changes, fail/replan before dependent reads/publication.
This prevents lock cycles between two repository operations without claiming the
whole multi-repository recovery is an atomic transaction.

Separate lock reservation from checkout validation. First acquire every planned
OS handle without initializing slots or validating checkouts. A busy/failed lock
attempt leaves readiness unchanged. With the complete reservation set held,
atomically mark all planned managed writes unavailable, then initialize/validate
each slot and checkout using those same handles. Any subsequent validation failure,
including a previously ready corrupt checkout or a later read member, leaves all
planned writes unavailable. Read-only members do not mutate their readiness flags.

Slot initialization stages and syncs complete ownership metadata in an owned sibling
before publishing the reserved final directory; failure leaves no half-owned final
slot. An attempt owns a unique temporary path within its registered slot and cleans up
only that path. Mark the managed source unavailable durably before slot/checkout
validation and replacement work; publish ready availability only after successful destination validation and
the guarded root-use operation. Readiness for all managed members is published in one registry transaction after
the job completion transition settles cancellation. If that final registry commit
fails, a job may already record completed recovery while every managed member
remains unavailable; the command returns the failure and re-add is required.
Failure/cancellation leaves it unavailable until
an explicit successful retry/re-add. Do not revive an old checkout merely because
it still exists. A crash releases the OS lock when all held handles close; retain
an incomplete availability marker until successful recovery. Never infer that a
persisted in-progress marker is a live lock, or that an unlocked marker means ready.
Orphan attempt directories are not recursively removed by a different attempt.

The guard owns its handle without cloning or deliberately inheriting it. Pinned
Rust 1.96.1 documents these lock APIs as stable since 1.89.0, with release on close
of all duplicated/inherited handles or explicit unlock. Locking is advisory or
mandatory depending on platform and coordinates participating processes only.

## Intake, source reads and operational display

Preflight (including gate/re-scan), direct ingest, supported ingest retry, add-repo
and every local/clone entry in add-system resolve this same registration. Pass its
repository key through adapter namespaces, extraction caches, findings, ownership
reconciliation, system membership and metrics. Pass its canonical root to plugin
discovery; existing artifact/hash/root enablement and gate checks remain mandatory.
No prior authorization transfers to a different root through a matching source name.

The source-window command accepts an exact repository identity and relative span,
never a caller-supplied root. The host resolves the registry and holds the required
shared managed-source guard while performing the bounded read. UI selection must
not use `repos[0]` or search graph props for a substitute root. `graph_and_reader`
and ADR relinking use the same host association. Unknown/legacy/missing identities
return unavailable. Keep rooted path/range checks and disclosure limits unchanged.
Release read guards before model calls; registry readback still represents the
current working tree, not the bytes cited by an earlier parse or a multi-file snapshot.

Remove operational roots and mutable display metadata from canonical Repo props.
Repo fact bytes use a declared versioned tuple of registered repository key and
recovered revision label. Return human-readable name/root metadata through the
operational `SystemRepo` DTO where needed, not through fact provenance. When
current sources share a display name, retain the name and include each exact
repository key in its visible label; cosmetic labels must not hide distinct sources. Existing
`EvidenceRef` wire fields remain unchanged; newly recovered facts use registered
repository keys. Old staging identities and evidence bindings remain untouched.

## Retry and migration

New retryable direct jobs use `ingest-source-v1:<source_id>`. Preserve existing Job
DTO shape and UI recovery-job behavior by recognizing both new and historical kind
prefixes. Resolve the bound source and validate retry support before calling the
mutating `JobStore::retry`. Historical `ingest:<path>` jobs lack this binding: return
an explicit re-run-ingestion instruction without changing their stored kind/status
or guessing a source. Add-repo/add-system retry remains explicitly unsupported.

Bump the disposable graph fact schema from 3 to 4 for changed local namespaces and
Repo fact bytes. Let the existing graph rebuild contract remove old-scheme facts;
do not mix old/new IDs by incremental upsert. Caches start empty. A one-time state
migration retires generated legacy preflight findings in the same transaction as its private
completion marker, so crash/restart cannot repeatedly delete newly registered
findings. Initialize/gate this migration before accepting intake. Separate graph
and state databases do not form one transaction; every startup step is repeatable.
Do not rewrite jobs, metrics, Workbench decisions, legacy decisions or staged
proposal JSON/IDs/revisions. Clearing graph/jobs does not delete registrations.

The existing unconditional `recover_interrupted` startup pass can mark another
live process's jobs interrupted. This separate lifecycle defect is tracked in #393;
source guards prevent conflicting source use but do not make jobs multiprocess-safe.
Concurrent old/new application versions using one state directory are unsupported
during the identity migration.

## Verification and limits

Tests cover same-basename isolation across real intake/cache/reconciliation paths;
separate-connection registration and guard contention; restart and process-exit
lock release; clone collision/failure/attempt ownership; guarded parser/enrichment
use; exact source lookup and UI unavailable behavior; bound and legacy retries;
root-free Repo facts; and graph/findings migration with unchanged historical stages.
Use handshake-controlled subprocess fixtures for lock lifetime, never timer races.

No registry action establishes source freshness, captures raw bytes, activates H3,
upgrades confidence or writes target source content. Parser receipts, complete input
closure, captured task/source reads, and reference-aware retention remain #385.
This removes the Repo-root component of #342, but opaque host identities do not
prove cross-installation or relocated whole-graph equality. #341's remaining IaC
path work is separate from canonical managed-root/plugin-settings integration.

## Implementation seams

The reusable ingest module parses a bounded `ManagedOrigin` (normalized key,
clone URL, display name and optional canonical GitHub repository key). File URLs
use URL decoding followed by exact local-path canonicalization; reject credentials,
queries/fragments and nonlocal file authorities. Keep old plugin permissions as
historical exact-root keys; the new clone location requires its own existing gate
and enablement checks. Do not copy approval from a former clone directory.

`ManagedCheckout` derives a slot only from trusted host storage and validated
registry/source IDs. Its exclusive guard alone exposes clone/publication; guards
own fresh file handles and expose the validated checkout path to the host. The
host holds the guard until recovery completes and separately updates durable
availability. A current exclusive owner may read its newly validated checkout while
other callers still see unavailable. A failed operation never marks ready.

ADR relinking snapshots the graph and exact source associations, releases graph
locks before acquiring missing read guards and reading source, then validates the
snapshot and applies the recovered ADR patch within one SQLite IMMEDIATE
transaction. A changed snapshot returns a mismatch without writes; a patch failure
rolls back all of its deletes/upserts. This excludes a second connection committing
between validation and ADR publication. Earlier ingestion writes remain separate. Reuse a caller's current source
guards and return explicit changed-context/busy errors rather than waiting or
recursively locking. This does not make earlier ingestion writes atomic.

Reserved T-0140–0147 bind the identity, clone, intake, source-reader, fact-hash,
retry, migration and lock-lifetime contracts. A reserved binding does not claim
implementation until its real regression exists and passes.
