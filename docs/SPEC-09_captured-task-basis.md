# SPEC-09 — Captured agent task evidence and immutable task bases

Status: implementation in progress, #401; prerequisite of #385 and SPEC-01 H3.
Decision: [ADR-0027](adr/ADR-0027-captured-agent-task-basis.md).
Trace: US-0031 / AC-0172–0179 / T-0172–0179.
Extends SPEC-03 and SPEC-06 after SPEC-08; SPEC-04/05/07 still apply.

## Outcome and boundary

Existing bounded escalation tasks use retained parser input when their supporting
facts have participating primary-source receipts. Every supplied evidence item
states its origin, and new immutable proposals bind those selections. Other
evidence remains explicitly working-tree unverified. Neither receipt coverage nor
an unchanged basis proves current repository contents, complete producer input
closure, a correct business interpretation or eligibility for curated context.

This issue does not expand the broker's relation allowlist or redefine directed
unresolved slots. The current adjacent-edge heuristic is not cardinality/direction
proof. In particular, rule-interpretation DEPENDS_ON gaps do not become supported
resolution tasks merely because their BusinessRule has a receipt. Full #385,
H3/AC-0108, Workbench projection and MCP/ACP execution remain incomplete.

## Host task preparation

Replace the evidence-only callback with a fact-qualified plan and reader. Private host interfaces in escalation.rs / a small task_evidence.rs module:

```rust
fn plan_task(nodes: &[Node], edges: &[Edge], gap_id: &str, action_id: &str)
    -> Result<TaskPlan, TaskPreparationError>;
fn prepare_task(state: &AppState, plan: TaskPlan, graph: OwnedGraphSnapshot)
    -> Result<PreparedAgentTask, TaskPreparationError>;
```

The plan identifies the source, Gap, selected adjacent relation, candidate facts,
complete fact digests, requested original citation occurrences and deterministic
evidence IDs/membership. No caller root or arbitrary replacement offset is allowed.
Keep the existing supplied-output limits separate from acquisition attempts:
eight supplied candidates, 12 supplied evidence items, 8 KiB per item and 48 KiB
combined supplied UTF-8 text. Unreadable legacy candidates consume attempted work,
not these supplied-item slots. Follow the existing candidate order and continue
past unreadable legacy candidates until eight readable candidates are supplied,
the neighborhood ends, or an explicit attempt budget is exhausted.

There are at most **64 attempted evidence requests per task**, including the
source and Gap requests. Metadata lookahead may select the complete 64-request
window before any source acquisition, and is counted separately from attempted
reads. The source and Gap are selected facts even when their provenance has no
citation; that request is a reported omission. The source must exist in the graph. Each probe names at
most one supporting fact/citation. The adjacent slot relationship is one additional
fixed metadata selection, so the bounded graph API accepts at most 65 unique keys;
preliminary/revalidation passes do not authorize more logical probes. Each normal
request names provenance occurrence zero; the qualified read seam also supports
exact nested receipt role/index occurrences for future task selection. This slice
does not automatically add nested initializer ranges to every escalation payload. Stop early
after eight readable candidates. Do not couple this cap to the 12 supplied items.
This is an explicit compatibility tradeoff: old assembly scans until eight readable
candidates or neighborhood exhaustion, without this attempt cap. The new cap can
omit a later readable candidate; it must not be described as unchanged scanning.

Return a deterministic SelectionReport with attempted/supplied counts, configured
limit, stop reason (candidate_limit, neighborhood_exhausted or attempt_limit) and
bounded ordered omission records with fixed reasons. Unvisited window entries
are reported as metadata-preselected with evidence not read; the tail beyond the
window is not inspected. Neither is labeled unreadable or absent. If the broker's required source and
candidate memberships are satisfied, attempt exhaustion may produce the bounded
task with this explicit limitation; otherwise return a fixed preparation failure
with the same report. Never silently increase the budget or claim complete coverage.

## Coherent graph and association selection

Add a bounded SqliteGraphStore operation in core-graph/source.rs that returns an
owned full graph snapshot and sorted unique per-key source selections within one
read transaction. The caller compares the returned complete graph with its owned expected snapshot
after the read transaction ends; equality therefore validates that same selection
revision. The operation is read_source_selection_snapshot(keys), returning
SourceSelectionSnapshot { graph, selections }. FactSourceState distinguishes
Missing, Absent { fact_digest }, Present { fact_digest, binding } and
Invalid { fact_digest: Option<String> }. Reject more than 65 raw keys before
deduplication and validate keys before entering the transaction. Each selection binds the typed FactKey and complete fact
digest, with a present exact SourceBinding, valid absence, missing fact or invalid
association state. Invalid schema/query/graph data fails the whole operation;
per-key malformed association data never turns into absence. Missing selected
facts remain expressible for later assessment. No source bytes, registry lookup
or OS lock acquisition occurs in the transaction. This is a whole-graph copy and
comparison plus at most 65 selected keys, not a constant-cost graph query.
Existing ContextSnapshot identity stays unchanged.

