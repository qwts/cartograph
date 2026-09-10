# SPEC-06 — Producer-bound primary source inspection

Status: implementation in progress, #395 under #385. Decision: [ADR-0024](adr/ADR-0024-primary-source-receipts.md).
SPEC-00 integrity, SPEC-03 immutable staging, SPEC-04 capture and SPEC-05 registration remain binding.

## Meaning and participation

The app retains the exact selected TypeScript/JavaScript bytes consumed by the
production parser and supplies captured inspection for direct guarded-exit rule
observations, their lexical owner nodes and lexical GOVERNS edges. A receipt means
`primary_source_only` with `input_closure_not_established`. It never establishes a
complete business rule, execution predicate, caller interpretation, configuration
closure, current revision freshness or curated-context eligibility.

Only the adapter invocation consuming an immutable `CapturedFile` can produce a
participating receipt. The factory calls the actual parser on that buffer; there
is no public operation to attest an unrelated earlier extraction. Direct lexical
eligibility belongs to that producer. Eval/synthetic buffers, placeholders,
directory resolution, config/event stitching, plugins, other adapters and later
enrichments are not admitted by matching their apparent evidence spans. Preserve
all existing rule interpretation gaps and producing tier/confidence.

## Receipt and current association

A version-one immutable metadata-only receipt binds:

- the registered source ID and exact repository key;
- the complete CaptureFileRef (capture, path, raw digest and length);
- the producer contract version and selected TS/TSX grammar/package version;
- a typed node identity or directed `(source, label, destination)` edge identity;
- a canonical digest of the entire emitted node/edge, including all properties
  and provenance; use a versioned domain prefix and recursively sorted JSON object
  keys, preserving array order and fact kind;
- every original-file citation, including nested rule sources, as a fixed ordered
  range inventory with role/index and the original EvidenceRef;
- the primary-source and incomplete-input-closure scope markers above.

The receipt ID hashes this complete canonical content, excluding only its own ID.
It is not the semantic `prov.content_hash`. Receipt metadata stays outside fact
properties to avoid circular identity. Receipts contain no original source text.
Bounds cover serialized receipt bytes, range count, identities and stored totals;
oversized/invalid facts remain explicitly outside participating inspection, never
receive truncated or adjusted source ranges. Capture text reads remain strict UTF-8.

The graph maintains private node/edge current-association tables, separate from
canonical recovered facts and exports. Each association includes the exact receipt
ID, full fact digest and repository key. Publish repository reconciliation and its
current associations in one SQLite IMMEDIATE transaction against an owned expected
graph snapshot. Clear previous associations for that repository even when the new
facts are unchanged or no longer participate. Validate the final emitted digest
before accepting an association. Any ordinary graph mutation invalidates affected
associations; deletion/clear removes current associations without deleting history.

An equal-length trailing-comment edit can leave complete facts and the graph
snapshot hash unchanged while changing their captures. Therefore neither graph
snapshot identity, fact digest nor EvidenceRef selects a current receipt. A source
request must preserve the exact expected receipt ID. Current binding reads copy
the fact and association from one database revision and reject mismatch. A later
mutation cannot turn an old returned receipt into a different historical source;
an expected-current request after publication must detect the changed receipt.

Persist source captures and immutable receipts before graph publication. A failed
publication may leave unused retained metadata/bytes, but may not publish a
binding as readable if persistence failed. Separate stores do not claim a global
transaction; corruption/deletion always returns unavailable, never a substitute.

## Production parsing

The common TypeScript branch used by direct ingest, supported retry, add-repo and
each manifest member acquires selected working-tree bytes using its registration.
Clone HEAD is only a separate revision label; parsed checkout bytes are not claimed
to be Git blobs. Git-object production ingestion remains outside this slice.

Enumerate supported extensions with exact UTF-8 paths, bounded visited entries,
file count and depth, without following symlinks. Preserve intentional vendor,
hidden-directory, declaration-file and generated-output exclusions. Reject selected
symlinks/unsupported paths explicitly; do not silently widen the selection. The
rooted capture reader independently confines acquisition. A changing external
filesystem can change the acquired byte set; the parser consumes exactly the set
retained, not a claimed atomic filesystem snapshot.

