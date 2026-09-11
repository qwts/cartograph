# Manual test procedures

Milestone-boundary verification a human performs by using the app — the half
of an exit gate that automation cannot see (window chrome, feel, end-to-end
reality). Each procedure has a stable id referenced from
[`test-map.md`](test-map.md); CI verifies the reference, a human performs the
steps. Record results in the closing comment of the milestone's task issue.

Convention: run the relevant procedures at each milestone boundary, not per
PR — per-PR verification is CI's job.

---

## MT-M0-01 — Shell boots, job spine survives restart

1. `npm run tauri dev` from the repo root.
2. Window opens; dark theme; badge reads **core vX.Y.Z** (green).
3. Click **Enqueue test job** → a `noop / queued` row appears.
4. Quit the app fully; relaunch.
5. **Pass:** the job row is still listed (durable spine, M0 exit gate).

## MT-M1-01 — Ingest a TS repo, walk endpoint → evidence

1. `npm run tauri dev`; paste the path of a TypeScript/Express repo into
   **Ingest** and submit.
2. Job runs to `done`; graph stats become non-zero; **Endpoints** lists
   recovered routes, each with a **Confirmed** tier badge.
3. Click an endpoint.
4. **Pass:** the evidence panel shows tier/extractor/`repo:path bytes@commit`
   and the read-only source with the registration call highlighted — the
   highlighted text is the actual registration in the actual file
   (M1 exit gate: evidence jump-to-source).

## MT-M2-01 — Ingest Terraform, export the topology map

1. `npm run tauri dev`; paste the path of a repo containing `.tf` files into
   **Ingest** and submit.
2. Graph stats grow; the **Topology map** card shows Mermaid text with
   `Resource` nodes and solid `TRIGGERS`/`ROUTES`/`SUBSCRIBES`/`GRANTS` edges
   where the Capability Registry matched (dotted for the reference DAG).
3. Click **Copy Mermaid**, paste into a Mermaid renderer (e.g.
   mermaid.live).
4. **Pass:** the rendered diagram matches the repo's infrastructure; anything
   the extractor could not resolve appears as a visibly distinct `?` node,
   never silently dropped (M2 exit gate: topology map artifact).

## MT-M3-01 — Trace flows, export the dossier

1. `npm run tauri dev`; ingest a TypeScript repo with Express endpoints and
   event SDK usage (emitter/Kafka/SQS — any repo exercising US-0004).
2. The **Flows** card lists each traced flow with a status and score; any
   runtime-computed channel appears as a `GAP: …` hop with a reason, and
   its branch stops there — never silently completed (R-INT-4).
3. Click **Copy dossier**, paste into a Markdown+Mermaid renderer.
4. **Pass:** each flow renders a sequence diagram (Gap arrows broken `--x`)
   and a provenance table with tier + confidence + evidence span on every
   hop (M3 exit gate: flow dossier export).

## MT-M4-01 — Screen-anchored flows

1. `npm run tauri dev`; ingest a repo with a React client (React Router or
   Next.js `pages/`) fetching its own backend's endpoints.
2. The **Flows** card anchors flows at screens (`Screen /route`), not at
   the endpoints those screens fetch; endpoints nothing fetches keep their
   own flows.
3. Copy the dossier and render it.
4. **Pass:** a screen flow runs `RENDERS → FETCHES → HANDLES → …` end to
   end with tier + confidence per hop; an unresolvable fetch URL appears
   as a `GAP: …` hop truncating that branch (M4 exit gate: flows anchored
   at Screen).

## MT-M5-01 — Cross-repo flow via the system manifest

1. Write a `cartograph.system.toml` declaring 2+ repos that share a channel
   (a queue URL / topic in both, or via the manifest `[env]` block); paste
   its path into **Ingest** (fresh graph — #50 clears stale schemes
   automatically).
2. The summary lists every repo as `identity@sha12`.
3. The **Flows** card shows one flow whose trigger lives in one repo and
   whose consumer hops land in another — inspect the dossier: the
   `SUBSCRIBES` hop's target carries the other repo's identity.
4. **Pass:** the cross-repo flow is Verified when both sides resolve
   (literal or manifest identity); an unresolved side appears as a `GAP: …`
   hop, never a silent stitch (M5 exit gate: cross-repo flow via literal
   channel ids).

## MT-M6-01 — Observed state joins infra to code (T1)

1. In a repo with Terraform + publishing code, run
   `terraform show -json > state.json` (or `terraform show -json plan.out`
   for a plan); add `state_json = "state.json"` to that repo's `[[repos]]`
   entry in `cartograph.system.toml` and ingest the manifest.
2. The **Topology map** shows the backed channel as a cylinder with a
   `BACKS` arrow from its resource — infra and the event layer are one
   picture.
3. Inspect an enriched resource's evidence: T0 `prov` (Deterministic)
   remains, and `observed_prov` (Dynamic, Confirmed) points into the state
   file; any placeholder the state confirms has lost its `?`.
4. **Pass:** BACKS appears only for channels code actually publishes or
   subscribes; values `terraform show` marks sensitive read `[redacted]`
   everywhere in the UI (M6: observed-fact provenance; AC-0009).

## MT-M6-02 — OTLP trace fills a runtime channel Gap (T1)

1. In a repo whose event SDK call computes its queue/topic identity at
   runtime, capture an OTLP trace with `messaging.system`,
   `messaging.destination.name`, and `code.file.path` span attributes.
   Export it with the collector file exporter as OTLP/JSON Lines.
2. Add `otel_jsonl = ["trace.jsonl"]` to that repo's `[[repos]]` entry in
   `cartograph.system.toml` and ingest the manifest.
