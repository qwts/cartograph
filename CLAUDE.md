# Cartograph — Claude Code guide

Start with **`AGENTS.md`** — it holds the product invariants (escalation
ladder, R-INT-1..5, non-goals) and the SDLC workflow (issue → branch → spec →
PR → gates). This file only adds Claude-specific orientation; do not duplicate
`AGENTS.md` here.

## Orientation

- Layout: Rust workspace per SPEC-00 §8.1 (`crates/*` = analysis engine,
  `src-tauri` = the `app` shell crate, `ui/` = React front end). Crates beyond
  `core-graph`/`core-prov` are doc-comment stubs until their milestone.
- When implementing a milestone, work from its exit gate in SPEC-00 §14 and
  the user stories mapped to it in `docs/US-TM.md`.
- SPEC-00 §15 lists four "verify-at-build" claims (Kuzu fit, `hcl-rs`
  coverage, `usearch` bindings, OTel ingest format) — confirm these against
  current reality before relying on them; they may have drifted.