The participating branch bypasses the TS parse cache. Existing ordinary adapter
entry points remain available for preflight/test callers without claiming retained
evidence. Directory enrichment still runs after captured per-file parsing and
remains outside input closure; before graph publication keep only receipts whose
complete final fact still matches. Other language caches are unchanged. Capturing
or persisting fails the requested recovery explicitly, with no live-read fallback.

## Host storage and retention

Use private local application storage for captures, receipts and persistent
per-source retention lock files. Unix directories/files are owner-only; reject
symlink substitutions between operations through rooted handles and identity checks.
SQLite opens its sidecars by pathname, so the canonical private application
namespace must remain trusted during each SQLite operation. These checks do not
promise isolation from arbitrary concurrent renames or writes by another process
with the same user's filesystem authority. On other platforms retain application-directory ownership
and fail unsupported access operations explicitly. No raw source enters proposal
SQLite, graph properties, default logs, context responses or spec exports. Retention
does not permit model disclosure or egress.

Short SQLite/WAL transactions bound receipt persistence (64 MiB logical metadata,
100,000 receipts initially); capture storage retains SPEC-04's 2 GiB/10,000-capture
logical limits. SQLite/WAL overhead is additional. Capacity errors explain that
the user can forget retained source before recovering again; no automatic eviction.
Captured inspection validates manifest membership and the selected raw object
without loading unrelated source objects. Output still obeys the retained span cap.

Recovery holds a shared per-source retention guard from acquisition through
publication. Inspection holds a shared guard while copying verified bytes.
Forgetting requires the exclusive guard. Acquire guards before graph/cache/store
mutexes and use fresh persistent try-only OS handles; busy/error fails explicitly,
never retries under another lock or falls back unlocked. Guards coordinate app
processes, not external editors. Managed-source guards remain separate and precede
retention guards; forget never acquires a managed guard.

The source retention UI previews the exact source and retained capture/receipt
counts and current versus historical reference counts. A metadata fingerprint
binds the affected capture/receipt inventory and the current associations shown;
confirmation revalidates it under
the exclusive guard. A changed inventory requires a refreshed preview. Forgetting
atomically removes that source's capture manifests and only raw objects no retained
manifest references. Immutable receipt metadata and proposal/review history remain;
their source reads become unavailable. Current graph associations may remain as
explicit unavailable references. Clearing graph/jobs never deletes retained source.
An explicit later recovery can restore identical content under its identical capture
identity, but different bytes never satisfy the old reference. Deletion ends the
stored span-cap lifetime; explicit recapture applies its supplied limits, while
immutable historical receipt ranges stay unchanged. Receipt metadata is
not automatically removed; reaching its capacity remains an explicit limit.

## Captured inspection and compatibility

The host exposes a fact-qualified description containing its exact receipt ID,
emitted digest and stored range inventory. Inspection accepts typed fact identity,
expected receipt ID and a stored range index; callers cannot supply another root,
path or arbitrary offsets. The host checks current association, fact digest,
registration, immutable receipt and capture membership before returning strict text
from the validated object. Unknown, changed, corrupt, forgotten or invalid-range
references return a fixed unavailable/stale status without echoing source content.
The description request also supplies the selected complete node or edge; the host
hashes it as a consistency check and refuses a changed selected fact. Descriptions
and receipts do not grant arbitrary filesystem access.

The evidence panel offers captured primary source only when the selected fact has
a participating association; it shows the incomplete-input-coverage label. Current
working-tree inspection remains a separately labeled operation with unverified
cited revision. Async results bind their selection and receipt, so switching facts
cannot display an earlier fact's returned source. Retention controls name the exact
source and require a concrete preview before deletion.

EvidenceRef and staging v1 bytes/IDs/reviews are unchanged. Agent task assembly
continues its unverified working-tree basis, and existing accepted proposals stay
awaiting reconciliation. H2 business semantics, full #385, H3 projection and MCP/ACP
execution remain incomplete. Runtime activation requires this whole parser,
publication, inspection and retention contract, not a standalone receipt API.

Reserved T-0148–0155 trace parser invocation, receipt integrity, atomic association,
production ownership, captured inspection, retention, UI and compatibility.

## Local-definition extension

[SPEC-08](SPEC-08_local-rule-definitions.md), tracked in #398, specifies v2
producer receipts for new local initializer citations while retaining the exact
v1 receipt contract and immutable history. Its expanded citations do not change
this specification's primary-source or incomplete-input-closure boundaries.
