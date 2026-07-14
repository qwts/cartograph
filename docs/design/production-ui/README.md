# Handoff: Cartograph — Cross-Layer Spec-Recovery Engine (production UI)

> **About this copy.** This is the authoritative design handoff for the
> production UI (epic #114), committed from the original handoff bundle.
> The bundle's interactive HTML prototype (`Cartograph.dc.html`,
> `support.js`) is intentionally **not** committed — it is a rendering
> runtime the handoff itself says not to port; the spec below plus
> `screenshots/` are the reference. Token values here supersede the older
> Stitch export in [`../DESIGN.md`](../DESIGN.md).

## Overview
Cartograph ingests a software system (repo, local folder, or system manifest), deterministically recovers its structure, flows, and a **provenance-tagged specification**, and reports exactly what it could and could not confirm. The core product principle is **truth semantics**: every recovered fact carries a confidence *tier* and full provenance; the engine never guesses — unresolved facts become explicit, escalatable **Gaps**.

The prototype is a dense, native-macOS-style IDE shell with a left rail, command palette (⌘K), an evidence drawer, an escalation/resolution modal, and nine primary surfaces. Everything runs **local-first**; any cloud egress is opt-in per recovery tier and **fails closed**.

## About the Design Files
The files in this bundle (`Cartograph.dc.html`, `support.js`) are a **design reference created in HTML** — a working prototype showing intended look and behavior. They are **not production code to copy directly**. `support.js` is the prototype's private rendering runtime and should **not** be ported; ignore it except to run the prototype locally.

Your task is to **recreate this design in the target codebase's existing environment**. Per the project's ADRs, that stack is **Tauri + Rust core + React (TypeScript) front-end** (see `docs/adr/ADR-0001-tauri-rust-react.md`). Use the codebase's established component library, state, and styling patterns. If a front-end environment does not yet exist, implement in React + TypeScript.

## Fidelity
**High-fidelity.** Final colors, typography, spacing, iconography, interaction states, and copy are all specified below and are authoritative. Recreate the UI faithfully using the codebase's component primitives. Sample *data* (fact counts, hashes, file spans, eval numbers) is illustrative placeholder content — wire real data from the Rust core in its place, but preserve the **shape** and **semantics** of every field.

---

## Design Tokens

### Color
| Token | Hex | Use |
|---|---|---|
| `bg` | `#0e0e0e` | App background / content area |
| `bg-rail` / `bg-header` | `#0b0b0b` | Left rail, header, status bar |
| `surface` | `#141414` | Cards, panels, controls |
| `surface-alt` | `#111111` | Inset panels, list rows, modals' inner cards |
| `surface-raised` | `#161616` | Drawer, command palette, modals, legend |
| `border` | `#2a2a2a` | Default 1px borders |
| `border-dim` | `#1f1f1f` / `#222` / `#202020` | Section dividers, subtle rules |
| `text` | `#e5e2e1` | Primary headings/values |
| `text-body` | `#c1c6d5` | Body text |
| `text-muted` | `#888` | Secondary / labels |
| `text-faint` | `#666` / `#555` | Captions, chevrons |

### Confidence tiers (the semantic palette — never use color alone; always pair with label + code + shape)
| Tier | Code | Label | Hex | Shape / line |
|---|---|---|---|---|
| Confirmed (static) | `T0` | Confirmed | `#27C93F` | Rectangle node; solid edge (edge stroke uses a muted `#3f6b45`) |
| Confirmed (observed) | `T1` | Confirmed (observed) | `#27C93F` (dot at 70% opacity) | same as T0 |
| Inferred Strong | `T2` | Inferred Strong | `#2D9CDB` | Rectangle; dashed edge |
| Inferred Weak | `T3` | Inferred Weak | `#F2C94C` | Rectangle; dashed yellow edge |
| Gap | `GAP` | Gap | `#EB5757` | **Octagon** node, 2px **dashed** red border; dashed red edge |
| Unsupported | — | Unsupported | `#ffb688` | register finding, not a graph fact |
| (No evidence) | — | No evidence found | `#7f7f7f` | register finding |
| Primary / accent | — | — | `#abc7ff` | CTAs, active nav, links, focus |
| Local-safe accent | — | — | `#7adaa1` / `#9bfcc1` | "on-device / 0 bytes egress" affirmations |

Tier badge pattern: `background: <color>1a; color: <color>; border: 1px solid <color>44; font: 9px 700 JetBrains Mono; padding: 2px 6px; radius: 4px`. (Hex+`1a`/`44` = alpha suffixes; replace with `rgba()` in the target.)