3. Inspect the previously unresolved PUBLISHES/SUBSCRIBES hop: its Gap is
   replaced by a Channel whose edge resolver is `t1.otel-trace`; provenance
   is Dynamic/Confirmed and points to the observed span id in `trace.jsonl`.
4. Include `http.request.method` plus `http.route` on an HTTP server span.
   The matching Endpoint keeps its Deterministic `prov` and gains separate
   Dynamic `observed`/`observed_prov` facts.
5. **Pass:** a uniquely source-matched identity resolves the Gap; ambiguous
   same-kind observations leave the Gap explicit with T0 and T1 recorded in
   `attempted_tiers` (AC-0012, R-INT-1, R-INT-4, M6 exit gate).

## MT-M6-03 — Pulumi program and observed deployment form a T0→T1 ladder

1. In a TypeScript Pulumi AWS repo, run `pulumi stack export --file stack.json`
   without `--show-secrets`; add `pulumi_json = "stack.json"` to that repo's
   `[[repos]]` entry in `cartograph.system.toml`, then ingest the manifest.
2. Inspect an import-proven resource created with `new aws.*`: its T0 `prov`
   is Deterministic and points to the constructor, while REFERENCES,
   `dependsOn`, `parent`, and Capability Registry edges remain T0.
3. Inspect the same resource's `observed` and `observed_prov`: the observed
   URN/inputs/outputs come from the stack artifact with Dynamic/Confirmed
   evidence, without replacing the T0 fact.
4. Repeat with `pulumi preview --json` output. Include an encrypted Pulumi
   secret wrapper in the fixture or stack and verify Cartograph displays
   `[redacted]`, never ciphertext or plaintext.
5. **Pass:** only observations matching an existing T0 Pulumi type + logical
   name enrich the graph; an unmatched exported resource does not create a
   new T0 Resource (AC-0051, AC-0052, R-INT-1).

## MT-M7-01 — Local semantic resolution clears its precision gate (T2)

1. Start Ollama locally and make the configured embedding model available:
   `ollama pull nomic-embed-text` (Cartograph never downloads it implicitly).
2. Run
   `cargo test -p semantic real_ollama_resolves_eval_gated_gap -- --ignored --nocapture`.
   Also run
   `cargo test -p app semantic_preview_uses_real_ingested_resource_and_call_gaps`;
   this fixture must recover its inputs through the production TypeScript,
   event, and Terraform extractors rather than constructing graph nodes by hand.
3. Inspect the printed report: provider is local Ollama, paired-eval precision
   meets the configured floor, ANN lookup is below 100ms, and one explicit
   channel Gap is replaced only in the returned best-effort overlay.
4. Stop Ollama and repeat the semantic preview from the app/API.
5. **Pass:** the stopped provider fails explicitly with no graph change or
   network fallback; the passing preview edge is Semantic/InferredStrong with
   evidence from both Gap and target, while the stored confirmed graph retains
   its original Gap. The real-ingest fixture fills both the IaC-backed channel
   Gap and unresolved relative-import call Gap without adding gaps for globals
   or package calls (AC-0021, AC-0022, R-INT-1, M7 exit gate).

## MT-M8-01 — Bounded T3, exact egress consent, and durable curation

1. Run `cargo test -p llm -p agents`. Confirm the local-only cloud test
   reports zero provider calls, and the bounded broker rejects Confirmed slots,
   invented targets, and missing both-side citations before staging anything.
2. Run `npm --prefix ui run storybook` and open
   **Privacy / EgressConsentDialog / ExactSpanPayload**.
3. Compare every displayed field with the story's firewall preview fixture:
   provider, tier, one-action id, system instructions, prompt, both repo/path/
   byte/commit spans, redacted span text, redaction count, and payload hash.
   Resize below 600 px and confirm no payload text is clipped or hidden.
4. Click **Allow this action once**. **Pass:** the interaction fires only the
   consent callback with that complete preview; no unredacted secret appears.
5. Run
   `cargo test -p agents accepted_and_rejected_decisions_persist_and_reapply_by_basis`.
   **Pass:** the final accept/reject state survives SQLite reopen and reappears
   for the unchanged task basis, while changed evidence has no inherited
   decision.
6. With Ollama and `qwen3:8b` already installed locally, run
   `cargo test -p agents real_ollama_returns_bounded_cited_agent_proposal -- --ignored --nocapture`.
   Stop Ollama and repeat. **Pass:** local failure
   is explicit and no cloud provider is selected; Cartograph never pulls a
   model automatically (AC-0020, AC-0023..0025, R-INT-1, R-INT-3, M8 exit gate).

## MT-M9-01 — Atlas filters, confidence integrity, and 10k-node interaction

1. Run `npm --prefix ui run storybook` and open
   **Atlas / AtlasCanvas / TenThousandNodeScale**. Pan and zoom the 10,000-node
   Cytoscape canvas, then switch through Infrastructure, Cloud, Server, Events,
   and Client.
2. **Pass:** controls remain responsive and each filter reports only its own
   node/edge projection; the app does not create 10,000 parallel DOM controls
   (the accessible entity index stays bounded).
3. Open **Atlas / AtlasCanvas / ConfidenceOverlay** and compare the legend to
   the canvas. **Pass:** Confirmed is green, InferredStrong blue,
   InferredWeak yellow, and Gap red with a dashed diamond; disabling the
   overlay removes tier color without relabeling facts.
4. Open **Shell / App / AtlasNodeToEvidence**, select the endpoint from the
   Atlas entity index, and inspect the evidence drawer.
5. **Pass:** file, byte span, commit, extractor, and tier are visible; the
   matching source span is highlighted in a read-only view (AC-0026..0028,
   R-INT-2, NG1).

