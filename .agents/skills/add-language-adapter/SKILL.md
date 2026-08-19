---
name: add-language-adapter
description: Scaffold a new Cartograph language adapter — a sandboxed WASM plugin for a language nobody covers yet, or an extension to an existing compiled-in crate. Use when asked to "add support for <language>", "write an adapter for X", or "why doesn't Cartograph parse <language>".
---

# Add a language adapter

Full reference: [`docs/adapters/AUTHORING_GUIDE.md`](../../../docs/adapters/AUTHORING_GUIDE.md).
Read it before scaffolding anything — this skill is the short version; the
guide has the exact file shapes, sandbox limits, and golden-corpus format.

## 1. Figure out which path applies

Ask yourself (or the user, via AskUserQuestion, if it's not obvious from
context):

- **Is this language already partially covered by an existing compiled-in
  crate's grammar family?** (e.g. plain JavaScript under
  `crates/adapters-lang-ts`'s TypeScript grammar, or a JSX/TSX-shaped
  variant.) → **Path B** (compiled-in crate extension). This needs a
  maintainer PR into this repo.
- **Is this a language nothing in `crates/adapters-lang-*` or
  `crates/iac` touches at all?** → **Path A** (WASM plugin). This can ship
  without any core-repo change.

Check `crates/ingest/src/preflight.rs`'s `INSTALLED_ADAPTERS` and
`PLANNED_ADAPTERS` first — they're the single source of truth for what's
covered, what's a known "planned" gap, and what's entirely unnamed. Don't
guess from memory; grep the file.

Default to Path A unless there's a clear grammar-family match — that's the
contribution path this project's own architecture (ADR-0017) is built
around, and it needs no core-repo PR.

## 2. Path A — WASM plugin

1. Fork `crates/adapters-plugin-host/tests/fixtures/ok-adapter` into a new,
   standalone crate (its own `[workspace]`, `crate-type = ["cdylib"]`,
   depends on `wit-bindgen` plus whatever pure-Rust parsing you need — no C
   toolchain, since cross-compiling a C-based tree-sitter grammar to
   `wasm32-wasip2` needs a WASI C sysroot that usually isn't available
   without extra setup; a pure-Rust parser avoids that entirely).
2. Implement `extract_source` against `wit/adapter.wit` — parse `source`
   bytes, emit `Node`/`Edge` records. Follow existing id/label conventions
   (`file:{repo}@{path}`, `sym:{repo}@{path}#{name}`, `IMPORTS`/`CALLS`
   edges) unless the language genuinely needs something else — check
   whichever compiled-in adapter (`adapters-lang-go` is a good compact
   reference) is closest in shape to the target language for what facts
   actually matter.
3. Build: `rustup target add wasm32-wasip2` (once), then
   `cargo build --target wasm32-wasip2 --release`.
4. Write `{plugin-id}.golden.json` next to the compiled `.wasm` — exact
   expected nodes/edges per source fixture, `extensions` naming the
   coverage claim. Don't hand-write provenance/pinned ids — the gate
   computes those. See the guide's golden-corpus section for the exact
   shape and the fixed `{repo: "golden", commit: "golden"}` source id the
   gate uses.
5. Drop both files into `.cartograph/adapters/` in a test repo, run the
   conformance gate from Settings → Adapters, iterate until every check
   passes, then enable it and confirm the Preflight `uncovered-language`
   finding for that language closes.

Sandbox reality check before you write a line of guest code: no filesystem,
no network, no ambient clock, ~10B fuel units, 64MB memory, 5s deadline per
call. Read the adversarial fixtures next to `ok-adapter`
(`busy-loop`/`memory-hog`/`net-probe`/`clock-probe`) if anything about your
approach might brush against those.

## 3. Path B — extending a compiled-in crate

This touches the trusted core — only do this as (or on behalf of) a
maintainer landing a real PR.

1. Match the existing crate's free-function convention (`SourceId`,
   `IncrementalCache`, `Extraction`, `extract_dir_incremental[_with_progress]`)
   — there's no shared Rust trait to implement, so copy the shape from the
   crate you're extending.
2. Wire the new extension/branch into `extract_tree_incremental`
   (`src-tauri/src/main.rs`), merging `Extraction`/`LayerBreakdown`/
   `DeltaSummary` like the existing calls do.
3. Update `INSTALLED_ADAPTERS` (and remove from `PLANNED_ADAPTERS` if it was
   there) in `crates/ingest/src/preflight.rs` — the single registry driving
   both Preflight and Settings. If the new entry shares an `id` with an
   existing one (same crate, e.g. JS sharing `t0.adapter-ts` with TS), check
   whether any frontend code keys a list by `id` — it needs to key by
   `language` instead, or the two rows collide as a React key.

## 4. Either path: traceability

Add an AC to `docs/user_stories.md`, a matching row in `docs/US-TM.md`, and a
`docs/test-map.md` entry naming the real test(s). Run
`node scripts/check-traceability.mjs` before considering this done — it's
required by `AGENTS.md`'s SDLC and fails closed on drift between the three
files.

## 5. Verify, don't just claim

Run the new/changed crate's test suite (`cargo test -p <crate>`) and, for
anything touching `preflight.rs` or the Settings/Preflight UI, the relevant
Storybook story tests (`npm test` in `ui/`, or a targeted
`npx vitest run <file>`). A plugin's conformance gate passing is itself a
real, meaningful verification step — treat a failed gate check as a bug to
fix, not a hint to weaken the golden corpus.
