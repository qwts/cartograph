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