### Typography
- **UI / sans**: `Inter`, weights 400/500/600/700.
- **Mono** (codes, hashes, file spans, metrics, egress payloads): `JetBrains Mono`, 400–700.
- **Icons**: Material Symbols Outlined.
- Scale: page title 22–26px/700; surface title 16–17px/600; card title 12–13px/600; body 11.5–12.5px; labels 10.5–11px/700 uppercase `letter-spacing:.05em`; mono metadata 9.5–10.5px. Minimum readable size ~9.5px (used sparingly for badges).

### Radius & shadow
- Radius: chips/badges 4–5px; buttons/inputs 7–8px; cards 9–11px; modals/drawer 12–14px.
- Shadows: drawer `-30px 0 70px rgba(0,0,0,.5)`; modal `0 30px 70px rgba(0,0,0,.6)`; palette `0 24px 70px rgba(0,0,0,.7)`.

### Spacing
Content padding 24–34px; card padding 12–15px; gaps 6–14px. 8-ish px rhythm.

### Motion
- `carto-spin` 1.3s linear infinite (loading glyphs).
- `carto-dash` 1s linear infinite (animated dashed Gap/inferred edges, `stroke-dashoffset` → -16).
- `carto-up` .16–.18s ease (drawer/modal/palette entrance: fade + 10px translateY).
- Thin global progress bar: `carto-indet` 1.1s ease-in-out infinite.
- Progress/width transitions .2–.3s.

---

## App Shell

**Left rail (54px, `#0b0b0b`):** logo tile (30px, gradient `135deg,#abc7ff,#7adaa1`), then 8 nav buttons (44×44, radius 9). Active = `background: rgba(171,199,255,.13); color:#abc7ff` plus a 3px×20px accent tick on the left edge. Bottom: ⌘K palette launcher. Every icon-only button has a `title` **and** `aria-label`.

Nav order & icons (Material Symbols): Workspace `space_dashboard`, Atlas `map`, Flows `account_tree`, Spec Workbench `description`, Gaps & Drift `report`, Provenance & Eval `analytics`, Jobs `terminal`, Settings `settings`.

**Header (50px):** breadcrumb (`image-trail › <Surface>`), a **Legend** button, and a **scope chip** (right) that reflects context — "Whole system" (green `public`), "Single evidence trail" (accent `my_location`) when a node is selected, or "Atlas · <layer>" (yellow `filter_alt`).

**Status bar (30px):** left = live status (icon+text, animates while ingesting); right = egress summary in mono — `Local-only · 0 bytes egress` unless a cloud tier is consented.

**Global progress:** a 2px accent indeterminate bar pinned to the top edge whenever any long job is running (`globalBusy`). Long work is **non-blocking** — never a modal spinner over the app.

**Command palette (⌘K / Ctrl+K):** centered overlay, 520px, dimmed backdrop. Lists all 8 surfaces with icon, label, hint, and a mono shortcut tag (⌘1–⌘8). Row hover `#242424`. `Esc` closes. Toggling ⌘K is wired on `window` keydown.

---

## Screens / Views

