# Cartograph — agent context

This is the shared context file for all coding agents (and humans acting like
them). It holds the product invariants and the SDLC workflow. Tool-specific
files (`CLAUDE.md`) only add orientation; they must not duplicate this file.

## What Cartograph is

A cross-layer **spec-recovery engine** (Tauri 2, Rust core, React UI). It
ingests the repos of a running system and recovers its specification as a
provenance-tagged knowledge graph across five layers (infra, cloud, server,
events, client), then compiles official spec artifacts. Master spec:
`docs/SPEC-00_master.md`. Read it before touching product code — it answers
~95% of build-time questions.

## Product invariants (violating these fails review)

- **Escalation ladder:** every fact is produced by the lowest tier that can
  establish it — T0 deterministic > T1 dynamic > T2 semantic > T3 agentic.
- **R-INT-1** T2/T3 never overwrite or upgrade a T0/T1 fact.
- **R-INT-2** Every node/edge stores tier + confidence; inferred content is
  never indistinguishable from confirmed — in code, UI, or exports.
- **R-INT-3** Agents (T3) are propose-only; no write access to confirmed facts.
- **R-INT-4** A flow with an unresolved hop is emitted partial with an explicit
  Gap node — never silently completed.
- **R-INT-5** `verified-only` export excludes InferredWeak; `best-effort`
  includes it clearly annotated.
- **Non-goals:** no editing of target code (NG1); no code
  regeneration/scaffolding (NG2 — revisit criteria in `docs/VISION.md`);
  no multi-user backend in v1 (NG5).
- **Deterministic tier never calls the LLM.** Local-first; cloud egress is
  per-tier opt-in with explicit consent (fail closed).
- **Determinism:** re-ingesting the same commit yields an identical graph
  (content-hash equality) — a CI invariant from M10 on.

## SDLC workflow (strict, from the first PR)

1. **Work starts from an issue.** GitHub Issues + the Cartograph project board
   are the canonical tracker; milestones M0–M10 mirror SPEC-00 §14. No issue →
   create one first (templates in `.github/ISSUE_TEMPLATE/`).
2. **Branch per change** (`feat/…`, `fix/…`, `chore/…`, `docs/…`); never commit
   to `main` directly.
3. **Spec before code.** If behavior changes, update `docs/user_stories.md`
   (US/AC schema at the top of that file) and `docs/US-TM.md` in the same PR.
   Architectural decisions get an ADR in `docs/adr/` (format: Status/Date/
   Deciders, Context, Decision, Consequences, Alternatives ≤3).
4. **PR body links its issue** with a closing keyword (`Closes #N`) — merged
   PRs close their issues automatically via
   `.github/workflows/close-linked-issues.yml`.
5. **Gates must pass.** CI runs docs/traceability, Rust, and frontend gates
   (see "Verification before done"). Gates are ratchets — they only get
   stricter.
6. **Traceability is enforced**, not aspirational: `node
   scripts/check-traceability.mjs` validates US ↔ AC ↔ matrix ↔ ADR
   consistency and runs in CI. Run it before pushing docs changes.

## Verification before "done"

```sh
node scripts/check-traceability.mjs
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
npm --prefix ui run lint && npm --prefix ui run typecheck && npm --prefix ui run test && npm --prefix ui run build
```

To see the app itself: `npm run tauri dev` (root). This section is the single
place that lists the gates; CI mirrors it exactly.

**UI components ship with a story.** `ui/` components are presentational
(props in, callbacks out) with a `*.stories.tsx` next to them; interaction
behavior goes in `play` functions (they run as tests via the vitest storybook
project — part of `npm run test`). Stories needing backend data mock the Rust
core with `mockIPC` from `@tauri-apps/api/mocks` (pattern: `App.stories.tsx`).
Storybook itself: `npm run storybook` (from `ui/`).

## Documentation map

- `docs/SPEC-00_master.md` — master spec (single source of truth)
- `docs/user_stories.md` / `docs/US-TM.md` — US/AC + traceability matrix
- `docs/adr/` — ADRs for *this app's own* decisions
- `docs/VISION.md` — post-v1 direction (SDLC-in-core); not license to build it
- `docs/design/` — design tokens (`DESIGN.md`) + Stitch mockups per view
- GitHub wiki — narrative contributor/process docs (`CONTRIBUTING.md` is a stub)
