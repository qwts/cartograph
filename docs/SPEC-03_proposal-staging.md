# SPEC-03 — Durable agent proposal staging

Status: implementation in progress, #384. H3 prerequisite under SPEC-01 and
[ADR-0021](adr/ADR-0021-durable-proposal-staging.md).

## Review boundary

The host persists each validated broker result before reporting success. A staged
record binds the complete proposal (including annotation, citations and original
Agentic/InferredWeak provenance), the producing job, the recovered graph snapshot,
and a versioned task-basis manifest to an immutable content identity. The graph
copy reads nodes and edges in one SQLite snapshot even when another app process
writes concurrently. This does not attest the separately read source bytes.
Retrying an identical stage operation is idempotent. Different reviewed content has a different
identity, even when its proposed edge tuple is unchanged.

The basis manifest retains the unresolved task fields, bounded candidate identities
and evidence membership, exact source references, and fingerprints of source spans
and candidate summaries. It never stores raw `AgentEvidence.text`. The original
task basis hash remains available for comparison, but matching that digest alone
is not a freshness attestation. Manifest construction and result validation use
the host's original bounded task, never a task reconstructed from caller input.
Staging validates exact candidate choice and cited evidence as well as the broker
provenance ceiling. Persisted payloads are bounded to 128 KiB per record.

Review accepts only a staged ID, expected review revision, decision and optional
note. It resolves the proposal on the host, updates the review with an atomic
compare-and-set, and increments the revision. Unknown IDs and stale revisions
fail explicitly. A review never rewrites staged content, changes producing
provenance, resolves a recovered Gap, or writes target code.

Staging is durable independently of job cleanup. Single-result and partial batch
runs persist every completed result before returning it as successful. A failed
stage is a failed result; the UI must not offer acceptance for an unstaged body.
Cancellation does not discard already persisted results. The review surface loads
staged history after restart through a bounded, deterministically ordered read
API, with an explicit continuation when more records exist. Review state remains
separate from immutable proposal identity.

Legacy `agent_decisions` remain historical records. They lack an immutable host
basis and are not silently imported or activated. Existing Workbench assertion
curation is unchanged by this prerequisite and must join the shared policy in the
later projection slice.

## Evidence version

The manifest records fingerprints of the exact host task span text and its source
references. Existing task assembly reads bounded working-tree spans, while some
references name a commit and local ingestion uses `workdir`. The producing adapter
does not yet persist a source-byte attestation for every cited file. A later Git
blob read alone cannot establish that its bytes match the original parse (checkout
filters and line endings can differ).

Therefore staged records explicitly carry `working_tree_unverified` evidence
binding and `awaiting_reconciliation` context status. Neither stage persistence,
snapshot equality nor acceptance establishes citation freshness. No raw source is
added to the durable store. Verified source capture for both local and Git inputs,
with per-file parse fingerprints and fail-closed reconciliation, is tracked in
#385 and is required before these records can enter curated context.

The first source-capture slice is #387 ([SPEC-04](SPEC-04_source-capture.md)):
bounded acquisition and local immutable byte storage, separate from the existing
`EvidenceRef` and staging wire format. It does not change these records or certify
their original task basis; actual producer/capture reconciliation remains #385.

## Delivery boundary

This slice makes review durable and binds it to the material actually produced.
Accepted records are explicitly **awaiting context reconciliation**. UI copy must
not promise that acceptance already affects exports. Recovered graph, context and
export behavior remain as specified before this slice; H3 and AC-0108 stay open.

The subsequent H3 projection must establish exact directed unresolved slots and
their cardinality; reconcile complete source/candidate bases against current
recovery; distinguish stale, conflicted, already-covered and eligible records;
preserve stronger facts and producing confidence; include Workbench decisions;
and give UI, exports and agent reads the same curated content identity. Accepting
again cannot by itself certify changed evidence. No MCP/ACP runtime is implied.

## Validation

Reserved AC-0126–0131 cover immutable complete-content identity, task/result
validation and source omission; atomic ID-only review and restart; durable single
and partial batch integration; resumed UI review and honest status; explicit
unverified source binding; and unchanged recovered facts/legacy separation. Tests include
modified annotation/citations, unknown and stale review IDs, storage failures,
restart, cancellation after completed instances, changed host span fingerprints,
and the inability of acceptance or a commit-shaped citation to assert freshness.
