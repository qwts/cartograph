# SPEC-02 — Source-backed rule evidence

Status: implementation in progress in #381. Extends SPEC-01's H2 gate;
it does not by itself fulfill the Vendure cart-readiness benchmark.

## Purpose and stages

Business-rule recovery needs real lexical owners, source predicates, dependencies,
and the consumers that give local return values their behavioral meaning. The
slice repairs TypeScript callable ownership and adds conservative guarded-exit
evidence and a rule inventory. Five conditional returns alone do not
count as five recovered Vendure entry constraints.

## Callable ownership

- Object-literal methods and property-bound arrow/function callbacks are actual
  source symbols with exact T0 provenance and DEFINED_IN edges. An object nested
  within a class must not have its method attributed to that enclosing class.
- Existing unambiguous top-level function/class-method identities remain stable.
  New object/nested callable identities include their lexical scope and a stable
  source-position discriminator where necessary. Same-named methods in separate
  objects and shadowed nested functions must not merge.
- The nearest actual callable owns its calls. Nested callbacks do not assign
  their calls or future guarded exits to an outer function. Computed property
  names remain source observations; they do not establish a runtime property key.
- Every emitted call-owner identity must refer to a real emitted symbol rather
  than a synthetic uncited placeholder. Existing anonymous route identities must
  remain consistent between symbol extraction and endpoint handling.
- Direct calls resolve only to a scope-proven callable binding. A file-wide name
  match must not resolve a call to a shadowed or unrelated nested declaration.
  Unknown member dispatch is not made confirmed just because an object has a
  similarly named method. Unsupported dispatch stays unresolved.
- Identity and provenance are deterministic for unchanged input. No model or
  runtime execution is used; source text is treated as data.

Graph schema version 3 invalidates disposable version-2 graph caches on upgrade.
Repositories must be re-ingested before the corrected ownership facts appear;
the existing graph-version mechanism must not keep old ambiguous Confirmed
links visible. Target source and separately stored durable job history are not
part of this cache invalidation.

## Guarded-exit evidence

Recover exact enclosing branch expressions, branch polarity, source order,
lexical owner, and return/throw expressions as observations. Preserve JavaScript
operators and short-circuit structure without strengthening them into business
language. A returned string is a local return; rejection requires cited consumer
evidence. Nested-function returns belong to the nested function.

An ancestor-condition list is not a complete execution predicate: previous exits,
mutation, loops, switch statements and try/finally can change behavior. Explicit
completeness and dependency gaps must accompany unsupported interpretation.
Rule facts must not silently incorporate outer-function conditions into a nested
callback's execution predicate.

Before storing source expressions, use a shared deterministic redaction boundary
for secret-shaped literals, retaining exact source references and marking any
redaction. This contract must be specified and tested before that stage emits raw
source text into graph properties or exports. Ownership alone stores no new
predicate or return-value text.

### Redaction boundary

Use a pure `core-redact` leaf crate for the existing provider-token, bearer-token,
private-key and credential-assignment recognizers from `llm`, and the compact
token-shaped-value detector from `ingest::toolchain`. Preserve those callers'
current substitution/omission behavior, consent payloads and idempotence. The
crate has no provider, network, filesystem, environment or model dependencies.
T0 source recovery must not depend on the `llm` or `ingest` crate.

Recognizers are shared; policies are distinct. Free-text replacement is not a
source parser: `password=false` must remain a typed boolean in source evidence.
The dependent source-expression sanitizer inspects decoded literal values and
AST-proven sensitive binding/property context before persistence. Withheld
strings retain their type and original source references, but no raw value or
secret-only digest. Unsafe comments are removed/sanitized too. Unsupported
decoding never falls back to raw text. Safe operators, booleans and null retain
their semantics. Long business message strings may be withheld by conservative
shape detection; report that loss rather than special-case benchmark names.

### Fact contract for the rule pass

Store a versioned typed payload under `BusinessRule.props.rule`, separate from
the node's ordinary provenance. It contains an actual `owner_id`, `exit_source`,
source order within that callable, outermost-to-innermost same-callable branch
conditions (expression and truthy/falsy polarity), and a typed local Return or
Throw effect. Each expression holds its original evidence reference, syntax kind,
sanitized display, CompleteSyntax/Redacted/Unsupported capture status, and typed
literal information when applicable. Literal values are either known typed
scalars or explicitly withheld with reasons.

Dependencies identify their Condition/ExitValue/ControlFlow role and source span,
and either a scope-proven binding/declaration, an actual graph target, or a Gap.
Interpretation separately records that a complete execution predicate and a
consumer effect have not yet been established. Explicit redaction records retain
source spans and fixed reason codes. Put these shared data types in
`core-graph::rules`, with a dependency on the existing pure provenance crate.

Link rules to actual owners through GOVERNS. Use explicit Gap nodes and
`DEPENDS_ON(rule → Gap)` edges with Gap confidence for unresolved interpretation
or prerequisites; reason codes include unresolved_call, preceding_exit, mutation,
loop_dependency, switch_control, exception_control, redacted_expression, and
consumer_semantics_unknown. Diagnostics must not interpolate raw source. Bound
gap production per rule and deduplicate reasons; this is not a path-enumeration
algorithm. Hash canonical sanitized facts and references, never separately
serialized secrets.

The initial pass always reports `execution_predicate_unknown` and
`consumer_semantics_unknown`. Unproven value bindings use `unresolved_binding`;
after 128 dependency observations, `dependency_limit` records omitted analysis.
Rules recovered from decoded eval strings are deferred with an
`eval_source_mapping_unknown` Gap until decoded offsets can be mapped to the
original literal. Existing eval symbol/call recovery remains available. Reused
source parses retarget every nested rule evidence reference to the new commit.
Capture is bounded per file: at most 256 rules and 1 MiB of serialized rule
payloads; each rule permits 32 conditions, 256 ancestor steps, 8 KiB per captured
expression and 64 KiB of serialized payload. Exceeding a capture/work bound emits
an `analysis_limit` Gap; an oversized rule is omitted explicitly rather than
published as a complete observation.

Inventories/exports render stored sanitized fragments. They must not append raw
evidence dereferences automatically; later MCP evidence-content tools apply the
same disclosure policy. Explicit local source viewing remains a separate surface.

## Inventory and benchmark

A rule inventory must expose the owner, source conditions, local effect,
dependencies, producing tier, source evidence, and incomplete interpretation.
It must not label all conditional returns as established business validations.
Shared context and exports use the same facts and preserve R-INT-5 filtering.

The pinned [Vendure benchmark](evals/vendure-cart-readiness.md) requires the
configuration, callback consumer, query filters, collection/stock behavior and
supporting rules before full matches can be recorded. Generic fixtures with
renamed methods and options prove extraction generality; benchmark-specific
identifiers must not appear in production matching logic.
