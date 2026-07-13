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

## MT-M7-01 — Local semantic resolution clears its precision gate (T2)

1. Start Ollama locally and make the configured embedding model available:
   `ollama pull nomic-embed-text` (Cartograph never downloads it implicitly).
2. Run
   `cargo test -p semantic real_ollama_resolves_eval_gated_gap -- --ignored --nocapture`.
3. Inspect the printed report: provider is local Ollama, paired-eval precision
   meets the configured floor, ANN lookup is below 100ms, and one explicit
   channel Gap is replaced only in the returned best-effort overlay.
4. Stop Ollama and repeat the semantic preview from the app/API.
5. **Pass:** the stopped provider fails explicitly with no graph change or
   network fallback; the passing preview edge is Semantic/InferredStrong with
   evidence from both Gap and target, while the stored confirmed graph retains
   its original Gap (AC-0021, AC-0022, R-INT-1, M7 exit gate).

## MT-SB-01 — Stories render on-brand

1. `cd ui && npm run storybook`.
2. Walk Shell/* and Atlas/* stories.
3. **Pass:** components use the DESIGN.md dark tokens; the four TierBadge
   states are visually distinct (R-INT-2); `Shell/App` stories run their
   interactions without error.
