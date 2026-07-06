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

## MT-SB-01 — Stories render on-brand

1. `cd ui && npm run storybook`.
2. Walk Shell/* and Atlas/* stories.
3. **Pass:** components use the DESIGN.md dark tokens; the four TierBadge
   states are visually distinct (R-INT-2); `Shell/App` stories run their
   interactions without error.