## MT-M9-02 — Flow Inspector sequence, explicit Gap, and export projection

1. Run `npm --prefix ui run storybook` and open
   **Atlas / FlowInspector / SequenceAndTriggerSelection**. Select each trigger,
   then pan, zoom, and fit the React Flow viewport.
2. **Pass:** the visual and accessible sequences follow the traced hop order;
   every hop carries a distinct tier/confidence badge, and the selected source
   flow's status and score remain visible (AC-0029, R-INT-2).
3. Open **Atlas / FlowInspector / ExplicitGap**. **Pass:** the unresolved hop is
   a dashed red card that shows the graph-provided reason and attempted tier
   sequence; no downstream hop is invented after the Gap (AC-0030, R-INT-4).
4. Open **Atlas / FlowInspector / VerifiedOnlyProjection**, switch from
   `best-effort` to `verified-only`, and expand the Mermaid + provenance dossier.
5. **Pass:** the InferredWeak hop disappears from both the visible sequence and
   copyable dossier, while Confirmed and explicit Gap hops remain annotated
   (AC-0031, R-INT-5).

## MT-M9-03 — Spec Workbench provenance, curation, and full export

1. Run `npm --prefix ui run storybook` and open
   **Spec / SpecWorkbench / FullArtifactSetAndInlineProvenance**.
2. Select each of the nine artifact entries. **Pass:** user stories, US-TM,
   flow dossiers, resource topology, data model, ADRs, Gap register, and Drift
   register plus Security findings are always present; every recovered assertion shows its tier,
   confidence, extractor, content hash, and every evidence span inline
   (AC-0032, AC-0035, R-INT-2).
3. Open **Spec / SpecWorkbench / AcceptRejectAndAnnotate** and exercise all
   three curation controls on the inferred assertion. **Pass:** only inferred
   content exposes the controls, annotation requires a note, and the resulting
   decision appears in the durable curation log without changing the tier
   badge (AC-0033, R-INT-1).
4. Open **Spec / SpecWorkbench / VerifiedOnlyExport**, switch between both
   modes, and use **Export bundle**. **Pass:** `verified-only` excludes weak
   inference, `best-effort` clearly tags it, and both exported projections
   contain the Gap and Drift registers (AC-0034, R-INT-5).
5. In a connected desktop build, record a decision, re-ingest the unchanged
   source, and reopen the Workbench. **Pass:** the decision reappears for the
   same content hash; changing its source/evidence produces a new undecided
   assertion (AC-0033).

## MT-M9-04 — Found/recovered ADRs and mapped drift

1. In a multi-repo fixture system, add `docs/adr/ADR-0001.md` to a docs repo
   with `Status`, `Governs`, and `Forbids` fields. Make `Governs` cite an
   existing graph id from a service repo in backticks and create a code edge
   whose label is listed by `Forbids`.
2. Ingest the system and open the Workbench **Architecture decisions** artifact.
   **Pass:** the found ADR and DECIDES link are Confirmed with exact file/span
   evidence; unrelated or nonexistent ids are not linked (AC-0036).
   Remove the `Governs` declaration and re-ingest. **Pass:** its former DECIDES
   link is absent. Delete the ADR file and re-ingest. **Pass:** its found ADR
   node is absent (AC-0036).
3. Include a code producer and channel not governed by a found ADR. **Pass:**
   the artifact includes a distinct **Recovered / Inferred** ADR with graph
   evidence and curation controls; it is never displayed as Confirmed
   (AC-0037, R-INT-2).
4. Open **Drift register**. **Pass:** the found-ADR conflict names the ADR,
   offending edge, any containing flow trigger, and confidence inherited from
   the offending fact. Reject the supporting inferred edge and export again.
   **Pass:** neither its recovered ADR nor its drift finding remains
   (AC-0037, AC-0038).

## MT-M9-05 — Explicit endpoint auth and IAM security findings

1. Ingest a fixture with three endpoint facts: one explicitly
   `authenticated: false`, one explicitly protected, and one with no recovered
   auth state. Include IAM `GRANTS` with both least-privilege actions and a
   wildcard action or literal wildcard resource scope.
2. Open Workbench **Security findings**. **Pass:** only the explicit negative
   endpoint appears as unauthenticated; the protected and unknown-auth
   endpoints do not. The row cites its evidence and maps to US-0015/AC-0041.
3. **Pass:** the wildcard grant appears with its exact action and resource
   scope, confidence, evidence, and US-0015/AC-0042 mapping. The bounded grant
   does not appear.
4. If the wildcard `GRANTS` support is inferred, reject it and export again.
   **Pass:** its derived finding disappears; confirmed findings and facts are
   unchanged (R-INT-1, R-INT-5).

## MT-M10-01 — Deterministic delta re-ingest

1. Ingest a local fixture containing at least two TS/TSX files and two
   Terraform files. Record the returned delta counts and the Atlas snapshot.
   **Pass:** every source context is initially reported recomputed.
2. Re-ingest without changing any input. **Pass:** recomputed is zero, source
   contexts are reported reused, the ordered T0 fact identity/content-hash set
   is identical, and graph reconciliation reports no changed facts (AC-0039).
3. Change one TS/TSX file and one Terraform file, then re-ingest. **Pass:** only
   those byte-changed extraction contexts are reparsed; unchanged contexts are
   reused, while cross-file calls, module/policy joins, and stitched facts
   reflect the new full repository state (AC-0040).
4. Delete one changed source and re-ingest. **Pass:** its cache context and
   graph facts disappear; no stale node/edge remains (AC-0040).

## MT-M10-02 — Python server recovery and language summary

1. Ingest a Python repo containing both an import-proven FastAPI route and an
   import-proven Flask route, with one handler calling a function imported
   from another local Python module.
