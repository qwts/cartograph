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

## MT-SB-01 — Stories render on-brand

1. `cd ui && npm run storybook`.
2. Walk Shell/* and Atlas/* stories.
3. **Pass:** components use the DESIGN.md dark tokens; the four TierBadge
   states are visually distinct (R-INT-2); `Shell/App` stories run their
   interactions without error.