One preliminary transaction selects the bounded request window and fixed slot.
The host then acquires managed read guards in sorted source order and retention
guards in sorted source order, outside application/database mutexes. Track each
source/access-kind result independently: do not use all-or-nothing acquisition
across unused candidates. Managed checkout failure cannot disable an otherwise
valid captured read from that same source. Captured-only reads require logical
registration, not an available original checkout or managed root.

One authoritative transaction repeats the window selection with the expected
graph. Graph change fails the task globally. Apply each per-key association
comparison or malformed/missing selection failure only when that request is
visited; an unread tail's registration, guard or association failure cannot abort
a prefix that already supplied eight candidates. A visited association change
requires fresh assembly; do not silently substitute a new receipt or retry.
The fixed slot is always checked. Stage only visited selections plus that slot.
Every staged selection therefore belongs to the same authoritative revision,
including explicit absence. No independent per-fact autocommit reads may stand
in for this coherent window operation.

Then copy source from exact immutable receipt/range selections while retention
guards remain held. A later graph publication may happen; the returned task binds
the coherent observed revision, not a global freeze across independent stores.
Release all source/retention guards before preview/provider execution.

## Evidence acquisition and failure policy

For a present association, load and validate the exact v1 or v2 receipt, its
registered source/repo, fact key/digest and original inventory occurrence. Match
EvidenceRef and role/index, never borrow an enclosing owner's receipt merely
because it covers the same bytes. Read that CaptureSpanRef through the existing
strict selected-object reader, preserving raw offsets, BOM and CRLF. Check the
8-KiB task range bound before requesting bytes. Keep whole original spans: existing
whole spans over 8 KiB already fail broker validation, so early rejection must not
be replaced by invented excerpts to make them pass. Excerpt selection is a separate
optional future contract; never truncate or adjust a receipt range in this issue.

Payload bounds do not bound acquisition work. The captured reader loads and
hashes the entire selected retained file before slicing, even for a tiny span.
Charge its exact validated receipt file length before every captured object read,
including repeated reads of the same file. A separate **128 MiB aggregate captured
file-validation budget** bounds this work per task, with the existing 16 MiB file
cap. If the next participating request would exceed the budget, fail the task
with a fixed diagnostic and a validation_byte_limit report; do not skip, downgrade
or fabricate an excerpt. This explicit limit can reject a task that otherwise fits
the span budget. No cross-request verification cache is assumed. Assessing a
selected proposal uses the same byte-work cap and reports per-item limit failure
without pretending the source was missing or verified.

The report includes metadata-lookahead, acquisition attempts, copied items,
captured validation bytes and their configured caps. These do not bound SQLite
whole-graph cost, manifests (up to SPEC-04's 8 MiB each), receipt decoding (128 KiB
each), lock latency or CPU wall time. At most 64 legacy reads retain their existing
256-KiB per-span acquisition ceiling; supplied evidence still obeys 8/48-KiB caps.
All paths run on blocking workers; no graph/store mutex spans OS guard acquisition
or provider execution.

A selected participating item's missing/corrupt receipt, unsupported inventory,
forgotten object, strict decoding error or excessive span fails this task with a
fixed source-free diagnostic. Do not downgrade it to unverified, skip it to pick
another candidate, or reread the checkout. Batch failure is per instance.

Only a valid absent association permits the existing exact-registration
working-tree reader. Preserve its current bounded/lossy text behavior and classify
the result working_tree_unverified; no Git revision or producer-byte claim follows.
Its failed read can remain an explicit omitted legacy item, subject to broker
membership requirements. A graph/store/query failure is never association absence.

## Pure task and basis data

Keep AgentTask, AgentEvidence, AgentProposal, EvidenceRef and existing v1 broker
entry points compatible. Add a pure PreparedAgentTask wrapper in agents:

```rust
struct PreparedAgentTask { task: AgentTask, source_basis: TaskSourceBasisV2 }
struct TaskSourceBasisV2 {
    schema_version: u32, // exactly 2
    graph_snapshot_id: String,
    selected_facts: Vec<TaskFactSelection>,
    evidence: Vec<TaskEvidenceOrigin>, // exactly one entry per task evidence ID
    selection: SelectionReport, // bounded attempts, omissions and stop reason
}
```

