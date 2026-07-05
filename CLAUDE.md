# Cartograph — Claude Code guide

Start with **`AGENTS.md`** — it holds the product invariants (escalation
ladder, R-INT-1..5, non-goals) and the SDLC workflow (issue → branch → spec →
PR → gates). This file only adds Claude-specific orientation; do not duplicate
`AGENTS.md` here.

## Orientation

- Pre-M0: the repo is spec + process; product code arrives with M0
  (Tauri 2 skeleton, Rust workspace per SPEC-00 §8.1 crate map).
- When implementing a milestone, work from its exit gate in SPEC-00 §14 and
  the user stories mapped to it in `docs/US-TM.md`.
- SPEC-00 §15 lists four "verify-at-build" claims (Kuzu fit, `hcl-rs`
  coverage, `usearch` bindings, OTel ingest format) — confirm these against
  current reality before relying on them; they may have drifted.

## Conventions

- ADRs: next free number in `docs/adr/`, format per existing files.
- User stories: fixed schema at the top of `docs/user_stories.md`; AC ids are
  globally sequential (AC-XXXX), test ids reserved as T-XXXX.
- Run `node scripts/check-traceability.mjs` after any docs/spec edit.
- Use `gh` for GitHub work (issues, project board, PRs). PR bodies must
  include `Closes #N`.