2. **Pass:** the ingest summary reports Python file/node/edge counts separately
   from TypeScript and Terraform; zero-count languages remain visible.
3. Inspect both endpoints and their handlers. **Pass:** methods, literal paths,
   HANDLES, local/imported CALLS, tier, extractor, file, exact byte span, and
   commit are present and Confirmed.
4. Add a lookalike object exposing `.get`/`.route` without a FastAPI/Flask
   import. **Pass:** it creates no Endpoint. Re-ingest unchanged, then change
   one Python file. **Pass:** unchanged Python contexts are reused and only the
   changed file is recomputed (AC-0053, ADR-0003, M10 language breadth).

## MT-M10-03 — Go server recovery and language summary

1. Ingest a Go module containing import-proven `net/http`, chi, and gin route
   registrations, with one handler calling a function in another local package.
2. **Pass:** the ingest summary reports Go file/node/edge counts separately
   from TypeScript, Python, and Terraform; zero-count languages remain visible.
3. Inspect the endpoints and their handlers. **Pass:** methods, literal paths,
   HANDLES, local/imported CALLS, tier, extractor, file, exact byte span, and
   commit are present and Confirmed. A route whose handler is a computed or
   external expression remains present with an explicit HANDLES Gap.
4. Add a lookalike router without a matching import and a computed route.
   **Pass:** neither creates an Endpoint. Mark the repo client-only and ingest;
   **pass:** no Go facts are produced. Re-ingest unchanged, then change one Go
   file. **Pass:** unchanged Go contexts are reused and only the changed file is
   recomputed. Add a `//go:build ignore` file and a GOOS-suffixed file without
   declaring a build target. **Pass:** neither contributes Confirmed facts
   (AC-0054, ADR-0003, M10 language breadth).

## MT-SB-01 — Stories render on-brand

1. `cd ui && npm run storybook`.
2. Walk Shell/* and Atlas/* stories.
3. **Pass:** components use the DESIGN.md dark tokens; the four TierBadge
   states are visually distinct (R-INT-2); `Shell/App` stories run their
   interactions without error.

## MT-DF-01 — Dogfood: recover Image Trail end to end

1. `npm run tauri dev`; **Connect** → repo `qwts/image-trail` (or a local
   clone path) and run Preflight → full recovery.
2. Workspace landing: the outcome tally and artifact grid are populated; the
   WebExtension layer row in the ingest summary reports ≥1 manifest.
3. Spec Workbench: `security.md` lists the over-broad optional host grants
   (`http://*/*`, `https://*/*`) with exact scopes; `data_model.md` names the
   IndexedDB stores (history, blobs, bookmarks, …); `gap_register.md` lists
   runtime-computed message identities as explicit Gaps with reasons.
4. Atlas: the Extension node, its contexts (service worker, action), and
   `chan:chrome-message:imageTrail.*` channels are present; a Gap octagon
   opens its Resolution Strategy.
5. Re-ingest the same commit; open **Provenance & Eval** → history.
6. **Pass:** the two ingest rows show identical whole-graph content hashes
   and the determinism footer reads verified (AC-0074, US-0016).

## MT-BB-01 — Large-repo recovery keeps the app interactive

1. `npm run tauri dev`; **Connect** → a local clone of a large real repo
   (thousands of source files — e.g. a production Next.js monorepo).
2. Preflight → **Run full recovery**. While the Recover stage line is
   visible, immediately: switch surfaces via `⌘1`…`⌘8`, open the command
   palette, and click **Run in background** → the Jobs surface.
3. Throughout the run: the pointer never becomes the macOS beachball, the
   stage label and progress advance, and every surface stays clickable.
