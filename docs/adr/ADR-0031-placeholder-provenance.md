# ADR-0031 — Placeholder provenance and proven import boundaries

- **Status:** Proposed in #237 (owner review)
- **Date:** 2026-09-22
- **Deciders:** Cartograph owner; implementation agent

## Context

Every T0 adapter closes over edge endpoints no parsed declaration defines, so
the store keeps referential integrity. Those placeholder nodes were minted as
`{"placeholder": true}` with no `prov`. `spec::provenance` then forced each one
to the explicit-Gap fallback (`spec.invalid-provenance`), so every import of an
external package became a "system gap": 206 on a clean spring-petclinic, and
about 72k on VSCode (#237, dogfood #253). The Gap register grouped them under
the internal label "unresolved Module edge". The noise drowned the real gaps
and broke R-INT-2: a fact without provenance could not show why it was there.

## Decision

1. **Every placeholder carries provenance.** One shared closure
   (`core_graph::placeholder`) cites the first edge in deterministic extraction
   order that names the endpoint. It records that edge's evidence and producing
   extractor, a human-readable `reason`, and a `boundary`. `placeholder: true`
   stays, so orphan cleanup, file counts, and T1 resolution keep working.
   `spec.invalid-provenance` remains only as an integrity backstop.
2. **Confirmed only on proof. Anything else stays a Gap.** Each adapter
   classifies its own import targets against the whole tree it walked:
   - `external` (Confirmed): the repository provably cannot provide the target.
     - JVM: no Java or Kotlin source declares the package, lies beneath it, or
       sits inside a package that encloses it. The header scan skips
       multiline file annotations; if any header cannot be parsed, no
       import is proven external.
     - Go, in module mode: a standard-library path, or a module outside every
       repository `go.mod` and local `replace`.
     - Python: no repository directory or module has the same top-level name.
     - JS/TS: a bare specifier that matches no tsconfig alias or `baseUrl` path,
       workspace package, `#` subpath import, or repository source directory,
       and that names a valid npm package. When a `package.json` declares the
       package, its declaration is cited too. Inherited tsconfig settings are
       not loaded, so under a tsconfig that `extends` another only a
       manifest-declared dependency is proven external.
   - `internal` (Confirmed): the repository declares this package but no single
     file declares the target. Examples: a JVM package, a Go package directory,
     a workspace package cited by its manifest, or an existing non-source file
     (stylesheet, image, JSON) that a relative JS/TS import names.
   - `unresolved` (Gap): everything else. This covers in-system imports with no
     unique declaration, relative misses, symbols, resources, Go without a
     `go.mod`, and any case the adapter cannot decide.
   A Confirmed boundary without citable evidence fails closed to a Gap. A
   placeholder named by several references is classified from all of them:
   one unresolved reference keeps it a Gap, and references that disagree on
   the boundary kind prove neither. A relative asset import that climbs
   above the repository root, and a Go import path with `..` or `.`
   elements, prove nothing and stay Gaps.
3. **Graph-contract changes.** JVM `IMPORTS` edges whose complete target (a
   type, a nested type, a Kotlin top-level function, or a member whose
   `Symbol` the declaring type defines) the repository declares exactly once
   now point to the declaring `file:` node, matching resolved TS relative
   imports. A prefix alone never retargets: `a.Foo.Missing` stays a Gap. A relative
   JS/TS import of a non-source asset keeps its real path (`file:…/x.css`)
   instead of the extensionless `.ts` guess (`x.css.ts`). Other ids and labels
   do not change: external targets stay `Module` nodes with `mod:` ids.
   Placeholder props gain `prov`, `reason`, and `boundary`, so graph content
   hashes change on re-ingest. An observed (T1) Terraform state resolution
   replaces the placeholder's Gap provenance with the observation's.

## Consequences

- Before/after gap findings (T0 adapters, store semantics). spring-petclinic
  drops from 206 to 0, with 195 Confirmed external modules and 11 in-repo
  imports resolved to files. excalidraw drops from 13,517 to 13,156. What
  remains is 13,020 existing Gap nodes, which this ADR leaves unchanged, plus
  136 unresolved in-repo symbols, module paths, and bare imports under an
  extending tsconfig. On VSCode, placeholder
  gaps drop from 1,920 to 1,128 (the ~72k in #237 predates other fixes), and
  793 placeholders become Confirmed boundaries. The external classification also removes most of the input
  that inflates #240 and #244.
- "External" means "outside the recovered tree". The classifiers read the
  same `.gitignore`-aware walk as the extractors (ADR-0030). A package that
  exists only in sources T0 skips (such as generated output under `target/`
  or a path the tree's `.gitignore` excludes) and shares no prefix with a
  declared package counts as external. The reason text states
  exactly what was proven.
- A bundler-only alias (webpack or Vite `resolve.alias`) that is spelled as
  a valid package name and matches no tsconfig alias or source directory
  counts as external. T0 does not read bundler configs, and this limit is
  tracked separately (#463).
- `mod:` ids that name an imported type rather than a module are an
  id-scheme question, tracked in #464.
- Shared external modules (such as `mod:react`) have global ids. Two repos in
  one system, or the Java and Kotlin adapters in one tree, now publish the
  same id with different evidence. Before this change every placeholder had
  identical empty props. The store keeps the last occurrence (SPEC-00 §4.4),
  and the ingest summary reports these as node identity collisions
  (AC-0195). The evidence then cites whichever occurrence came last.
- Legacy graphs still hold placeholders without provenance until their next
  re-ingest. Until then the register falls back to the old cause label.

## Alternatives

1. Treat only manifest-declared dependencies (pom, package.json, go.mod
   `require`) as external: rejected as the gate. Transitive packages such as
   `jakarta.persistence` via a Spring Boot starter would stay false gaps.
   Manifests are cited as corroboration when present.
2. Add a new `External` node label: rejected. SPEC-00 §4.1 already has
   `Module`, and flows already end at "external". A new label would change the
   graph contract for every surface for no added meaning.
3. Hide placeholders from the register without provenance: rejected. The facts
   would still carry no evidence (R-INT-2), and in-system misses would
   disappear silently (R-INT-4).
