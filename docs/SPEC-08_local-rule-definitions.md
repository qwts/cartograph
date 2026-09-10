# SPEC-08 — Cited local definitions for rule conditions

Status: implementation in progress, #398. Decision: [ADR-0026](adr/ADR-0026-local-rule-definitions.md).
Extends SPEC-02 and SPEC-06; SPEC-00 integrity and immutable proposal history remain binding.

## Meaning and eligibility

Recover how a directly initialized local `const` used by a rule condition is
written, with its lexical binding and original citations. Keep the observed
condition unchanged. An initializer is not a substituted execution predicate,
proof of a value at use, configuration, object immutability or consumer meaning.
In particular, `const values = []` does not prove that `values.length` is zero
after a mutation. No source evaluation or model call belongs to this projection.

Seed definitions from condition dependencies, then follow original identifier
sites inside retained initializers. Admit only a simple identifier declaration
with a direct initializer, a stable non-invalidated binding and the same actual
callable as the rule. Prove declaration-before-use structurally: its declaration
statement must precede the containing use statement in the same or an ancestor
block. Reject use in its own initializer, same-declaration-statement use, TDZ,
destructuring, mutable/ambiguous bindings, enclosing callable captures and
loop/switch/try/with crossings. Resolve each nested read at its original site,
never using the eventual guard's scope or source position.

Keep existing dependency binding/target observations intact. Ineligible values
and unsupported interpretation receive fixed reason Gap nodes and cited
dependencies. An observed declaration reference never silently becomes a proven
initializer or value. Definition identities derive from existing declaration
offset identities; no benchmark names or source literals become special cases.

## Versioned stored contract

New guarded-exit payloads use schema version 2. Version 1 remains readable with
its exact prior meaning and without local-definition evidence. Add an optional
`local_definitions` field: absent for v1, required array (including empty) for v2.
An explicitly present null is invalid; v1 must reject this new field rather than
normalizing it away. Unknown versions fail explicitly. Never rewrite stored v1
facts, receipts, proposal IDs/content, review revisions or source references.
Existing graph schema 4 need not invalidate still-correct v1 observations.

Each local definition stores its existing binding ID, declaration source,
deterministically ordered use sources, sanitized initializer SourceExpression,
flat expression arena and initializer dependencies. The original condition
dependency or another initializer dependency must justify each use and identity.
Initializer dependencies reuse the existing binding/target/unresolved resolution
contract; their container supplies the initializer role.

The arena has a root index and nodes in deterministic producer order. Every node
contains a SourceExpression and a typed shape. Child/dependency indices are bounded
local references; no raw identifier/property/operator field bypasses sanitization.
Supported shapes are literal, identifier/runtime input, property name, member read
(object, property, computed and optional flags), parentheses, unary, binary,
logical and explicit unsupported nodes. Member values always remain unresolved,
even when the property name or a const object binding is known.

Operators use closed enums. Preserve ordinary JavaScript equality/comparison,
arithmetic, bitwise, `in` and `instanceof` binary operators; unary not, plus,
minus, bitwise-not, typeof and void; and logical and/or/nullish. Capturing these
operators is source structure, not constant folding or arithmetic interpretation.
Preserve operand order, grouping and short-circuit/optional structure exactly.
Calls, new, assignment/update/delete, callbacks, await/yield, templates, regex,
type wrappers and other unsupported forms retain an explicit Gap rather than
being approximated as a simpler expression. No source is executed or substituted.

## Bounds, sanitization and validation

Use the existing source-aware sanitizer on each original expression node and
initializer before storing display or literal data. Preserve fixed redaction
reasons and original byte offsets, including withheld descendants. A sanitized
CompleteSyntax flag does not establish supported semantics or runtime values.
Do not expose secrets through operator fields, new identifiers, diagnostics,
unsupported fallbacks or secret-only hashes.

Initial hard caps per rule are 16 definitions, 64 arena nodes total across all
definitions, 128 initializer dependencies total, expression depth 8 and definition
chain depth 4. Keep the existing rule dependency cap 128, rule payload 64 KiB and
per-file rule payload 1 MiB. Bound producer traversal independently of emitted
data; budget exhaustion emits a fixed analysis/definition-limit Gap. Never mark
a truncated expression complete. Existing whole-rule omission behavior applies
when no valid bounded payload fits.

Before deserializing v2 nested data, bound its shape and total serialized size.
Validation checks collection and text bounds, known forms/operator arities,
reachable acyclic arenas, valid indices, depth, same-source identity, child spans
within their original parent expression, initializer/root agreement, declaration
and use coherence, duplicate identities/uses, and referenced dependency/Gap shape.
Validation proves wire consistency only; source existence, sanitizer correctness,
lexical ownership and producer authority still require the actual producer.
V1 decoding retains its earlier contract without inventing new evidence.

## Source traversal and receipts

Extend the shared source visitor to every declaration, use, initializer,
expression node, initializer dependency/declaration and redaction. Cached ordinary
extraction must retarget every nested revision and recompute the complete rule
hash. The captured lane continues to parse retained bytes and bypass the cache.

New receipts use a v2 producer contract, schema and content-hash domain/prefix.
Keep v1 receipt decoding, identity, exact grammar/producer tuple and range order
unchanged. Validation dispatches by known version; mismatched or unknown tuples
fail. Source captures/manifests, raw CaptureFileRef and immutable proposal/review
schemas do not change. New production does not mint a v1 receipt over a v2 rule.

Preserve the prior inventory roles and append per-definition groups before rule
redactions: declaration by definition index, uses by a global role counter,
initializer by definition index, expression nodes by global arena-order counter,
initializer dependencies by global counter and optional dependency declarations
with that same index. Validate contiguous groups/counters and required members.
Retain every occurrence, even when root/initializer or use spans are identical.
The complete fact digest and exact inventory must agree. Keep the 1,024-range and
128-KiB receipt caps; omit an oversized whole receipt, never trim its ranges.

Current graph associations still publish atomically with their complete facts.
Changed v2 facts/receipts replace current associations; historical v1 receipts
remain inspectable according to their original capture availability. Retain
`primary_source_only` and `input_closure_not_established`; definitions neither
establish complete producer input closure nor activate accepted proposals.

## Shared presentation and evaluation

Add a Local const initializers section to the existing Source rule evidence
artifact. Show binding/use/declaration, stored initializer and structured
expression evidence, capture status, source citations and remaining gaps. State:
"Initializer as written; value at use and business meaning are not established."
V1 records state that definition evidence was not collected. Shared context
returns the same typed properties without source reads. Escape source-looking
Markdown/HTML, retain provenance/export filtering and the Workbench's incomplete
interpretation banner. Definition counts never become complete-rule counts.

Freeze the local-definitions review plan before generating new output. Preserve
the existing Vendure oracle, 17-file input manifest and baseline/score. Produce
separate revision/hash/run/score artifacts and repeat deterministic source-only
extraction using the unchanged pinned input. Inspect the stock tracking predicate
against original source; also inspect the mutated-array initializer and unsupported
dependencies as negative cases. Report changed definition evidence independently
of the frozen eight-rule denominator and 41 decision cases. No case is passed
without actual execution; no complete rule is claimed without every oracle field.

The ordinary source-only example is an observation benchmark, not proof of
production captured input closure. Actual captured parser, receipt, SQLite,
shared-context and export integration tests provide their own scoped evidence.
Configuration/consumer interpretation, query/collection behavior, full input
closure (#385), H3 curation and MCP/ACP execution remain subsequent obligations.
