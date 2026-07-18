# ADR-0018 — multi-platform port scope: Windows and Android/iOS reader, tvOS/visionOS non-goals

- **Status:** Proposed
- **Date:** 2026-07-18
- **Deciders:** Chris Kane

## Context

ADR-0001 names "macOS primary, Windows" but nothing since has treated any
platform beyond the macOS shell: `package.yml` builds `macos-latest` /
`universal-apple-darwin` only, ci.yml is all `ubuntu-latest`, and ADR-0016 is
Developer ID distribution. A port to iOS, iPadOS, tvOS, visionOS, Android and
Windows was requested, so this ADR fixes scope before any code moves.

Three facts from the spike reframe the request:

1. **Tauri 2 has no tvOS or visionOS support** and no credible path to it —
   visionOS is an unmerged feature request open since June 2024 spanning wry,
   cargo-mobile2, swift-rs and ring; tvOS is not tracked at all. Rust's own
   `aarch64-apple-visionos` target exists, so the blocker is the webview and
   windowing layer, not the core.
2. **The Apple mobile family cannot run our WASM plugin host.**
   `adapters-plugin-host` builds a wasmtime `Config` with Cranelift's JIT
   (`crates/adapters-plugin-host/src/lib.rs:238`), and iOS denies
   writable-executable memory to third-party apps. Independently,
   App Store guideline 2.5.2 forbids downloading code that changes app
   features — enforced sharply against developer tools in March 2026. So
   ADR-0017's *runtime-loadable, AI-authorable* premise is blocked twice over
   on iOS: once by the CPU, once by policy.
3. **The product's core input model is an arbitrary user directory.** Ingest
   canonicalizes user-supplied absolute paths (`src-tauri/src/main.rs:1422`,
   `:2137`, `:2495`, `:2522`) and evidence resolves `root + rel_path`
   (`src-tauri/src/evidence.rs:78`). iOS security-scoped bookmarks and Android
   SAF content URIs are not `Path`s, and NG5 forbids a v1 server to offload to.

Kuzu, the dependency that would have been the hardest to cross-compile, is
already gone (ADR-0008). The remaining native deps — bundled SQLite, usearch,
tree-sitter grammars, `git2` with vendored OpenSSL — all cross-compile; they
are CI cost, not blockers.

## Decision

Adopt a **three-tier platform policy**, and treat the port as two independent
workstreams rather than one "go multi-platform" epic.

1. **Tier 1 — Windows, full parity.** The code is already `cfg`-shaped for it
   and Tauri/WebView2 is mature. This is packaging and path normalization, not
   a port. Gating work: add a Windows CI leg; give `open_external`
   (`main.rs:2867`) a non-`explorer` implementation; suppress the console
   window on the `gh` shell-out; and **strip the `\\?\` UNC prefix from
   `canonicalize` output before any path reaches storage** — otherwise stored
   paths differ by platform and the M10 re-ingest determinism invariant breaks
   cross-platform. That last item is a correctness bug, not a packaging chore.
2. **Tier 2 — Android and iOS/iPadOS as a *reader*, not the full tool.**
   Ingest, clone, plugin authoring and local-LLM tiers stay desktop-only. The
   mobile build opens an already-ingested graph and browses it: flows,
   findings, evidence, spec bundle. This is the only scope that survives the
   sandbox, the JIT ban, 2.5.2, and the absence of Ollama
   (`crates/llm/src/lib.rs:359` hard-codes loopback and *rejects* non-loopback
   HTTP by design — pointing a phone at a desktop Ollama would require
   breaking a deliberate security invariant, which this ADR declines to do).
   iPadOS is the target that actually earns the work; iPhone follows for free.
3. **Tier 3 — tvOS and visionOS are non-goals.** No Tauri support, no
   developer-tool precedent, and tvOS's focus-engine remote cannot drive a
   dense graph UI. If visionOS ever matters, ship the iPad build in
   compatibility mode for zero porting work. Revisit only if Tauri lands
   visionOS support *and* a concrete user need appears.
4. **Prerequisite for Tier 2: make the plugin host interpreter-only.**
   Standardize on a non-JIT WASM execution path (wasmtime's Pulley
   interpreter, or wasmi) so one code path runs everywhere, and **bundle all
   adapters in the app on mobile** — no runtime download. ADR-0017 is not
   superseded; its runtime-loading and AI-authoring lifecycle becomes
   explicitly desktop-only, which should be recorded there.
5. **Prerequisite for Tier 2: a capability-based file access seam.** Replace
   direct `Path` use at the ingest and evidence boundaries with a trait that a
   desktop impl satisfies with `std::fs` and a mobile impl satisfies with
   picker-derived scoped URLs. Hold one directory-level scope, not one per
   file — iOS caps concurrent security-scoped URLs and requires file
   coordination for out-of-sandbox reads.
6. **Sequencing.** Windows first (independent, cheap, fixes a real
   determinism bug). Then the interpreter switch and the file-access seam,
   both of which are desktop-testable. Then Android (permissive, proves the
   NDK cross-compile). iOS last, since it is strictly the most constrained.

Before any of it, `open_external` needs a fallback `cfg` arm: it defines
`launcher` only for macos/linux/windows, so adding a mobile target is an
immediate compile error.

## Consequences

- Windows becomes a supported, CI-verified target, and a latent cross-platform
  determinism bug is closed as a side effect.
- Mobile ships a genuinely smaller product. "Cartograph on iPad" will not
  ingest a repo, and marketing/docs must say so plainly; this is a real
  reduction in scope from what was asked for.
- The interpreter switch costs desktop plugin throughput (a large constant
  factor on an extraction-heavy workload) in exchange for one execution path.
  If that proves unacceptable, the fallback is JIT-on-desktop /
  interpreter-on-mobile — two paths, and a determinism obligation to prove
  they produce identical fact-set hashes.
- Two Apple targets and the whole tvOS/visionOS request are declined. That is
  the substantive disagreement with the ask and the main thing to push back on
  if the reasoning above is wrong.
- Mobile signing, provisioning and store review become new release-pipeline
  surface; ADR-0016's fail-closed posture needs an iOS/Android analogue.

## Alternatives (≤3)

- **Port everything, all six platforms, full parity** — what was asked for.
  Rejected: tvOS/visionOS have no webview layer to build on at any effort
  level, and full-parity iOS requires either shipping a JIT (impossible) or
  abandoning runtime plugins (already the Tier 2 position) *and* solving repo
  ingest inside a sandbox with no `gh` and no local LLM.
- **Mobile as a thin client to a desktop/server instance** — sidesteps the
  sandbox and the LLM problem entirely, and is how most dev tools do this.
  Rejected for v1 by NG5 (no multi-user backend) and by the local-first
  egress posture; worth reopening as a post-v1 ADR since it dominates the
  reader design on every axis except architectural commitment.
- **Windows only, defer all mobile** — lowest cost, highest certainty.
  Rejected as under-reaching: the file-access seam and the interpreter switch
  are independently valuable and testable on desktop, so mobile can be
  de-risked without committing to ship it.
</content>
</invoke>
