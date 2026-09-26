# ADR-0035 — Erasure-level parameter signatures in JVM method identity

- **Status:** Proposed in #435 (owner review)
- **Date:** 2026-09-24
- **Deciders:** Cartograph owner; implementation agent

## Context

The Java adapter's method/constructor symbol id was
`sym:<repo>@<path>#<Class>.<method>`, with no parameter signature. Every
overload of a method collapsed onto one id, so the store kept only the last
declaration extracted: its span, its props, and one merged `DEFINED_IN`. The
other overloads vanished as distinct facts, and calls resolved to whichever
overload happened to be stored last — silently wrong, not a Gap, contrary to
R-INT-4.

Measured on spring-petclinic @818c413: `Owner.getPet` (×3) and
`VetRepository.findAll` (×2) collapse to one declaration each. After #242
these surface as node identity collisions in the ingest summary (AC-0195), so
they are visible, but they are still merged.

## Decision

1. **Method/constructor symbol ids carry an erasure-level parameter
   signature.** `param_type_list` reads each formal parameter's declared type
   from tree-sitter (`spread_parameter`/varargs gets a `"..."` marker), and
   `signature_suffix` renders it as `(Type1,Type2)`, appended to the existing
   qualified name: `sym:<repo>@<path>#Owner.getPet(String)`. No-arg methods
   get `()`. This is erasure-level, not full JVM erasure: it is the source
   text of each parameter's type, not a resolved/imported FQN — sufficient to
   separate overloads within one file, not to disambiguate identical-looking
   type names imported from different packages.
2. **Call resolution becomes arity-aware, same-class and cross-file.** A call
   site's argument count (`call_arity`) is matched against each candidate
   method's `min_arity`/`variadic` (`arity_matches`) rather than assuming a
   single target:
   - Exactly one candidate accepts the call's arity → a Confirmed `CALLS`
     edge to that overload's symbol id.
   - Zero or more than one candidate accepts it → an explicit Gap
     (`gap:overload:<repo>@<path>@<byte>`) with a reason naming which case
     ("no local overload accepts this call's argument count" /
     "ambiguous overload: multiple local methods accept this argument
     count"), never a silent guess.
   Cross-file calls through an imported type build both an `unresolved_gap`
   (the existing "unresolved Java import target" case) and an `overload_gap`
   at call-extraction time, since provenance spans are only available
   per-file; the directory-join pass picks whichever applies once the
   declaring type, if any, is found.
3. **Static-import member proof becomes prefix-based.** A static import names
   a method with no parameter list (`import static Foo.bar;`), so it cannot
   prove which overload a call reaches. `resolve_repo_imports` now proves the
   import boundary by prefix (`symbol == target || symbol.starts_with(target
   + "(")`) instead of an exact id match, so at least one local overload
   existing is enough to keep the import Confirmed; call resolution still
   arity-matches to pick (or fail to pick) the specific overload.
4. **Graph-contract / schema-version change.** Every JVM method and
   constructor symbol id changes shape. `GRAPH_SCHEMA_VERSION` bumps 5 → 6:
   per ADR-0008/ADR-0031 precedent, a mismatched db is cleared and rebuilt on
   open rather than migrated in place — the graph is a disposable ingest
   artifact, not a system of record, so there is no migration to write.
   Existing stores re-key their JVM method/constructor facts and Gap register
   on the next re-ingest; content hashes change.

## Consequences

- `Owner.getPet(String)` and `Owner.getPet(int)` become two distinct
  Confirmed facts instead of one collapsed one. Calls to each resolve to the
  matching overload by argument count; a call this adapter cannot arity-match
  (an ambiguous or unmatched overload set) becomes an explicit Gap instead of
  a silent wrong edge.
- This is erasure *by source text*, not by resolved type identity: two
  parameters spelled `String` in one file and `java.lang.String` in another
  are treated as different signatures even though the JVM would treat them
  the same after full resolution. This can under-merge (treat two spellings
  of the same overload as different) but never over-merges an ambiguous call
  onto a wrong target, which matches R-INT-4's fail-closed requirement.
- Overload resolution is arity-only, not type-only: `foo(String)` and
  `foo(Object)` are not distinguished by argument count, so a call to either
  is ambiguous and becomes a Gap rather than a guess. A future T0/T1 pass
  could narrow this with literal-argument type inference; out of scope here.
- `GRAPH_SCHEMA_VERSION` also changed in the concurrently-developed #464
  (proven-external JVM import package ids, ADR-0033/PR #527), which touches
  the same adapter file. The coordinator decided #527 merges first and claims
  4 → 5; this PR was rebased onto that outcome and now bumps 5 → 6 instead of
  reusing #527's 5. This PR therefore depends on #527 merging first — if
  #527 has not merged yet, this bump (and the doc comment above
  `GRAPH_SCHEMA_VERSION`) needs re-checking before merge.

## Alternatives

1. Full JVM-erasure resolution (resolve each parameter type to its FQN via
   the whole classpath before minting an id): rejected for this adapter's T0
   tier. It requires classpath resolution this adapter does not do anywhere
   else, is a much larger change, and the arity-only fallback already fails
   closed to a Gap instead of guessing — a future T1/semantic pass can narrow
   ambiguous cases without another id-scheme break.
2. Keep one id per method name and store all overloads' spans as a list on
   one node: rejected. It breaks the one-node-one-fact model every other
   adapter and the store's identity-collision reporting (AC-0195) assumes,
   and every consumer of `DEFINED_IN`/`CALLS` would need to learn a new
   one-to-many shape.
3. Leave overload calls unresolved (always Gap, never match) until full type
   inference exists: rejected as strictly worse than arity-matching — most
   real overload sets differ in arity, so arity-matching resolves the common
   case correctly today while still failing closed on the genuinely
   ambiguous remainder.
