# ADR-0012 — Explicit Markdown ADR directives and derived decision overlays

- **Status:** Accepted
- **Date:** 2026-07-13
- **Deciders:** Cartograph maintainers

## Context
US-0013 must distinguish author-written decisions from decisions inferred from
the recovered graph, link both to governed targets, and surface code conflicts
without allowing inference to masquerade as confirmed intent. Arbitrary natural
language cannot establish a Confirmed target or prohibition at T0, while making
the graph store itself hold generated drafts would blur ingest facts with a
disposable spec projection.

## Decision
- T0 scans confined Markdown ADR/RFC paths and creates Confirmed `ADR` nodes.
  It creates `DECIDES` edges only for existing graph ids explicitly listed by
  `Governs:` or cited exactly in backticks. After every repo is loaded, a
  deterministic relink pass evaluates those ids against the full system graph.
- An optional `Forbids:` field lists uppercase graph-edge labels. It is an
  explicit, deterministic author constraint rather than a natural-language
  guess.
- The spec compiler derives recovered ADR drafts in its disposable projection.
  v1 recognizes evidence-backed asynchronous channel architecture and emits a
  `Semantic` inferred ADR plus `DECIDES`, retaining all cited graph spans.
- A found ADR constraint and a governed conflicting edge produce a `Drift`
  projection with `DRIFTS_FROM`/`CONFLICTS` links. The finding retains the
  offending edge confidence and records the exact edge plus containing flow
  triggers.
- Derived facts never mutate confirmed graph input. Recovered/inferred ADRs and
  inferred drift remain subject to the Workbench export and curation policies;
  supporting facts are filtered by those policies before any derivation runs.

## Consequences
- Found author intent is clearly separated from recovered proposed intent.
- T0 linking fails closed when target ids are ambiguous or absent.
- Drift findings are reproducible and directly traceable to decision text,
  graph evidence, and flows.
- Broader natural-language decision recovery can extend the T2/T3 projection
  later without changing the Confirmed Markdown contract.

## Alternatives (≤3)
- **Fuzzy-match prose at T0** — rejected because similarity cannot establish
  Confirmed author intent.
- **Persist generated ADRs in the graph** — rejected because compilation and
  re-ingest would mix proposed facts with confirmed extraction output.
- **Require YAML front matter** — rejected because simple Markdown fields fit
  existing ADR files and remain human-readable without another parser.
