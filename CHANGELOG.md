# cartograph

## 0.9.0

### Minor Changes

- b48ae60: Conformance gate for WASM adapter plugins (#200): a durable `plugin-gate` job
  proves the SPI contract under the standard bounds, the plugin's own golden
  corpus (expected facts pinned with the host identity), and double-run
  determinism. The verdict persists per (plugin id, content hash) — replaced
  bytes are ungated again — and Settings shows a per-artifact gate chip with the
  failing check named, plus a run-gate action. A failed or ungated plugin stays
  proposed and never joins extraction.
- b29c16c: The unsupported lane resolves (#201): Preflight's uncovered-language findings
  carry a request-adapter action; a plugin that lands in a discovery directory,
  passes the conformance gate, and is enabled for the project counts as
  coverage on the next scan — the originating finding closes — and extraction
  routes the files its golden corpus claims through the plugin with host-pinned
  facts. Compiled-in adapters always win a contested extension, routing is
  deterministic and all-or-nothing per plugin, and bytes swapped after gating
  fail the ingest closed. Manifest-ingested repos now register as discovery
  roots too.

## 0.8.0

### Minor Changes

- ced9d60: Adapter plugins are now discoverable and switchable per project: Cartograph scans the project's .cartograph/adapters/ and a user-level adapters directory (the project copy wins on id conflict, shadowing stated), keys every artifact by its content hash, and lists them in Settings with a per-project enable/disable that fails closed — a plugin is off until you turn it on, and it only ever extracts facts behind the conformance gate.
- 3966468: Plugin facts are pinned to the exact artifact that produced them: the host stamps every fact's provenance with `{plugin-id}@{hash}` and the full BLAKE3 artifact hash, overwriting anything the guest wrote — a plugin can never impersonate a built-in extractor, a rebuilt artifact is a different extractor identity, and repeat runs of the same artifact are provably identical.

## 0.7.0

### Minor Changes

- 68190e2: Settings now answers "what can Cartograph read?": an Adapters section lists every installed adapter with what it extracts and which files it claims — the same registry Preflight consults, so the two can never disagree — explains in plain language what an adapter is, and lists known adapter types not yet installed (JavaScript, C, C++, Kotlin, Swift, Objective-C) with a request link. Preflight findings for those languages now name the missing adapter instead of dead-ending.
- 619c113: Atlas gains a keyboard-first focus mode: select a node and press Enter (or use the breadcrumb control) to re-root the view on that node and its direct connections, with the layout reflowing deterministically. Each focus stacks a navigable level; Esc or the breadcrumb backs out one level at a time to the full graph, and every transition is announced for screen readers.
- bbc41fc: Gap triage now works at the class level end to end: a whole cause class escalates locally in one durable, cancellable job — one staged proposal per instance, failures recorded without aborting — with per-instance accept/reject through the standard decision records (nothing joins the graph unaccepted). Classes that truncate traced flows rank first, in the cause lane and the by-tier tab alike, so the top of the register is the highest-value hour of triage. Cloud escalation stays per-instance: a consent grant binds to one exact payload.
- b59f5e3: Help now lives in the app: a native Help menu (Cartograph Help, the wiki user guide and issue reporting in the system browser, About with the version), an in-app Help view with a topic for every surface plus a concepts page — rendered offline from bundled markdown, reachable from the menu, the command palette, the header's Help action on every surface, or ? / F1 — and single-sourced content: docs/help/ mirrors to the wiki with a CI gate that fails when they drift.
- 13eba95: The app now answers its own jargon in place: small keyboard-accessible "?" affordances explain System gaps, unsupported patterns, no-evidence questions, confidence tiers, verified-only vs best-effort, and recovery authority right where those terms appear — one interaction to open, Esc to dismiss, with a Learn-more link. All notes come from one shared source, so no surface can drift from another.

## 0.6.0

### Minor Changes

- a748749: Multi-ingest stacking is now stated, not silent: the Workspace lists every repo currently merged into the system (from the graph's own facts) and says that new ingests merge in. The destructive action speaks your language — "Clear system" — and its confirmation names exactly what is removed (every recovered fact, listed by repo) and what survives (job history and settings).

## 0.5.0

### Minor Changes

- de05df5: Flow Inspector now traces real targets beyond web apps: extension contexts (popup, background, content scripts) and manifest keyboard commands anchor their own flows, walking from the manifest entry into the entry file's symbols and across message channels. When zero flows trace, the Inspector names every anchor kind recovery looked for and what it found instead of a generic hint.
- 3c09551: The gap register triages by cause at scale: past a screenful, the System-gaps lane groups gaps into cause classes (stop reason × extractor) ranked by instance count — thousands of gaps read as a handful of causes. Each class expands on demand to paged instance rows that stay responsive, and every instance keeps its Resolution Strategy path.

## 0.4.0

### Minor Changes

- 7b127a2: Atlas is readable at real-repo scale: nodes now lay out deterministically in labeled architecture bands (Infrastructure → Cloud → Server → Events → Client) instead of an undifferentiated force grid, clustered within each band by repo/module. Past 200 nodes the initial view renders collapsed clusters with aggregated relation counts — tap a cluster (or its index chip) to expand it in place, with Expand-all/Collapse controls — so a 14k-node graph opens as a handful of labeled boxes, and identical snapshots always produce the identical layout.

## 0.3.0

### Minor Changes

- 49aebb6: Java language adapter (T0): classes, interfaces, enums, records, and methods become Confirmed Symbols with exact evidence spans; same-class and import-proven cross-file calls join the graph deterministically (declared-package misses fail closed to explicit Gaps); annotation-proven Spring Web mappings become Endpoints with class+method path composition. Preflight now detects Java as covered (and Kotlin as its own uncovered language), and the ingest summary reports a Java layer row.
- 638126d: Jobs surface hygiene: the dev-only "Enqueue test job" control is gone from the production UI (with its `enqueue_job` command), and a confirm-gated **Clear finished** action removes done/failed/cancelled jobs from the durable spine while queued, running, and interrupted (resumable) work is always kept.

### Patch Changes

- 5d45f56: Recovery no longer freezes the app: ingest, add-repo, add-system, and ingest retry/resume commands now run their extraction on a blocking worker thread instead of the webview/main thread, so a large repository recovers with the UI fully interactive (no more macOS beachball).

## 0.2.0

### Minor Changes

- ca35dc5: Anthropic Claude API provider: the first cloud reasoning lane behind
  the fail-closed egress firewall, with three pinned model lanes (Haiku
  for triage, Opus as the default T3 reasoning lane, Fable opt-in for the
  hardest escalations with server-side refusal fallback to Opus).
  Embeddings remain local-only; consent disclosures carry provider,
  model, endpoint, pricing, and the Fable retention requirement; safety
  refusals are a typed outcome.
- 327b4de: Atlas canvas v2: node shape now encodes kind independently of color —
  octagon with a dashed red border for Gaps (no longer colliding with
  the gateway diamond), diamonds for gateways/channels, rounded
  rectangles for the rest. Every edge carries a clickable mono
  tier+relation chip (T0 HANDLES, GAP …) on the canvas and in a
  visible-relations index, opening the evidence drawer for the edge
  exactly like a node; the layer filter drives the header scope chip
  (Atlas · <layer>); and a single click keeps selecting nodes and edges
  while the drawer is open.
- 23da0fb: Chrome runtime messaging channels (US-0016, #99): `chrome.runtime.sendMessage`
  / `chrome.tabs.sendMessage` producer sites and explicit handler registrations
  (`[MessageType.X]: handler` dispatch tables behind a real
  `onMessage.addListener`) stitch into deterministic `chrome-message` channels
  with PUBLISHES/SUBSCRIBES edges connecting extension contexts. Message
  identities resolve through string literals, repo-wide const-string maps, and
  one-hop creator functions; anything runtime-computed stays an explicit Gap,
  and shadowed `chrome` bindings or conflicting const maps fail closed.
- cb0d4a6: Gap-escalation orchestration: gap_strategies derives strategy cards
  from real provenance (attempted tiers, stop reason, required evidence
  citations, a closed candidate set from the k-hop context) with exact
  redacted-payload egress estimates; escalation_preview returns the
  exact payload a cloud run would send for the one-action consent
  dialog; run_escalation executes as a durable job — local SLM
  immediately, cloud only with standing consent plus a per-payload
  grant whose hash matches the preview — records egress bytes, and
  returns a staged proposal that never joins the graph.
- 35d1968: Evidence drawer v2: resizable (320-560px) with a click-through overlay
  wrapper, Esc to close, true file line numbers (windowed reads now
  report their real starting line), the full span range including end
  line:col, why-this-is-a-Gap/Inferred strips with a collapsible
  why-this-tier explanation, a provenance table with the complete
  copyable BLAKE3 content hash, navigable supporting-evidence spans, a
  Resolution Strategy CTA on Gaps, and the R-INT-1 integrity footer.
- ca0c09c: Flow Inspector surface (#107): header with flow id, named gap count, trigger
  summary and score; verified-only/best-effort projection with an explicit
  hidden-hop count, annotated excluded cards, and the Gap node always retained
  (R-INT-4); wrapping hop-card layout with Fit/zoom that never scrolls
  horizontally; Gap hops open the Resolution Strategy modal and every other hop
  opens the evidence drawer from its own provenance.
- 1a5db62: Gaps & Drift register surface: the product's core honesty claim gets
  its own surface. Three lanes that never conflate — System Gaps (with
  next-escalation-tier tails and a resolution seam), unsupported
  patterns (tool limitations, explicitly not gaps), and no-evidence
  findings — plus a by-escalation-tier grouping and a Drift tab, all
  wired from the spec compiler's own registers and reconciled with the
  Workspace tally by construction.
- d3dd51c: Knowledge-graph context assembly for the escalation tiers: deterministic
  k-hop subgraph extraction around any focus, citation-bound serialization
  (every statement carries the graph identity it came from and unknown
  provenance is never presented as confirmed), hybrid structural+similarity
  candidate merging, and budgeted packing that records what was dropped —
  never silent truncation.
- 9365e7a: Adopt the production UI design tokens: dark IDE palette from the design
  handoff (app/rail/surface tiers, confidence-tier and register-finding
  colors, radius/shadow/motion scales) and locally bundled Inter, JetBrains
  Mono, and Material Symbols fonts — no runtime font fetch.
- 5f55359: IndexedDB data model (US-0016, #99): explicit `createObjectStore`
  declarations and repository operations (`tx.objectStore(X).put(…)`, bound
  store handles, `.index(…)` chains) become cited `DataEntity` nodes with
  symbol-attributed READS/WRITES relations. Store identities resolve through
  string literals or repo-wide const-string maps; runtime-computed identities
  stay explicit Gaps.
- 1fdbe1d: Connect → Preflight → Recover ingest flow: the bare path-input card is
  replaced by the handoff's three-step flow. Connect picks a GitHub repo,
  local folder, or system manifest behind a local-only reassurance strip;
  Preflight runs on-device detection (0 bytes egress) and renders the
  three-way classification — potential system gaps and unsupported
  patterns as separate cards that never conflate; Recover streams real
  core job progress with a Run-in-background hand-off to the Jobs
  surface.
- 7c6e910: Job spine v2: jobs carry stage, percent progress, failure detail, and
  artifact links; new cancel/retry commands cover the full lifecycle
  (cancel is cooperative at stage boundaries, retry also resumes jobs
  interrupted by an app restart); ingest runs as a staged job emitting
  live `job://changed` events. Existing state-spine databases migrate in
  place.
- 2b10773: Jobs surface: durable job management in the shell — every lifecycle
  state with live progress and stage, cancel/retry/resume actions,
  failure detail, artifact links, and job://changed event subscription;
  degrades gracefully against a pre-v2 core.
- 9cbf4ed: Managed local SLM tier: a versioned model catalog pins every
  LLM-touching action (embedding, triage, proposal) to a local model so
  provenance attributes work to an exact mapping; Ollama health probing
  reports missing models and unreachable endpoints as explicit,
  remediable states — never a silent failure and never a cloud fallback;
  and local completions validate against the caller's schema with a
  bounded retry.
- 454938c: Preflight detection and the register finding model: a local-only
  `preflight` command detects languages, frameworks, and adapter coverage
  and classifies constructs into the three-way split — potential system
  gaps vs unsupported patterns — before recovery runs; unsupported and
  no-evidence findings persist on the state spine distinctly from Gaps,
  and a `findings_summary` command serves every surface the same
  gap/unsupported/no-evidence/drift tallies from one definition.
- 0e13866: Provenance & Eval surface: tier distribution with a fully described
  stacked bar (register findings kept distinct from graph facts),
  per-extractor coverage bars (not-in-scope reads n/a, never 0%),
  paired-eval quality-gate cards with pass/below-floor states, and
  evidence health over re-ingests — the determinism footer claims
  verification only when the history really contains same-commit
  records with equal content hashes. Every chart is screen-reader
  complete; nothing encodes by color alone.
- 21952be: Recovery metrics: every completed ingest now persists a history record
  — tier tallies counted with the register's own provenance definition,
  unsupported/no-evidence counts, per-extractor coverage, and an
  order-independent whole-graph content hash — so re-ingesting the same
  commit shows identical hashes in queryable history and the determinism
  invariant becomes observable data. New ingest_history and
  extractor_coverage commands feed the Provenance & Eval surface.
- 493acf6: Resolution Strategy modal: one component for every gap — escalation
  ladder with the R-INT-1 statement, why deterministic recovery stopped,
  required-evidence citations, and runnable strategy cards with egress,
  cost, latency, privacy, and export impact. Local runs immediately;
  cloud opens the exact-payload one-action consent dialog first and only
  runs with the approved hash. Results are staged proposals with
  Accept/Reject through the durable decision records — they never
  auto-join the spec. Gap rows in the register and the evidence drawer's
  CTA now open the modal for real.
- ba1a5cc: Persistent tier settings and standing revocable cloud consent: per-tier
  enable/provider configuration on the state spine (T0 is not
  configurable — always on, never an LLM), consent that only permits
  cloud on top of the firewall's per-payload grant, immediate
  revocation (including when switching a tier back to local), an
  observed-egress log, and a live egress summary for the status bar.
- e32650e: Settings surface: recovery-tier cards with T0 locked always-on, per-tier
  enable toggles persisted in the core, Local/Cloud provider selection for
  T2/T3, and the fail-closed cloud consent flow — the full disclosure
  (provider, model, endpoint, pricing, caveats) renders before consent is
  recordable, consent is revocable with immediate effect, and the status
  bar's egress line now derives live from settings state.
- bc3d2b9: IDE app shell: left nav rail with eight surfaces, breadcrumb header with
  scope chip and Legend popover, status bar with live state and egress
  summary, thin global progress bar, ⌘K command palette with ⌘1–⌘8
  shortcuts, and a route-level error boundary. Existing dashboards moved
  under their surfaces (Workspace, Atlas, Flows, Spec Workbench, Jobs);
  Gaps & Drift, Provenance & Eval, and Settings show honest interim states.
- fa58d19: Spec Workbench polish (#108): Confirmed (T0/T1) assertion blocks now show
  their Accept/Reject/Annotate controls visibly locked — disabled plus
  aria-disabled, with an inline "Confirmed T0 — locked, read-only" explanation
  and a document-level R-INT-1 note — instead of hiding them; doc-list entries
  carry per-document count chips (US/AC/flows/ADR, alert tone for non-empty
  registers). T2/T3 curation and evidence preservation are unchanged.
- 703f531: WebExtension manifest adapter (US-0016, #99): Manifest V2/V3 `manifest.json`
  files become deterministic T0 topology — Extension, ExtensionContext
  (service worker / background scripts, content scripts, pages, toolbar
  action), and Command nodes with DECLARES/ENTRY edges — plus exact-scope
  GRANTS security facts that surface wildcard host permissions in the security
  projection. Declared entry files bind to their `.ts` sources when the built
  `.js` is absent; a missing entry is an explicit Gap. The ingest summary and
  extractor coverage report the WebExtension layer separately.
- 282cee4: Workspace landing: the post-recovery report replaces the bare card
  grid. An outcome card states the overall tier and the honest findings
  tally (gaps, unsupported patterns, and no-evidence listed explicitly,
  never guessed) with CTAs into Gaps & Drift and Provenance & Eval; five
  provenance-health cards summarize the tier distribution from the same
  register summary every surface shares; and the artifacts grid carries
  independent generation and recovery-authority badges, with the gap
  register showing a single open-findings badge.

## 0.1.0

### Minor Changes

- 01082bc: Ship signed, notarized universal macOS app and DMG assets through versioned GitHub Releases.