> Router is a single `view` value. Selecting a node opens the **evidence drawer** as an overlay on top of the current surface (it must NOT block clicks to the rest of the app — see Interactions #1). `connect`/`preflight`/`recover` are the ingest flow; `report` is the Workspace landing.

### 1. Connect (`connect`)
Purpose: choose an ingest target. Source selector (GitHub / Local folder / System manifest — segmented, active = accent border+tint), a mono "Target" readout, and a green local-only reassurance strip (`verified_user`, "Local-only preflight. Nothing leaves the device unless you opt a tier into cloud in Settings."). Buttons: Back, Preflight (primary).

### 2. Preflight (`preflight`)
Purpose: detect languages/frameworks/adapters locally. Header notes **0 bytes egress**. Four result cards, each: icon, title, checklist of items, optional note strip. Critically, the last two cards enforce the **three-way classification from first contact**:
- **Potential system gaps** (`link_off`, red): evidence exists but isn't statically resolvable (e.g. dynamically injected function bodies via `executeScript`, runtime-computed sync host). Note: these become explicit **System Gaps** with Resolution Strategies after recovery — *never* a guess.
- **Unsupported patterns** (`block`, `#ffb688`): no adapter covers them (WASM module, inline `eval()`). Note: a **tool limitation, not a System Gap**.

Do **not** conflate these two, and never say an Unsupported item "becomes a Gap." Buttons: Back, Structure only, Run full recovery (primary).

### 3. Recover (`recover`)
Purpose: progress view. Centered spinner (`progress_activity`, spinning), stage label (Cloning+discovering → T0 parse/call graph/endpoints → adapters/channel identity/flow tracer), mono %, 320px progress bar, and a **Run in background** button (moves work to Jobs, returns to a usable UI). In the prototype this is a timed simulation; wire to real core progress events.

### 4. Workspace / Report (`report`) — landing
- Title `image-trail` + mono commit chip `@ a1b9f30` + **Re-ingest** button.
- **Outcome card:** icon `insights`, "Partial recovery", tier badge "InferredStrong overall". Subtitle states the honest tally: *"…6 open findings — 3 gaps and 2 unsupported patterns (plus 1 no-evidence) — are listed explicitly rather than guessed."* Primary CTA **"Triage 3 gaps"** → Gaps & Drift; secondary "Provenance & eval".
- **Provenance health:** 5 summary cards (Confirmed / Inferred Strong / Inferred Weak / Gap / Unsupported) with count + subtext. A note points to the full surface.
- **Artifacts grid** (2-col): user_stories.md, Flow dossiers, US-TM.md, Topology/resource map, Gap register, ADR set + drift. Each card carries **two independent badges** (see truth semantics below): a green **"Artifact generated"** (generation axis) and a separate **authority** badge (`Recovery: authoritative` / `partial` / `inferred`, or for the Gap register a **"6 open findings"** state — the register card shows exactly ONE completion-style badge, never two).

### 5. Atlas (`atlas`)
Purpose: system topology graph. Layer filter (All / Client / Events / Server / Storage). Canvas is an SVG edge layer + absolutely-positioned node buttons over an 880×520 board.
- **Nodes** carry shape by kind (rectangle = service/component, diamond = gateway/channel, **octagon dashed red = Gap**), a tier-colored icon, label, and a tier badge. Border encodes tier (solid for confirmed, dashed for weak, 2px dashed red for gap).
- **Edges** are cubic Bézier `path`s; confirmed = solid muted green with arrowhead; gap/weak = dashed animated with colored arrowhead. Each edge has a clickable mono label chip (tier code + relation, e.g. `T0 TRIGGERS`, `GAP executeScript`).
- Clicking any node **or** edge opens the evidence drawer for that element. Every node/edge must be clickable and crash-free (see State/Interactions).

### 6. Flows (`flows`)
Purpose: inspect one BusinessFlow (F-0007 · Capture image at cursor) as a hop sequence.
- Header: flow id, status badge (`PARTIAL (1 gap)`), trigger summary, flow score.
- **Verified-only / Best-effort** segmented toggle. **Verified-only** excludes InferredWeak hops (shows "1 hidden") but **retains the Gap node**; **Best-effort** includes the InferredWeak hop, annotated, and the projection note makes the +1 count difference explicit.
- Zoom out / Fit / zoom in controls. **Layout must be responsive and never horizontally scroll**: hops are cards in a **wrapping** flex row (they flow to a second row at narrow widths) at a readable minimum size (~168px wide floor); Fit sizes card width to the available viewport. Do not use a CSS transform that shrinks text while leaving intrinsic scroll width.
- Each hop card: kind, title, tier badge, mono file:line. A **Gap** hop is dashed-red and shows its stop reason; clicking it opens the **Resolution Strategy** modal. Non-gap hops open the evidence drawer. Selecting a hop must not lose flow context.

### 7. Spec Workbench (`spec`)
Purpose: read/curate the recovered spec. Left doc list (User stories, Traceability matrix, Flow dossiers, ADR set, Gap register) with tier badges. Right = document blocks. Prominent prototype/illustrative qualifier strip.
- **Lock semantics:** **T0 Confirmed blocks are read-only** — Accept/Reject/Annotate are truly `disabled` with `aria-disabled="true"` and an inline "Confirmed T0 — locked, read-only" explanation. **Only proposed T2/T3 blocks are curatable.** Curating preserves original evidence & provenance (shown after a decision). Do not expose disabled controls as enabled in the a11y tree.

### 8. Gaps & Drift (`triage`)
Purpose: the honest register. Header: "Register complete · **6 open findings** — 3 gaps · 2 unsupported · 1 no-evidence." Three tabs:
- **Lanes** — the three-way split: **System gaps** (3, red `link_off`), **Unsupported patterns** (2, `#ffb688` `block`), **No evidence found** (1, grey `search_off`). Each item row: id (for gaps), severity badge, text, tail (next tier / file), chevron. Gap rows open the Resolution Strategy.
- **By escalation tier** — gaps grouped under T1/T2/T3 with open/resolved state.
- **Drift** — ADR/code conflicts (e.g. MV3 service worker vs observed persistent listener).

### 9. Provenance & Eval (`prov`)
Purpose: the required dedicated provenance/eval surface. All headings use `role="heading" aria-level="3"`; charts use `role="img"` with descriptive `aria-label`s (and per-bar labels) so nothing depends on color alone.
- **Tier distribution** — stacked bar + legend. Header: "Tier distribution · 134 graph facts · 2 unsupported patterns." Counts: Confirmed 98, Inferred Strong 22, Inferred Weak 11, Gap 3, Unsupported 2 (these must reconcile with every other surface — single source of truth).
- **Extractor coverage** — per-extractor bar (id@version, coverage %, fact count).
- **Paired-eval quality gate (T2/T3)** — precision/recall vs floor, GATE PASS / BELOW FLOOR.
- **Evidence health over re-ingests** — grouped stacked bars; aria: "Confirmed facts rise from 62 to 98 while Gaps fall from 8 to 3…". Footer states the determinism invariant (re-ingest ⇒ identical graph by content-hash).

### 10. Settings (`settings`)
Purpose: tiers, providers, egress. Green "Local core · on-device by default" banner with live egress summary.
- Per **recovery tier** card: code chip, name, description, and an on/off toggle. **T0 is "always on / lock"** and states it **never invokes an LLM**. T2/T3 expose a **provider** choice (Local (Ollama) / Cloud (opt-in)).
- **Fail-closed cloud egress consent:** selecting Cloud reveals a consent panel that must show, *before* consent: **Provider / model** (e.g. "OpenAI · text-embedding-3-large", "Anthropic · claude-sonnet-4"), **Deployment/region**, **Endpoint**, exact **source span** leaving the device, **purpose/action**, and **estimated size**. Consent is explicit and revocable; default is local-only. No cloud call is possible until granted.

---

## Overlays

### Evidence drawer
Right-side panel, **resizable** (320px ↔ 560px via the handle), animates in. Read-only. Contents:
- Header: "Evidence · read-only", title, tier badge (code · label), confidence.
- For Gaps/inferred: a "Why this is a Gap / Inferred" note strip.
- **"Why this tier?"** collapsible explanation.
- **Source (read-only)** code block with line numbers and highlighted span; span shows the **full range incl. end line:col** (e.g. `bytes 210–980 · L22:1 – L26:24`).
- **Provenance** table: Tier, Confidence, Extractor (id@version), File, Span, Commit, **content_hash** (a **valid 64-hex BLAKE3** digest shown in full, wrapping, with a **copy** affordance whose output equals the displayed value).
- **Supporting evidence** list (navigable, read-only).
- For Gaps: **"Open Resolution Strategy"** CTA.
- Footer: "Source navigation is read-only. T2/T3 never overwrite or masquerade as T0/T1."

### Resolution Strategy / Escalation modal (reuse for every gap)
Centered modal. Shows: gap id + text; the **escalation ladder** (T0 established → next tier), with "T2/T3 never overwrite T0/T1"; **why deterministic recovery stopped**; strategy cards (tiers attempted, proposed next escalation, required evidence, consent/scope); a **Local / Cloud** run-mode toggle with egress · cost · latency · privacy and **export impact**. Running → a proposal card (result tier, confidence, text) with **Accept as <tier> / Reject**. Accepting records the decision; proposals **never auto-join** the spec; original evidence preserved.

### Legend popover
Confidence tiers (code, label, method), node shapes, and edge line treatments — the canonical key for the non-color-alone encoding.

---

## Interactions & Behavior (critical, verified requirements)
1. **Overlay must not swallow clicks.** The evidence drawer is a right-anchored panel with `pointer-events:none` on its full-screen wrapper and `pointer-events:auto` only on the panel — so a single click on any Atlas node/edge, flow hop, or rail route while a drawer is open selects/navigates immediately (it does not require a first click just to dismiss). `Esc` and the ✕ still close it. Navigating surfaces clears the selection.
2. **No route/node/edge is a dead or crashing path.** Source-span/coordinate values are objects; normalize them to display strings before render (never render an object as a child). Wrap the surface in a **route-level error boundary** so one bad inspector value can't blank the app.
3. **Counts reconcile everywhere** (Workspace, Provenance, Gaps & Drift, Spec doc badge, artifact card): 3 gaps / 2 unsupported / 1 no-evidence / 6 findings; 134 graph facts. Keep a single data source.
4. **Keyboard-first & a11y:** ⌘K palette; visible focus; descriptive `aria-label`s on icon-only controls; real `disabled`/`aria-disabled` on locked T0 controls; `role="heading"`/`role="img"`+aria on the Provenance surface; resizable 320px drawer.
5. **Jobs are durable:** running/queued/complete/failed with progress, current stage, resume/retry/cancel, failure detail, timestamps, and artifact/output links. Long work stays non-blocking (thin top bar), never the generic light-theme modal.

## State Management
Prototype holds one component's local state; in the target, model these as app state / store slices and back the data with the Rust core over Tauri commands:
- `view` (active surface) · `selNode` + `selKind` (evidence selection) · `escalate` + `escStep` (`strategy`|`running`|`proposal`) + `escMode` (`local`|`cloud`).
- `ingest` (`running`|`done`) + `mode` (`full`|`fileonly`) + `progress`.
- `flowMode` (`verified`|`besteffort`) + `flowZoom` (fit/zoom). `triageMode`, `doc`, `source`.
- Curation: `decisions` (per T2/T3 block), `accepted` (per gap).
- Settings: `tiers` (t1/t2/t3 on/off), `providers` (local/cloud), `consented` (per tier) → derives `egressSummary` and fail-closed behavior.
- `jobsState` (per-job overrides: cancelled/retried/resumed). `cmdk`, `legend`, `whyOpen`, `drawerWide`, `copied` (hash copy feedback).

## Assets
- **Fonts:** Inter, JetBrains Mono, Material Symbols Outlined (Google Fonts). Swap to the codebase's icon set if it has one; keep glyph meaning.
- **No raster/image assets.** Nodes/edges are SVG+CSS. All "images" are data-driven.
- No brand assets — the logo is a CSS gradient tile.

## Domain source of truth
The recovery model, tiers, and provenance rules are specified in the attached repo docs — implement against these, not the prototype's sample values:
- `docs/SPEC-00_master.md` — master spec.
- `docs/adr/ADR-0001-tauri-rust-react.md` — stack.
- `docs/adr/ADR-0002-escalation-ladder.md` — tier/escalation + provenance-first integrity model.
- `docs/adr/ADR-0003-treesitter-adapter-spi.md` — extractor/adapter SPI.

## Screenshots (`screenshots/`)
Reference renders of every surface and overlay at ~900px wide. Use them to match layout, density, and color; the exact token values above are authoritative where a screenshot is ambiguous.

**Surfaces**
- `01-workspace.png` — Workspace/Report landing: outcome card, provenance-health cards, artifacts grid (note the two independent badges per card).
- `02-atlas.png` — Atlas topology: node shapes (rect/diamond/octagon), tier-colored edges + label chips, layer filter.
- `03-flows.png` — Flow Inspector: responsive wrapping hop sequence (two rows), Verified-only/Best-effort toggle, Fit/zoom, Gap + InferredWeak annotations.
- `04-spec-workbench.png` — Spec Workbench: doc list + blocks; locked T0 (read-only) vs curatable T2/T3.
- `05-gaps-and-drift.png` — Gaps & Drift register, Lanes tab (System gaps / Unsupported / No evidence).
- `06-provenance-and-eval.png` — Provenance & Eval: tier distribution, extractor coverage, paired-eval gates, evidence-health chart.
- `07-jobs.png` — Jobs: durable states (complete/running/queued/failed) with progress, stage, actions, failure detail, artifact links.
- `08-settings.png` — Settings: per-tier cards, T0 always-on, provider choices.

**Overlays & ingest flow**
- `09-settings-cloud-egress-consent.png` — fail-closed cloud consent panel (provider/model, deployment, endpoint, exact span, size).
- `10-evidence-drawer-confirmed.png` — evidence drawer: "Why this tier?", read-only source with span range, full provenance table + full BLAKE3 hash with copy.
- `11-resolution-strategy-modal.png` — escalation ladder, why-stopped, strategy cards, run-mode/egress.
- `12-command-palette.png` — ⌘K palette.
- `13-legend.png` — tier / shape / edge-line legend (the non-color-alone key).
- `14-connect.png` — Connect (source selector + local-only reassurance).
- `15-preflight.png` — Preflight: "Potential system gaps" vs "Unsupported patterns" split.

## Files in this bundle
- `Cartograph.dc.html` — the full high-fidelity prototype (all 10 surfaces + overlays). Open in a browser to interact. Read its template + logic for exact styles/values.
- `support.js` — the prototype's rendering runtime. **Do not port.** Present only so the HTML runs locally.
- `screenshots/` — 15 reference renders (see above).
