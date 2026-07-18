# ADR-0018 — multi-platform port scope: Windows now, mobile not pursued

- **Status:** Accepted
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

A fourth fact emerged while scoping, and it decided the outcome: **a mobile
build has no legal way to receive a graph.** Ingest cannot run on-device (see
above), so the graph must arrive from a desktop — and every transport is
blocked or weak. A LAN server is NG5. Cloud sync sends source-derived data off
the machine, against the local-first egress posture. That leaves manual file
transfer of an exported bundle: legal, but a stale snapshot re-exported by hand
on every code change. The reader concept survives its technical constraints and
then fails on product value.

## Decision

**Windows is the only platform target pursued. Mobile is not.**

1. **Windows, to full parity.** The code is already `cfg`-shaped for it
   and Tauri/WebView2 is mature. This is packaging and path normalization, not
   a port. Gating work: add a Windows CI leg; give `open_external`
   (`main.rs:2867`) a non-`explorer` implementation; suppress the console
   window on the `gh` shell-out; and **strip the `\\?\` UNC prefix from
   `canonicalize` output before any path reaches storage** — otherwise stored
   paths differ by platform and the M10 re-ingest determinism invariant breaks
   cross-platform. That last item is a correctness bug, not a packaging chore.
2. **iOS, iPadOS and Android are not pursued.** Not "later" — no work is
   scheduled and no seams are built speculatively. Full parity is impossible
   (sandboxed ingest, no `gh`, no Ollama, no JIT, 2.5.2); the reduced reader
   scope is possible but has no viable graph transport, so it would ship a
   stale hand-synced snapshot. Cartograph is a desktop analysis tool and this
   ADR stops treating that as a limitation to design around.
3. **tvOS and visionOS are non-goals.** No Tauri support, no developer-tool
   precedent, and tvOS's focus-engine remote cannot drive a dense graph UI.
   If visionOS ever matters, ship the iPad build in compatibility mode.
4. **What would reopen this.** A concrete user need for mobile access, plus a
   decision to revisit NG5 — because the honest answer to "Cartograph on my
   iPad" is a thin client to a desktop or hosted instance, not an on-device
   port. That is a product-architecture decision, not a porting one, and it
   gets its own ADR if it ever comes up. The mobile findings above are
   recorded so that ADR starts from evidence rather than re-running the spike.

The mobile-driven prerequisites this spike identified — an interpreter-only
WASM host and a capability-based file-access seam — are **not** adopted. Both
were justified by mobile alone; building them now would be speculative
generality against a target we are declining.

## Consequences

- Windows becomes a supported, CI-verified target, and a latent cross-platform
  determinism bug is closed as a side effect. Both are worth doing on their
  own merits; neither depended on the mobile question.
- The plugin host keeps its JIT and ADR-0017 stands unmodified. `Path`-based
  ingest and evidence APIs stay as they are.
- Five of the six requested platforms are declined. If mobile later turns out
  to matter, the reader design is *not* the head start — the transport
  problem above means the work restarts from the thin-client question.
- ADR-0016's macOS signing posture needs a Windows analogue (code signing,
  installer), which is new release-pipeline surface.
- ADR-0001's "macOS primary, Windows" finally becomes true rather than
  aspirational.

## Alternatives (≤3)

- **Port everything, all six platforms, full parity** — what was originally
  asked for. Rejected: tvOS/visionOS have no webview layer to build on at any
  effort level, and full-parity iOS requires shipping a JIT (impossible) *and*
  solving repo ingest inside a sandbox with no `gh` and no local LLM.
- **Mobile as a read-only graph browser** — technically survives the sandbox,
  the JIT ban and 2.5.2. Rejected on product value, not feasibility: with no
  legal transport it degrades to hand-synced stale snapshots.
- **Mobile as a thin client to a desktop/hosted instance** — sidesteps the
  sandbox, the transport problem and the LLM problem at once, and is how most
  dev tools solve this. Rejected for v1 by NG5, but it is the *right* design
  if mobile is ever revisited; recorded here so it is the starting point.
</content>
</invoke>