TaskFactSelection stores a typed node/directed-edge key, complete fact digest and
optional exact association (repo key, receipt ID, emitted digest). Record visited
facts relevant to the selection report and the fixed slot metadata; do not treat
unvisited candidates as attempted. Its pure DTO
can mirror FactKey; agents do not gain GraphStore, filesystem or provider trust.
TaskEvidenceOrigin identifies the evidence ID, supporting fact and occurrence,
and a tagged origin: working_tree_unverified or captured_primary_source. The
captured variant binds registered_source_id (distinct from AgentTask.source_id),
receipt ID, receipt inventory index, role/index and the complete CaptureSpanRef
metadata, with primary_source_only/input_closure_not_established markers.

The host supplies this basis from the actual read, never webview/provider input.
Pure validation establishes bounded wire consistency and exact item/membership
matching, not producer authority. Cap new serialized source-basis metadata at
64 KiB before hashing or provider preparation; keep existing task/summary limits.
No raw source, root locator, timestamp or mutable freshness state enters this DTO.

## Immutable staging v1 and v2

Keep the exact StageContentV1 serialization, validator and proposal-stage-v1 hash
domain. Reading/reviewing/restarting never rewrites historical immutable JSON,
proposal IDs, original graph basis, evidence binding or review state. Version-one
records remain working_tree_unverified and awaiting_reconciliation. Do not attach
retrospective receipts to their evidence or infer them from old fingerprints.

Keep separate StageContentV1 and StageContentV2 decoding; the v2 task manifest
uses schema_version 2 with the original fingerprinted task fields. Add v2 decoding with unknown versions,
unknown fields, present-null required fields and excessive sizes rejected before
large allocation. Preserve existing unresolved-task fields, evidence text hashes
and byte counts, all candidate summary hashes/bytes/memberships and producing job.
StageContentV2 has a required complete TaskSourceBasisV2 and a canonical
fingerprint of its sorted selected facts/associations, including absent entries.
The public flat StagedProposal DTO adds an optional source_basis field: absent
and omitted when serializing v1, required for v2, and explicit null is rejected.
The internal v1 content serializer remains frozen and separate. Validate duplicated graph
snapshot fields agree. Use a new proposal-stage-v2 content-hash domain; all
immutable reviewed material and selections contribute. Reviews/timestamps remain
outside immutable identity, with existing revision CAS and idempotent insertion.

Add stage_prepared/preview_prepared/propose_prepared APIs, sharing existing broker
validation and replay checks rather than copying them. Prepared task basis hashes use a separate versioned input domain over canonical
action-independent fingerprinted task fields: gap/source/relation/confidence,
evidence references plus text hashes/lengths, candidate identities plus summary
hashes/lengths, sorted memberships and the complete source basis. This binds all
raw supplied text by its fingerprint and permits stored v2 records to reconstruct
the prepared hash without retaining raw text. The action ID remains outside this
semantic hash. Bound summaries before hashing or preview, and
retain the existing 64-hex AgentProposal.basis_hash wire field and v1 algorithm.
Action ID remains the consent identity rather than a semantic freshness proof.
Retain the 128-KiB complete staged-record bound and no-note review headroom.
History cursor interpretation stays version 1 independently of content versions.
Validate schema1/working_tree_unverified and schema2/per_item explicitly. Stored
SQL type/length checks precede loading bounded immutable content; v2 nested shape
and count/depth bounds precede typed decoding, including old permissive nested
provenance/citation DTOs. Unknown fields cannot disappear during validation.

New stages use per-item origin plus a top-level per_item evidence binding; context
status remains awaiting_reconciliation regardless of coverage or human decision.
Never persist raw AgentEvidence.text or candidate summaries. Apply SPEC-03 replay
admission to every supplied item, including uncited/unselected items, unchanged.

## Consent, execution and current-basis assessment

Use the same preparation path for gap_strategies, escalation_preview,
run_escalation and every run_class_escalation instance. Include bounded source
origin and selection metadata in the broker's redacted payload, so changing a
receipt changes the exact consent hash even if graph facts and span text are equal.
Include a canonical source-basis fingerprint in the prompt as fixed hex alongside
the bounded descriptive metadata, so redaction of a path or identity cannot
collapse different receipt selections onto the same consent payload. Existing
tier opt-in, local/cloud policy and per-payload grants remain mandatory;
retention is not disclosure authorization. A class still cannot amortize consent.
Prepare class instances lazily, one gap → prepared task → provider → stage at a
time, with a fresh bounded selection per instance; do not acquire an
unbounded union of source guards or present separate observations as one revision.