4. From Jobs, **Cancel** the run; it stops at the next stage boundary.
5. **Pass:** no "application not responding" episode at any point during a
   multi-minute recovery (AC-0078, #158).

## MT-HELP-01 — Native Help menu (AC-0090, T-0090)

1. Launch the packaged app (`npm run tauri build` artifact or dev shell).
2. Open the **Help** menu in the native menu bar: it lists **Cartograph
   Help**, **User guide (wiki)**, **Report an issue**, and **About
   Cartograph**.
3. **Cartograph Help** switches the app to the in-app Help view.
4. **User guide (wiki)** and **Report an issue** open in the system
   browser — never inside the webview.
5. **About Cartograph** shows the app name and the version from
   `tauri.conf.json`.

## Planned context-hub acceptance

The following procedures bind T-0106–T-0118 for [SPEC-01](SPEC-01_context-hub.md).
**All H2–H7 acceptance gates remain PLANNED; all procedures below are
UNEXECUTED.** They are future milestone acceptance work, not H1 validation.
Defining a procedure, a passing traceability check, or a partial implementation
does not establish a pass. H1 retains its automated T-0101–T-0105 bindings.

Before running a procedure, record the Cartograph commit/build, platform, target
repository and immutable input identity, configuration, reviewer, and the actual
documented UI/API actions used. Its prerequisites must be implemented and
available; otherwise record **BLOCKED**, with the missing capability and issue.
Do not substitute mock-only results for an end-to-end or real-runtime requirement.
Use disposable local fixtures and synthetic canaries for negative cases. Preserve
redacted transcripts, screenshots, output hashes, and case-level expected/actual
results in the owning milestone task issue; record PASS or FAIL only after all
steps execute. An observed violation is FAIL even if other cases pass.

## MT-H2-01 — Pinned domain recovery and measured coverage (AC-0106, T-0106)

**Gate: H2 PLANNED. Procedure: UNEXECUTED.**

Prerequisites: domain/feature and rule recovery is available; the independent
expected-set review and decision-case matrix in the
[Vendure benchmark](evals/vendure-cart-readiness.md) are complete and frozen.

1. Prepare the benchmark's exact commit and declared default configuration;
   record their identities and the independently reviewed oracle checksum.
   Keep the oracle and corroborating upstream tests outside extraction input.
2. Recover the declared production-source scope twice with identical inputs.
   Open the cart-readiness feature and follow each rule's predicate, configuration
   condition, consequence, and dependency citations to the pinned source.
3. Score all eight expected rules and every frozen decision case using the
   benchmark procedure. Record entry guards and supporting stock rules separately,
   plus extra assertions, duplicates, inferred proposals, missing rules, and gaps.
   Inspect an unsupported dependency path and an unknown runtime input explicitly.
4. Compare rule identities and content hashes between the two runs. Have a
   reviewer explain each rejection and override from the hub and its citations.

**Pass:** all benchmark acceptance gates hold: 5/5 entry guards and 3/3 supporting
rules, zero unsupported confirmed assertions, complete required citations, every
frozen decision case correct, and identical-input determinism. Unsupported paths
and unknown inputs remain visible; inference does not count as confirmed coverage.
**Fail:** any gate misses, including a plausible rule name without its predicate
or evidence. Retain the scored manifest, case decisions, output hashes, coverage
report, and reviewer findings; partial recovery is a measured result, not H2 pass.

## MT-H2-02 — Implementation, intent, inference, and future design (AC-0107, T-0107)

**Gate: H2 PLANNED. Procedure: UNEXECUTED.**

Prerequisites: typed domain views and their provenance display are implemented.
Prepare a pinned fixture with a code guard `quantity > 5`, a design document
stating an intended limit of 10, a cited T3 interpretation, and a proposed future
limit of 20. Include a second feature with code but no supplied design document.

1. Ingest the fixture and open the feature's behavior and design views. Inspect
   each of the four records, its meaning, producing tier/confidence, and citation.
2. Follow the code and document citations. Verify the documented limit is shown
   as written intent despite its conflict with implementation. Review the T3
   interpretation and future design without applying a source-code change.
3. Open the second feature and ask for its design evidence and design gaps.
   Read the corresponding context output as well as the visible explanation.

**Pass:** the four meanings remain distinguishable; neither the document nor an
accepted interpretation proves implementation, and future design remains future.
The second feature reports unknown intent or missing evidence, without asserting
that no design exists. **Fail:** any conflation or tier upgrade. Retain the fixture
identities, four record/citation pairs, and screenshots/context responses for both
the conflicting-intent and missing-document cases.

## MT-H2-03 — Cited local-definition assessment (AC-0171, T-0171)

This source-only analyst procedure can use an independent review agent; record
the reviewer kind and audit scope. It does not execute the human desktop H2
procedures above or establish the complete domain-truth gate.

1. Freeze the [local-definition review plan](evals/vendure-cart-readiness.local-definitions.review-plan.md)
   before output, retaining the existing Vendure oracle, eight-rule denominator,
   41 decision cases, target commit and 17-file input manifest. Verify exact input
   membership and every file hash; keep the oracle, tests and output outside input.
2. At the recorded implementation commit, run the source-only
   `recover_source_rules` example twice into fresh directories. Compare graph,
   bundle, inventory and metadata bytes; record hashes, sizes and revisions in a
   separate local-definitions baseline. Do not install or execute target code.
3. Have an independent reviewer compare the emitted declarations, admitted uses,
   initializer structure and dependencies to original source. The checked-in
   `scripts/audit-local-definitions.cjs` uses the installed UI TypeScript 6.0.3
   parser/binder independently of the producer. Run
   `node scripts/audit-local-definitions.cjs SOURCE_ROOT INPUT_MANIFEST OUTPUT_DIR NEW_AUDIT_JSON`
   with the staged root, frozen manifest and each output directory; retain its
   result and script hash. It never executes target modules or resolves imports.
   Audit the tracking condition and mutated-array negative cases in the frozen
   plan separately. State any syntax or semantics not covered by those checks.
4. Retain separate score JSON and Markdown describing source-evidence gains,
   remaining gaps, unexpected claims and review limits. Preserve historical
   baseline/score files. A changed complete-rule score requires the entire frozen
   oracle's proof; initializer counts alone do not satisfy a rule or decision case.
5. Record captured-parser receipt integrity through its integration regressions
   separately; retrospective source matching by this example is not input-closure
   or production-capture proof. Record the assessment and artifacts on issue #398.

**Pass:** input and oracle remain unchanged, both runs agree, cited gains survive
the stated source audit and no unsupported value/business claim is counted.
**Fail:** changed/contaminated input, unequal output, incorrect source evidence or
inflated rule/case claims. Passing this bounded assessment leaves H2, full #385,
curated context and the market pilot open.

## MT-H3-01 — Shared curation and stale-evidence reconciliation (AC-0108, T-0108)

**Gate: H3 PLANNED. Procedure: UNEXECUTED.**

Prerequisites: durable proposals, shared curated projection, revision-bound source
evidence, and re-ingestion reconciliation are implemented. Prepare two cited T3
proposals against distinct source spans, plus an unaccepted control proposal.

1. Record the recovered graph and proposal identities, producing tiers, and
   evidence basis. Accept the first two proposals through human review.
2. Read the curated UI, context API, and best-effort export at the same revision.
   Match proposal identities and review state across all three. Also read the
   separately addressable recovered view and the verified-only export.
3. Change only the first proposal's cited source in a disposable fixture revision
   and re-ingest it. Inspect both proposals through all three curated surfaces.
   Attempt to reuse the first proposal without reconciling its changed evidence.
4. Perform the documented reconciliation/review flow against the new source and
   read all three surfaces again. Reopen the original revision and review history.

**Pass:** the same accepted projection appears consistently where the export
policy permits it; T3 remains InferredWeak and is excluded from verified-only.
Acceptance never rewrites recovered facts or includes the unaccepted control.
Changed evidence makes the affected proposal visibly stale until reconciled;
unchanged evidence is distinguished, and original provenance/history remains
inspectable. **Fail:** stale content silently remains current, a surface disagrees,
or acceptance upgrades tier. Retain before/after responses, export hashes, review
receipts, source revisions, and the reconciliation outcome.

## MT-H3-02 — Named project isolation across restart (AC-0109, T-0109)

**Gate: H3 PLANNED. Procedure: UNEXECUTED.**

Prerequisites: persistent named contexts, domain membership, review history, and
task references are implemented. Prepare projects A and B with the same relative
path and symbol names but different rule values and immutable input identities.

1. Create named contexts A and B. In each, ingest its fixture, associate a domain,
   make a distinct review decision, and retain a task reference. Record all IDs.
2. Switch A → B → A. Inspect domain evidence, source revision, decisions, and task
   references in each; follow the links to their owning records and sources.
3. Quit fully and relaunch. Repeat the inspection, then re-ingest a changed
   revision of A and switch to B again.
4. Through the documented context interface, request A's scoped record/task while
   selecting B. Verify it is rejected or explicitly identified as outside B,
   rather than rebound to B's same-named source.

**Pass:** names and records survive restart, every link retains its actual project
and revision, and A's update/reviews do not alter B. **Fail:** missing history,
silent project substitution, or cross-project evidence/task leakage. Retain the
project/revision/record matrix and responses before and after restart and update.

## MT-H4-01 — Shared durable investigation coordinator (AC-0110, T-0110)

**Gate: H4 PLANNED; MCP entry requires H5 support. Procedure: UNEXECUTED.**

Prerequisites: app and MCP investigation entry points and the shared durable
coordinator are implemented. Configure a named/versioned specialist, permitted
feature scope, tier ceiling, and finite time/token/tool budgets. Declare the
task-start latency bound before the run.

1. Start one permitted investigation in the app and a second through MCP against
   the same project/revision. Record request-to-task-ID latency for both.
2. Retrieve each task from both interfaces. Compare its durable ID, project,
   revision, scope, named agent/version, tier ceiling, input references, budgets,
   origin, and parent identity (absent for these root tasks).
3. Let the tasks complete; read their cited findings and task status through both
   interfaces. Restart the app and retrieve both tasks again by their original IDs.

**Pass:** both ingress paths use the same durable task records and enforce the
declared limits; IDs arrive within the declared bound without waiting for model
completion, and metadata/results remain consistent after restart. Distinct starts
need not share one ID. **Fail:** transport-local task copies disagree, required
metadata is missing, or task identity/results are lost. Retain the request/status
transcripts, durations, task records, and restart evidence.

## MT-H4-02 — Controlled local specialist investigation (AC-0191, T-0191)

**Increment #404. Procedure: UNEXECUTED. H4/H5 remain PLANNED.**

Prerequisites: a reachable loopback Ollama runtime and pinned model; production
coordinator/source-reader/bounded-provider integration; a pinned captured
TypeScript fixture with an independently reviewed expected answer kept outside
all model inputs. Record runtime/model identity and fixture/capture hashes before
starting. Do not provision cloud access as a substitute for this local procedure.

1. Start a Domain analyst investigation over that fixture with a scoped question.
   Record its durable task ID, request-to-ID latency, immutable specialist/prompt,
   provider/model, graph/scope snapshot and declared limits. Retrieve ordered
   events while the actual provider chooses query, evidence-read and finish steps.
2. Independently inspect the supplied input ledger and saved citations against the
   retained exact source occurrences. Compare each finding to the withheld
   expected answer; record unsupported claims, omissions and coverage limitations.
   Check raw source replay is absent and all generated findings remain T3/weak.
3. Run an Evidence auditor follow-up against the selected parent's saved result.
   Verify its history retains original scope, status, revision and uncertainty;
   no current-source lookup silently replaces the parent's historical basis.
4. Restart the app and reopen both task IDs and their historical citations. Change
   or remove the checkout, and verify retained source is still the cited source.
   Use a disposable capture to verify forgetting reports unavailability without
   changing the saved finding or opening current source.
5. Record actual calls, tool actions, read attempts, validation bytes, generated
   token reservations, provider-reported usage (or unknown), elapsed time and any
   limits/failures. Retain the journal and independent citation review on #404.

**Pass:** the actual local model drives the production query/read/finish loop,
results are evidence-supported within explicit coverage, and durable identity and
citations survive restart and source changes. **Fail:** scripted/fabricated output
substitutes for a model run, an oracle enters model input, source or authority is
silently rebound, limits are bypassed, or unknown outcomes are replayed.

A production-module harness run may establish coordinator/local-provider evidence
with separately CI-tested UI behavior; it is not native-app or external-MCP
end-to-end evidence. MT-H4-01 and all H5 cross-ingress procedures remain unexecuted
until their actual prerequisites and steps are run.

## MT-H5-01 — ACP capabilities, environment, and egress (AC-0111, T-0111)

**Gates: H4–H5 PLANNED. Procedure: UNEXECUTED.**

Prerequisites: ACP negotiation, coordinator policy, enforced runtime filesystem
boundary, and per-tier consent are implemented. Use a compatible local runtime,
a disposable read-only target, a permitted output directory, and synthetic
canaries outside both. Enable tool/runtime/network audit capture.

1. Record negotiated protocol/runtime capabilities and configured tool, path,
   tier, and egress permissions. Run a permitted context read and output write.
2. Request an unnegotiated tool, a target-code write, an out-of-scope canary read,
   and a confirmed-fact write. Repeat the requests when instructions embedded in
   repository text claim to grant those permissions. Compare target hashes and
   confirmed graph contents before and after.
3. Under local-only policy, request cloud execution and verify no cloud runtime
   launch or outbound payload occurs. Exercise the opted-in test provider with
   denied consent, then with consent for one exact redacted payload and tier;
   change that payload or tier and retry without new consent.
4. Complete an allowed investigation and inspect its returned proposal citations
   and confidence ceiling. Check the audit for tools/paths used during execution.

**Pass:** only negotiated and authorized operations execute; denied reads do not
return canaries, source/confirmed facts remain unchanged, and egress requires
matching per-tier consent. Allowed results are cited proposals. **Fail:** any
unauthorized execution, disclosure, egress, or fact upgrade, even if the runtime
reported a permission dialog. Retain redacted negotiation/denial/network records,
hash comparisons, consent receipts, and the successful proposal.

## MT-H5-02 — Cancellation, disconnect, and uncertain runtime outcomes (AC-0112, T-0112)

**Gates: H4–H5 PLANNED. Procedure: UNEXECUTED.**

Prerequisites: durable task lifecycle and runtime reconciliation are implemented.
Use a controllable runtime that exposes invocation IDs and can pause before work,
during work, and after producing a result but before host acknowledgement.

1. Start a task, wait for progress, and cancel it. Observe the runtime cancellation
   receipt and terminal state; query that task after reconnecting and restarting.
2. Start a second task, disconnect only the caller during work, and reconnect.
   Retrieve its progress and eventual result by the original ID; confirm the
   reconnect did not create another runtime invocation.
3. Start a third task and terminate the app while the runtime is paused before
   acknowledging its outcome. Relaunch and inspect the persisted task. Request a
   retry before reconciling the runtime invocation, then reconcile it using the
   documented recovery flow and retrieve the resulting terminal record.
4. Repeat the crash case with a runtime that cannot establish whether it finished.
   Inspect the unresolved state and attempted retry behavior.

**Pass:** progress/terminal records remain queryable, cancellation reaches the
runtime, and disconnect/restart do not silently duplicate work. An uncertain
outcome stays explicit and prevents automatic retry until reconciled; an
unreconcilable outcome is not invented as success. **Fail:** lost outcomes,
unpropagated cancellation, or duplicate invocation without reconciliation. Retain
task and invocation IDs, progress/cancel transcripts, restart records, and the
recovery decision for each case.

## MT-H5-03 — Delegation ancestry, deduplication, and shared limits (AC-0113, T-0113)

**Gate: H5 PLANNED. Procedure: UNEXECUTED.**

Prerequisites: nested MCP/ACP delegation and coordinator accounting are implemented.
Configure a depth limit of two descendant levels, finite shared tool/token/time
budgets, and a reproducible worker that requests specified child investigations.

1. Start a root MCP investigation. Have its ACP worker create a permitted child;
   read both records and verify project/revision, parent ancestry, and budget
   accounting remain connected to the root.
2. Submit the same child request concurrently and again after reconnect, using
   the documented deduplication identity. Count actual runtime invocations.
3. Attempt direct self-delegation, A → B → A ancestry recursion, and a chain
   extending beyond the configured depth. Inspect returned errors and task records.
4. Run two valid children whose combined requests exceed each shared budget in
   turn. Inspect the root/child usage ledger and any attempted later delegation.

**Pass:** duplicate requests do not duplicate execution, cycles and excess depth
are rejected explicitly, and children cannot reset or multiply their shared
allowance. Exhaustion stops further work with the responsible bound visible.
**Fail:** an unbounded loop, missing ancestry, or execution beyond the configured
enforced limits. Retain the task tree, deduplication keys, invocation counts,
configured limits, usage ledger, and denial records.

## MT-H5-04 — Real external MCP caller and ACP runtime (AC-0114, T-0114)

**Gate: H5 PLANNED. Procedure: UNEXECUTED.**

Prerequisites: a released external development application's MCP caller, a real
compatible ACP runtime, and Cartograph's documented connection instructions.
Record all product/runtime versions, negotiated capabilities, and finite budgets.
Mock protocol fixtures alone do not satisfy this procedure.

1. Connect the external caller and read a scoped Cartograph context page. Follow
   a citation and compare its project/revision with the app's corresponding view.
2. Start an ACP investigation through MCP. Record its durable task ID and observe
   real progress while it runs; disconnect/reconnect the caller and retrieve the
   completed cited result. Inspect the same task in Cartograph.
3. Start a separate investigation, observe progress, cancel from the caller, and
   retrieve its terminal outcome after reconnecting. Verify runtime cancellation.
4. Record whether MCP Tasks was negotiated. Execute these lifecycle steps through
   negotiated Tasks support when present; also exercise the explicit
   start/status/result/cancel compatibility path with Tasks disabled or a second
   real caller that does not negotiate it. Record the actual tool names used.

**Pass:** the real caller completes context read, investigation, progress,
reconnect, cancellation, and cited-result retrieval with the same durable records
as the app; the explicit lifecycle baseline also works without Tasks support.
**Fail:** any required step needs a mock or loses identity/provenance. Retain
redacted connection instructions, negotiation and lifecycle transcripts, runtime
invocation IDs, and the cited result; name any incompatible capabilities.

## MT-H6-01 — Cited structural measurements and missing coverage (AC-0115, T-0115)

**Gate: H6 PLANNED. Procedure: UNEXECUTED.**

Prerequisites: feature-scoped structural evaluation is implemented. Prepare a
small pinned multi-file feature with a known dependency cycle and independently
counted size, fan-in/out, and cohesion-proxy expectations under the documented
metric definitions; include available Git history and an external dependency.

1. Evaluate the declared feature twice at the same revision and budget. Compare
   every reported measurement with the independently calculated values and follow
   its source/revision citation. Record the selected and excluded scope.
2. Repeat with dependency evidence unavailable, and separately with history
   unavailable. Preserve the same source content and record changed input coverage.
3. Inspect measurement, coverage, and confidence fields independently. Attempt to
   follow a prior-revision citation after re-ingesting a changed fixture revision.

**Pass:** measurements match their documented definitions and pinned evidence;
identical inputs agree. Missing dependency/history inputs are explicitly reported
as incomplete coverage, not zero dependencies/changes or a confidence substitute.
Old evidence remains tied to its revision. **Fail:** incorrect counts, uncited or
misbound measurements, or hidden coverage loss. Retain the independent worksheet,
input/output identities, and full versus incomplete-coverage reports.

## MT-H6-02 — Contextual responsibility hotspots (AC-0116, T-0116)

**Gate: H6 PLANNED. Procedure: UNEXECUTED.**

Prerequisites: structural evaluation and the architecture specialist are
implemented. Before viewing its output, a reviewer selects two comparably large
files: one cohesive unit with a documented reason to remain together, and one
combining distinct responsibilities with documented coupling concerns. Freeze
their revision, responsibilities, constraints, and expected supporting evidence.

1. Evaluate each file in its feature/dependency scope with the same declared
   budgets. Inspect the measurements separately from the specialist's judgment.
2. For every hotspot assertion or exception, follow citations for responsibility,
   cohesion, coupling, and design constraints; compare them with the frozen review.
3. Remove the design document from a disposable input variant and repeat. Inspect
   whether the specialist acknowledges unknown constraints and revises uncertainty.

**Pass:** size alone yields no confirmed god-file claim. The justified large file
is not condemned solely for its size, and the responsibility hotspot is assessed
using cited context with scope, uncertainty, impact, and remedy. Interpretations
retain their inferred tier; missing constraints remain unknown. **Fail:** a
size-threshold-only verdict, invented rationale, or confirmed subjective judgment.
Retain both reports, the independent comparison, and the missing-document result.

## MT-H6-03 — Approved design, separate implementation, and re-ingestion (AC-0117, T-0117)

**Gates: H6–H7 PLANNED. Procedure: UNEXECUTED.**

Prerequisites: future-design review and before/after comparison are implemented.
Freeze a baseline repository revision, approved future design, and executable
acceptance cases. Obtain separate human authorization naming the external coding
agent, disposable implementation checkout, allowed change, and permitted commands.

1. Save the original recovered evidence and approved design/review IDs. Have the
   separately authorized external agent implement the change in its checkout.
   Confirm Cartograph's analysis itself has not written the ingested target.
2. Execute the approved acceptance cases against the changed checkout and retain
   observed results, command/runtime identities, and the tested source revision.
   Re-ingest that exact revision and compare it with the baseline and design.
3. Repeat the comparison on a controlled failing variant that violates one
   acceptance case. Also inspect a variant with no observed test result.
4. Reopen the original evidence and review history after both comparisons.

**Pass:** before/after evidence remains distinct and tied to actual revisions;
the report maps the change to approved design and observed tests, identifies the
failing case, and keeps unobserved behavior unknown. It never silently asserts
complete behavioral equivalence or rewrites original evidence. **Fail:** missing
authorization boundary, target writes from analysis, lost baseline, or a success
claim contradicted by a failing/missing test. Retain authorization, design/AC
references, diffs, test records, and both comparison reports.

## MT-H7-01 — Independent market pilot with recorded outcomes (AC-0118, T-0118)

**Gate: H7 PLANNED. Procedure: UNEXECUTED.**

Prerequisites: the preceding capabilities needed by the pilot are implemented;
an independently recruited new user has an authorized repository and the normal
distribution/onboarding instructions. Before observing results, freeze the pilot
scope, independent answer rubric, setup and investigation time/cost thresholds,
acceptable coverage, citation/accuracy targets, and improvement acceptance cases.

1. Observe the user install/configure Cartograph and ingest the chosen revision
   without operator intervention. Time setup and record every failure, recovery,
   credential prompt, coverage explanation, and instance of assistance separately.
2. Have the user find the feature breakdown and business rules for one domain,
   distinguish implemented behavior from documented/future design, identify design
   gaps, and investigate one architecture concern. Record elapsed time, runtime
   and token use/cost, the exact answers, citations, and disclosed uncovered scope.
3. Have an independent reviewer score those answers against the frozen rubric,
   following citations to the actual source. Record correct/total answers,
   supported/total assertions, uncovered cases, and disagreements.
4. Have the user select one improvement, review its design/acceptance criteria,
   and complete the separately authorized implementation and re-ingestion flow
   in MT-H6-03. Record test results, evidence comparison, and the user's review.
5. Compare observed setup, answer quality, coverage, time/cost, and improvement
   results with the preregistered thresholds. Record the user's unresolved friction
   and any follow-up issues before deciding this pilot's result.

**Pass:** the new user completes the workflow independently, meets the frozen
thresholds, and reviews one improvement supported by observed acceptance evidence.
**Fail:** assistance was required for completion, a threshold misses, evidence is
unsupported, or the improvement is unreviewed. Retain the anonymized session
record, scored rubric, measurements, and improvement artifacts. This procedure
records one pilot; broader market readiness still requires the several external
users and launch requirements named in SPEC-01, rather than extrapolating from
one successful participant.