Once a model call has started, reingest/forget/cancellation does not rewrite its
copied task or discard its completed proposal. Stage the original prepared basis,
including SPEC-07's valid retained-execution/missing-row exception. Never require
that source is still retained or current merely to preserve completed history.

Add a selected-proposal read-only assess_staged_basis(proposal_id) host API. Return
the observed current graph identity, graph comparison, selected-association
comparison, per-item retained availability and unverified count; v1 returns
legacy_unverified. Distinguish changed, unavailable and operational failure.
An unchanged result means only graph content/selected associations matched at that
observation and selected captured bytes remained readable. Historical availability reads use copied exact receipt selections under
source-tagged retention guards, independently of the current graph association.
Keep these guards across retained-byte checks and the coherent current graph
observation; do not require current-receipt UI readers or a managed checkout.
Busy/storage failures are operational failures, distinct from known missing or
forgotten bytes. Do not reread legacy working trees to manufacture freshness,
cache a durable fresh flag, mutate stages,
or expose a single eligible/current-business-truth boolean. UI presents these
dimensions separately and retains the awaiting-reconciliation label.

## Six scoped test seams

1. Actual captured TS producer (including a v2 local-definition receipt), host reader and
   fake provider: mutate/delete the checkout; the prepared task receives retained
   bytes and exact original ranges with explicit mixed-origin status where needed.
2. Two graph connections and a deterministic hook: publish between preliminary
   selection and the authoritative read; assembly rejects/requires reassembly,
   never pairs one graph revision with another association set.
3. Equal-length trailing-comment recapture: unchanged complete facts and supplied
   span text still change receipt selection, prepared/staged identity and consent;
   a stale approved hash prevents provider invocation.
4. Participating receipt/object corruption, forgetting, split UTF-8 and oversized
   range: fixed failure before provider, no working-tree fallback or source-bearing
   error; valid absent associations remain explicitly unverified. Include two
   source reads followed by ten unreadable legacy candidates and a later readable
   candidate, deterministic 64-attempt exhaustion/reporting, and small requested
   spans in large retained files to enforce aggregate full-file validation work.
   Include a failed/unavailable unused candidate tail and independent access-kind
   guards for a source whose original managed checkout has disappeared. Whole oversized
   spans still fail; no excerpt is manufactured to satisfy the broker bound.
5. Literal v1 immutable rows/IDs/reviews survive mixed v1/v2 history, restart and
   review; v2 tampering, unsupported versions and source-basis/record bounds fail.
6. Fake provider interleaving with reingest/forget/cancel: completed results stage
   their original basis, later assessment reports change/unavailability, and graph,
   confidence, exports and legacy history remain unchanged. Add focused UI stories
   for mixed evidence, changed/unavailable basis and late assessment selection.

## Expected files and review choices

- crates/core-graph/src/source.rs and crates/core-graph/src/source/tests.rs:
  bounded coherent selection API.
- src-tauri/src/escalation.rs, src-tauri/src/main.rs,
  src-tauri/src/primary_source.rs and new src-tauri/src/task_evidence.rs:
  planning, qualified capture reads and production single/batch/preview wiring.
- crates/agents/src/lib.rs, crates/agents/src/staging.rs and their tests: pure prepared-task contract,
  shared broker preparation/validation, explicit v1/v2 content and identity.
- src-tauri/src/proposals.rs and focused host fixtures: selected basis assessment.
- ui/src/store.ts, ui/src/components/ResolutionStrategyModal.tsx and
  ui/src/components/ProposalHistory.tsx plus their adjacent stories:
  version-aware DTOs, evidence labels and selection-bound assessment responses.
- SPEC-03/06 and a new issue specification/ADR/trace rows, assigned only by root.

## Delivery and compatibility

PreparedAgentTask is a validated wrapper; its inner AgentTask remains compatible.
New immutable stages expose per_item evidence binding. Version-one rows and their
wire serialization stay unchanged, including absent new fields and their original
identity domain. Version-aware history preserves review CAS and no-note headroom.
An explicit current-basis assessment never changes receipt retention or historical
reference counts by inventing source use. Retention previews must count actual
v2 receipt references in immutable staged bases, including unselected supplied
candidates; v1 history keeps its previous unverified-reference treatment.

UI preview and history distinguish retained parser input, unverified working-tree
input and selection limits; detailed metadata remains inspectable without adding
protocol mechanics to ordinary user flows. Preview and assessment commands use the
blocking worker boundary. Late assessment responses are bound to proposal ID and
request generation and cannot replace another selection's result.

Focused local gates run under the shared execution policy; complete workspace,
Storybook/build, native Windows and CodeQL gates run in exact-SHA CI before ready.
Full H2/H3 and market-readiness claims require their separate acceptance evidence.
