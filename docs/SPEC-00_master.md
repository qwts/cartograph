# SPEC-00 — Master Specification
## "Cartograph" — Cross-Layer Spec-Recovery Engine (working name)

| Field | Value |
|---|---|
| Doc ID | SPEC-00 |
| Version | 0.1.1 (draft — §17 tracker amended per ADR-0007) |
| Status | Ready-for-build planning |
| Owner | Chris Kane |
| Purpose | Single source of truth for design/architecture/requirements. Intended to answer ~95% of build-time questions for the author and for Claude Code. |
| Consumers | Human author + Claude Code (implementation agent) |
| Companion files | `user_stories.md`, `US-TM.md`, `tracker.csv`, `adr/ADR-00xx.md` |

### Decisions locked (this document is built on these)
1. **Runtime:** Tauri 2 — Rust core + web UI. Targets macOS (primary) and Windows.
2. **Scope:** Spec **recovery** only. The deliverable is the recovered specification. No code regeneration/scaffolding — explicit non-goal.
3. **LLM posture:** Pluggable provider interface; **local-first** (Ollama default); cloud providers (Claude/Grok/GPT/Gemini) are **opt-in per analysis tier** with explicit egress consent.

### Decide-and-stated defaults (override inline if wrong)
- First-class language adapters: **TypeScript/JavaScript → Python → Go** (in that order).
- IaC: **Terraform (HCL) + Pulumi**. Cloud resolver: **AWS first → Azure → GCP**.
- Event systems first: **AWS (SQS/SNS/EventBridge) + Kafka + in-process/domain-event buses**.
- Client frameworks first: **React/Next → Vue → Svelte**, plus a cross-cutting **GraphQL** operation extractor.
- Graph store: **Kuzu** (embedded) with a **SQLite recursive-CTE** fallback path.
- GitHub: **GitHub App auth** + `git2` clone, with **PAT** fallback and optional `gh` CLI shell-out.

---

## 1. Problem, Goals, Non-Goals

### 1.1 Problem
A running system's true specification is distributed across IaC, cloud topology, server code, event wiring, and client code — and is almost never written down accurately. Existing approaches fail on one of two axes: pure-static tools miss business semantics and cross-layer flows; pure-LLM tools hallucinate flows that do not exist. The result is documentation no one trusts.

### 1.2 Goals
- **G1** Ingest 1..N GitHub repos that together form one system.
- **G2** Build a single **unified, provenance-tagged knowledge graph** across the five layers (infra, cloud, server, events, client).
- **G3** Recover **business logic** (rules/validations/computations), **business flows** (cross-layer, end-to-end paths), and **ADRs** (recovered and linked to the code they govern).
- **G4** Emit an **official specification** in the author's artifact conventions (US/AC, ADR, flow docs, traceability matrix) such that the system *could be re-specified and rebuilt by a third party* from the document alone.
- **G5** Guarantee **integrity**: every asserted fact carries provenance and a confidence tier; the system prefers an explicit **gap** over an unsupported assertion (Score=0 preference).

### 1.3 Non-Goals (explicit)
- **NG1** Not a code editor / IDE / Copilot surface. No editing of target code.
- **NG2** No code regeneration, scaffolding, migration, or "rebuild" automation. Output is a *specification*, full stop.
- **NG3** Not a clone of any existing documentation, mapping, or APM product. Layer-tracing + provenance-first integrity is the differentiator.
- **NG4** Not a runtime observability platform. Dynamic-tier inputs are consumed if present; the app does not deploy agents into production.
- **NG5** No multi-user/server backend in v1. Single-user desktop, local-first.

---

## 2. Core Thesis — The Escalation Ladder + Provenance

The entire engine is organized as a **four-tier escalation ladder**. A fact (node) or relationship (edge) is produced by the **lowest tier that can establish it**. Higher tiers only run on what lower tiers leave unresolved.

| Tier | Name | Method | Produces | Confidence ceiling |
|---|---|---|---|---|
| T0 | **Deterministic** | Static parse (tree-sitter), type/import/call graph, IaC HCL/AST graph, framework adapters, channel-identity literal match | Confirmed facts | `Confirmed` |
| T1 | **Dynamic** | Execution-derived evidence: `terraform show -json`/state, `pulumi stack export`, test runs, OpenTelemetry traces, captured logs, recorded HTTP | Confirmed-by-observation facts | `Confirmed` (observed) |
| T2 | **Semantic** | Local embeddings + similarity over symbols/channels/docs; clustering; name/contract matching | Inferred links/groupings | `InferredStrong` |
| T3 | **Agentic** | Bounded LLM agent reads surrounding code/evidence and proposes a resolution with cited evidence | Inferred links/annotations | `InferredWeak` |
| — | **Gap** | None of the above resolved it | Explicit `Gap` node with reason | `Gap` |

**Hard rules (integrity spine):**
- **R-INT-1** T2/T3 may **never overwrite or upgrade** a T0/T1 fact. They only fill unresolved slots.
- **R-INT-2** Every node/edge stores its producing tier and confidence tier. The UI and exports render tier distinctly; inferred content is never visually or structurally indistinguishable from confirmed content.
- **R-INT-3** T3 (agents) can **read** the graph and **propose** edges/annotations only. Agents have **no write access** to confirmed facts and cannot create T0/T1 facts.
- **R-INT-4** A flow with any unresolved hop is emitted as **partial with a Gap node**, never silently completed.
- **R-INT-5** "Score=0 export" mode emits only `Confirmed`+`InferredStrong` content with all `Gap`s listed; "best-effort" mode additionally includes `InferredWeak`, clearly annotated.

This ladder is the product. Sections 3–7 are its data; sections 8–9 are its container and surface.

---

## 3. Target-System Model — Five Layers + Deterministic Extractors

Each layer has a **deterministic (T0) extractor** as its primary source. "Do not be vague" applies most here.

### 3.1 Infrastructure (IaC) → Resource Graph
| Source | T0 extraction | T1 enrichment |
|---|---|---|
| Terraform (HCL) | Parse `resource`/`module`/`data`/`output`/`locals`; build resource DAG from interpolation refs (`${aws_x.y.arn}`); resolve `for_each`/`count` structurally | `terraform show -json` (plan/state) → authoritative resolved graph |
| Pulumi (TS/Py/Go) | Resolve resource constructor calls (`new aws.s3.Bucket(...)`) via the **language adapter** AST; capture parent/dependsOn | `pulumi preview --json` / `stack export` |
**Output nodes:** `Resource{type, logical_id, provider, region?}`. **Edges:** `DEPENDS_ON`, `REFERENCES`.

### 3.2 Cloud → Topology + Capability Resolution
A hand-curated **Capability Registry** maps resource types to runtime semantics deterministically. Examples (AWS):
- `aws_lambda_event_source_mapping` ⇒ `TRIGGERS(SQS|Kinesis|DynamoDB-stream → Lambda)` edge.
- `aws_lb_target_group_attachment` + listener ⇒ `ROUTES(ALB → ECS/EC2 target)`.
- `aws_iam_policy` document ⇒ `GRANTS(principal → action → resource)` (drives least-privilege gap detection).
- `aws_sns_topic_subscription` ⇒ `SUBSCRIBES(endpoint → topic)`.
The registry is versioned deterministic knowledge, **not inference**. Priority: AWS → Azure → GCP.

### 3.3 Server-side → Service Graph
Per-language adapter (T0):
- Import/module graph; intra-procedural call graph always; inter-procedural where the type system permits (Go, typed TS = strong; Python/Ruby = partial, escalates to T1/T2).
- **Framework adapters** map registration patterns → endpoints: Express/Fastify/Nest, FastAPI/Flask/Django, `net/http`/chi/gin/gRPC.
- Data access detection: ORM models/repositories, raw SQL string literals → `DataEntity`.
**Output nodes:** `Endpoint{method, path, handler_sym}`, `Symbol`, `DataEntity`. **Edges:** `HANDLES`, `CALLS`, `READS`/`WRITES`.

### 3.4 Events → Event Graph (the cross-layer stitch)
T0 detection by SDK call-signature registry: `sqs.sendMessage`, `sns.publish`, `eventBridge.putEvents`, Kafka `producer.send`/`@EventPattern`, in-proc bus `emit/on`, webhook route registration.
**Channel identity matching** links producer↔consumer:
- Literal channel id (queue URL / topic ARN / `detail-type` / Kafka topic string) ⇒ **T0 deterministic edge**.
- Channel id from env/config file present in repo ⇒ T0 via config resolver.
- Channel id computed at runtime ⇒ escalate (T1 trace → T2 semantic name match → T3 agent → Gap).
**Output nodes:** `Channel{kind, identity}`. **Edges:** `PUBLISHES`, `SUBSCRIBES`.
This graph is what carries flows across repo boundaries.

### 3.5 Client-side → Interaction Graph
T0 per client adapter:
- **Route graph** (React Router/Next/Vue Router) → `Screen` nodes.
- **Component tree** + **data-fetch call sites** (`fetch`/axios/react-query/RTK Query/GraphQL ops) → `FETCHES(component → Endpoint)` — the **front anchor of every business flow**.
- **State stores** (Redux/Zustand/Pinia) for flow state.
**Output nodes:** `Screen`, `Component`. **Edges:** `RENDERS`, `FETCHES`, `TRIGGERS(action → fetch)`.

---

## 4. Unified Knowledge Graph — Two-Layer Schema

Two layers in one store. The **Code layer** holds T0/T1 facts. The **Domain layer** is *projected up* from the code layer (deterministic projections where possible; inferred ones tier-marked).

### 4.1 Code layer
**Nodes:** `Repo, Module, File, Symbol(Function|Class|Method), Endpoint, DataEntity, Resource, Channel, Screen, Component, Config`.
**Edges:** `DEFINED_IN, IMPORTS, CALLS, HANDLES, READS, WRITES, PUBLISHES, SUBSCRIBES, TRIGGERS, ROUTES, RENDERS, FETCHES, DEPENDS_ON, REFERENCES, GRANTS`.

### 4.2 Domain layer
**Nodes:** `Capability, BusinessFlow, BusinessRule, DomainEntity, Actor, ADR`.
**Edges:** `REALIZES(flow→capability), STEP_OF(code-edge→flow), GOVERNS(rule→symbol|endpoint), MAPS_TO(DomainEntity→DataEntity), DECIDES(ADR→target), TRIGGERED_BY(flow→Screen|Channel|Schedule), PERFORMED_BY(flow→Actor)`.

### 4.3 Provenance (on every node and edge)
```
Provenance {
    tier:            Deterministic | Dynamic | Semantic | Agentic
    confidence_tier: Confirmed | InferredStrong | InferredWeak | Gap
    evidence:        [EvidenceRef]        // 0..n
    extractor_id:    String               // which adapter/resolver produced it
    content_hash:    String               // content-addressed for idempotent re-ingest
    created_at, supersedes?: ...
}
EvidenceRef { repo, path, span(byte_start, byte_end), commit_sha }
```
`content_hash` makes re-ingest idempotent and enables stable diffs across commits.

### 4.4 Store choice
- **Primary: Kuzu** (embedded property graph, Cypher-style, fast path queries; no server process — fits a desktop). *Verify at M0: embedding fit + maintenance status.*
- **Fallback: SQLite/WAL** edge table + **recursive CTE** traversal. Slower on deep paths but zero-risk and already your backbone.
- **Analytical: DuckDB** (optional) for fact-level aggregate queries (e.g., "endpoints with no inbound FETCHES").

---

## 5. Business-Flow Tracing (the heart)

### 5.1 Definition
A **BusinessFlow** is a directed, cross-layer path that realizes one unit of business intent, anchored at a **trigger** and traced forward through the code graph:

```
Trigger(Screen.action | Channel | Schedule | Webhook)
  → FETCHES → Endpoint → HANDLES → Symbol → CALLS* → {WRITES DataEntity | PUBLISHES Channel}
  → (Channel) SUBSCRIBES → Symbol(other repo) → ... → terminal(DataEntity write | external | Gap)
```

### 5.2 Hop-resolution algorithm
For each outgoing hop from the current node, resolve via the ladder, **first success wins**:

1. **T0** — direct graph edge exists (literal call target, literal channel id, IaC trigger). Stop.
2. **T1** — observed in a trace/test/state export. Stop.
3. **T2** — semantic match (e.g., `publish("order.placed")` ↔ `subscribe` on a channel whose embedding/name matches). Mark `InferredStrong`.
4. **T3** — agent reads both sites + config, proposes the link with cited evidence. Mark `InferredWeak`.
5. **Gap** — emit `Gap{reason}`, truncate this branch.

### 5.3 Flow confidence score
```
hop_weight = { Confirmed:1.0, InferredStrong:0.6, InferredWeak:0.3, Gap:0.0 }
flow_score = mean(hop_weight over hops)
flow_status = Verified   if all hops Confirmed
            | Partial    if any Gap present
            | Inferred   otherwise
```
Export honors **R-INT-5**: `verified-only` vs `best-effort`.

### 5.4 BusinessRule extraction
T0: guard conditions, validation schemas (zod/pydantic/JSON-Schema), authorization checks, computed values in handlers → `BusinessRule{predicate, location}` linked via `GOVERNS`. T2/T3 only *name/cluster* rules; they never invent predicates.

---

## 6. ADR Recovery

- **Recovered ADRs** (T2/T3): cluster decisions implied by the graph (e.g., "SQS chosen over direct invoke for fulfillment", "Postgres as system-of-record", "monorepo vs polyrepo topology") and draft an ADR in the author's ADR format, **marked recovered/inferred**, linked via `DECIDES`.
- **Found ADRs** (T0): parse existing `adr/`, `docs/decisions/`, RFCs already in repos; link to governed targets.
- Conflicts between found ADRs and observed code surface as **drift findings** ("ADR-0007 says no synchronous cross-service calls; flow F-0012 contains one").

---

## 7. Official Spec Output (the deliverable)

The spec compiler **projects the graph → artifacts**. Artifact set:

| Artifact | Source | Format |
|---|---|---|
| `user_stories.md` | Capabilities + flows → US/AC | author schema (see `user_stories.md`) |
| `US-TM.md` | US ↔ AC ↔ Module ↔ Flow ↔ ADR ↔ Test | traceability matrix |
| Flow dossiers | each BusinessFlow | Markdown + Mermaid sequence + provenance table |
| Resource/topology map | Resource graph | Mermaid/Graphviz + table |
| Data model | DataEntity + MAPS_TO | ERD (Mermaid) + table |
| ADR set | found + recovered | author ADR format |
| Gap register | all `Gap` nodes | table (the explicit 5% the system could not confirm) |
| Drift register | ADR/code conflicts | table |

**US/AC mapping rule:** one `Capability` → one or more US; each terminal/branch condition of its realizing flows → AC (Given/When/Then). Security and performance facts (auth edges, rate limits, IAM grants, timeouts) map onto US/AC per the author's "security/perf mapped to US/AC" standard.

---

## 8. Application Architecture (Tauri)

MVC mapping: **Rust core = Model + Controller**, **web UI = View**. Plugin-based via the adapter SPI. Interface-abstracted (traits). Local-first.

### 8.1 Crate map (Rust workspace)
| Crate | Responsibility | Tier(s) |
|---|---|---|
| `core-graph` | Kuzu binding, schema, query API | — |
| `core-prov` | Provenance + confidence model, content-addressing | — |
| `ingest` | GitHub App auth (`octocrab`), clone (`git2`), repo discovery, **topology manifest** | T0 |
| `adapters-lang-*` | Language adapters wrapping **tree-sitter** grammars (one crate per family) | T0 |
| `adapters-fw` | Framework registries (HTTP/event SDK signatures) | T0 |
| `iac` | HCL parse (`hcl-rs`), Pulumi-via-adapter, **Capability Registry** | T0 (+T1 hooks) |
| `events` | Producer/consumer SDK registry, channel-identity resolver | T0 |
| `dynamic` | terraform/pulumi JSON, OTel trace + test-run ingest | T1 |
| `semantic` | Local embeddings + ANN index | T2 |
| `agents` | Bounded broker + propose-only agents | T3 |
| `flowtracer` | Cross-layer path engine + tier escalation | T0–T3 |
| `llm` | **Pluggable provider trait** + per-tier egress policy | T2/T3 |
| `spec` | Graph → artifact compiler | — |
| `app` | Tauri commands, job orchestration, UI event bus | — |

### 8.2 Plugin SPI (contracts — the only "code" in this doc; spaces, not tabs)
```rust
// Deterministic-tier producers MUST NOT call the LLM.
pub trait LanguageAdapter: Send + Sync {
    fn id(&self) -> AdapterId;
    fn languages(&self) -> &[Language];
    fn detect(&self, repo: &RepoView) -> Confidence;
    fn parse_file(&self, file: &SourceFile) -> ParseResult;        // tree-sitter -> Symbols
    fn call_edges(&self, unit: &Unit) -> Vec<CallEdge>;
    fn endpoints(&self, unit: &Unit, fw: &FrameworkRegistry) -> Vec<Endpoint>;
    fn data_access(&self, unit: &Unit) -> Vec<DataAccess>;
    fn event_ops(&self, unit: &Unit, ev: &EventRegistry) -> Vec<EventOp>;
}

pub trait HopResolver: Send + Sync {
    fn tier(&self) -> Tier;                                        // T0..T3
    fn try_resolve(&self, hop: &UnresolvedHop, g: &GraphView) -> Resolution;
}
// Resolution = Resolved { edge, confidence, evidence } | Unresolved | Gap { reason }

pub trait LlmProvider: Send + Sync {
    fn id(&self) -> ProviderId;
    fn locality(&self) -> Locality;                                // Local | Cloud
    fn capabilities(&self) -> ProviderCaps;                        // embeddings | chat | tool_use
    fn embed(&self, batch: &[String]) -> Result<Vec<Embedding>>;
    fn complete(&self, req: CompletionReq) -> Result<Completion>;
}
```
A new language/cloud/event system = a new adapter crate implementing the SPI. No core changes (open/closed).

### 8.3 Data stores
- **Graph:** Kuzu (fallback SQLite recursive-CTE).
- **Relational/state spine:** SQLite/WAL — jobs, provenance log, artifact versions, eval results, config.
- **Embedding index:** `usearch` (fallback `sqlite-vec`). *Verify binding at M7.*
- **Evidence blobs:** content-addressed files under app data dir; referenced by `content_hash`.

### 8.4 Process / concurrency
Rust core runs ingest/analysis on a worker pool (`tokio` + `rayon` for CPU-bound parse). Jobs are durable (SQLite job table) and resumable. UI subscribes to job/event stream via Tauri events. No blocking of the webview thread.

---

## 9. Frontend & Interaction Design (web UI)

Stack: **React + TypeScript + Vite**. UI state: **Zustand**. Big graph canvas: **Cytoscape.js**. Editable flow/spec canvas: **React Flow**. Diagram export: **Mermaid + SVG**. Styling per `frontend-design` skill.

### 9.1 Primary surfaces
1. **Workspace / Repo set** — add repos (GitHub App / PAT), define system topology (which repos form one system), trigger ingest, watch job progress. Provenance health summary (counts by confidence tier).
2. **Atlas (graph canvas)** — the unified graph. Layer filter (infra/cloud/server/events/client). **Confidence overlay**: nodes/edges colored by tier; Gaps flagged. Search, focus, expand-neighbors. Click a node → evidence panel (file/span/commit, jump-to-source read-only).
3. **Flow Inspector** — pick a trigger → render the traced flow as a sequence (React Flow + Mermaid). Each hop shows its tier badge; Gaps are explicit "unresolved" cards with the reason and the escalation that was attempted. Toggle `verified-only` vs `best-effort`.
4. **Spec Workbench** — the compiled artifacts (US/AC, ADRs, registers). Inline provenance on every assertion. Human can **accept / reject / annotate** inferred items; rejections are recorded and re-applied on re-ingest. Export controls.
5. **Gap & Drift Registers** — the honest 5%: everything unconfirmed and every ADR/code conflict, sortable by layer/flow/severity.
6. **Provenance/Eval panel** — paired-eval results, coverage metrics, tier distribution over time.

### 9.2 Interaction principles
- **Read-only on target code** (NG1). Source view is navigation, never edit.
- **Inferred ≠ confirmed**, always — color, badge, and a "why this tier" hover with evidence (R-INT-2).
- **Human-in-the-loop curation** is a first-class workflow: accept/reject/annotate decisions persist and survive re-ingest via `content_hash` + decision log.
- Command surface: keyboard-driven palette; every long action is a durable, resumable job.

---

## 10. GitHub Integration

- **Auth:** GitHub App (installation token) preferred; PAT fallback; optional `gh` CLI shell-out for environments already authenticated.
- **Clone/read:** `git2` (libgit2) shallow clone + sparse where possible. API via `octocrab` for metadata, PR/commit history (feeds T1 evidence + ADR timeline).
- **Topology manifest:** `cartograph.system.toml` declares the repo set, layer hints, env/config locations, and known channel identities. Author-editable; T2 may *suggest* additions (always confirmed by user).
- **Incremental:** v1 one-shot ingest; file-watch / commit-delta incremental is M-later. `content_hash` makes deltas cheap.

---

## 11. LLM & Privacy

- **Provider trait** (`LlmProvider`) with `locality` = Local | Cloud.
- **Default:** Ollama (local) for both embeddings (T2) and agent completions (T3).
- **Cloud opt-in is per-tier and per-action**, gated by an explicit egress consent dialog that shows exactly what payload leaves the machine (code spans, never whole repos by default).
- **Egress policy** is config + enforced in the `llm` crate: a Local-only policy makes T2/T3 hard-fail closed rather than silently call cloud.
- Providers behind the trait: Ollama (default), Claude, Grok, GPT, Gemini.

---

## 12. Security & Privacy Model

- **Local-first**; no telemetry by default. All analysis runs on-device.
- **Secret handling:** tokens in OS keychain (macOS Keychain / Windows Credential Manager via Tauri/`keyring`). Never written to the graph or logs.
- **Secret scanning on ingest:** detect and **redact** secrets from evidence blobs and from any LLM payload.
- **Least-privilege GitHub App** scopes (read-only contents + metadata).
- **Egress firewall:** enforced per §11; default-deny for cloud LLM.
- **Provenance is also a security feature:** IAM `GRANTS` edges + endpoint auth edges feed a "security view" of the spec (unauthenticated endpoints, over-broad policies surface as findings mapped to US/AC).

---

## 13. Confidence, Provenance & Evaluation

- **Confidence tiers:** Confirmed / InferredStrong / InferredWeak / Gap (your existing tiering).
- **Coverage metrics:** % nodes/edges Confirmed; % flows Verified vs Partial; Gap count per layer.
- **Paired evals (your methodology):** for the semantic/agent tiers, hold out a labeled set of known links; measure precision/recall of T2/T3 against T0 ground truth on repos where T0 is complete. Quality gate: T2/T3 must clear a precision floor before their outputs are shown un-flagged in `best-effort` exports.
- **Eval-gated write path:** T2/T3 proposals enter a staging area; only proposals above threshold (or human-accepted) join the exported spec.
- **Determinism check:** re-ingesting the same commit must yield an identical graph (content-hash equality) — a CI invariant.

---

## 14. Milestone Plan (M0–M10)

Each milestone names explicit tech and an exit gate. Preference order honored throughout: **deterministic > dynamic > semantic > agentic**.

| M | Goal | Explicit tech | Exit gate |
|---|---|---|---|
| **M0** | Skeleton + stores | Tauri 2, Rust workspace, Kuzu (verify) + SQLite/WAL, React+Vite+TS, Zustand | App boots; empty graph round-trips; job table durable |
| **M1** | Deterministic single-repo (TS) | tree-sitter-typescript, Express/Nest fw adapter | Import+call graph + endpoints for one TS repo; evidence jump-to-source |
| **M2** | IaC + cloud (Terraform/AWS) | `hcl-rs`, AWS Capability Registry | Resource graph + TRIGGERS/ROUTES edges; topology map artifact |
| **M3** | Events + deterministic flow tracer | events SDK registry, `flowtracer` (T0 only) | End-to-end T0 flow Screen→…→DataEntity within one repo; flow dossier export |
| **M4** | Client-side anchor | tree-sitter-tsx, React Router/Next adapter, react-query/fetch detector | FETCHES edges; flows anchored at Screen.action |
| **M5** | Multi-repo stitching | `cartograph.system.toml`, channel-identity matcher | Cross-repo flow via literal channel ids; Gap nodes where unresolved |
| **M6** | Dynamic tier | `terraform show -json`, `pulumi stack export`, OTel/test-run ingest | T1 resolves a previously-Gap hop; observed-fact provenance |
| **M7** | Semantic tier | Ollama embeddings, `usearch` (verify), paired-eval harness | T2 fills unresolved channel/call hops above precision floor |
| **M8** | Agentic tier (bounded) | `agents` broker, `LlmProvider` (Ollama default), egress firewall | T3 proposes links with cited evidence; propose-only enforced; no T0 overwrite |
| **M9** | Spec compiler + Workbench | `spec` crate, Mermaid, React Flow, accept/reject curation | Full artifact set incl. US-TM, Gap + Drift registers; curation persists |
| **M10** | Quality gates + export modes | eval gates, determinism CI invariant, `verified-only`/`best-effort` export | Re-ingest determinism passes; export honors R-INT-5; Python+Go adapters added |

(Python and Go adapters are folded in across M1/M10; TS is the M1 proving ground.)

---

## 15. Tech Stack Decision Table (top-3 bounded; **pick** in bold)

| Concern | Options (≤3) | Pick / rationale |
|---|---|---|
| Shell/runtime | **Tauri**, Electron, Wails(Go) | **Tauri** — Rust core matches deterministic ethos; small footprint; Mac+Win |
| Core language | **Rust**, Go, C++ | **Rust** — safety + perf for the parse/graph core; strong tree-sitter/Tauri story |
| Parsing | **tree-sitter**, native compilers/LSP, ANTLR | **tree-sitter** — uniform multi-language, incremental, you've used it |
| Graph store | **Kuzu**, SQLite recursive-CTE, DuckDB+PGQ | **Kuzu** (verify); SQLite CTE as zero-risk fallback |
| Relational/state | **SQLite/WAL**, DuckDB, RocksDB | **SQLite/WAL** — your backbone; durable jobs/provenance |
| Embedding index | **usearch**, hnsw_rs, sqlite-vec | **usearch** (verify binding); sqlite-vec fallback |
| Local LLM | **Ollama**, llama.cpp, LM Studio | **Ollama** — your existing local rigs |
| GitHub | **octocrab + git2**, gh CLI, gix | **octocrab + git2**; gh CLI optional |
| HCL | **hcl-rs**, `terraform show -json`(T1), tree-sitter-hcl | **hcl-rs** for T0; JSON for T1 enrichment |
| UI framework | **React+Vite+TS**, SvelteKit, SolidJS | **React** — ecosystem + Cytoscape/React Flow |
| Big graph canvas | **Cytoscape.js**, React Flow, Sigma.js | **Cytoscape.js** — scales, path styling |
| Editable flow canvas | **React Flow**, Cytoscape, tldraw | **React Flow** — editable flow/spec views |
| Diagram export | **Mermaid+SVG**, Graphviz/DOT, D2 | **Mermaid+SVG** — portable, Claude-Code-friendly |
| UI state | **Zustand**, Redux Toolkit, Jotai | **Zustand** — light, sufficient |

**Verify-at-build (integrity flags):** Kuzu embedding fit + maintenance; `hcl-rs` coverage of your real Terraform; `usearch`/`fastembed` Rust bindings; OTel ingest format for your stack. These are the four claims most likely to have drifted since the author's knowledge cutoff — Claude Code should confirm before relying on them.

---

## 16. Risks & Open Questions (the explicit 5%)

| ID | Risk / question | Disposition |
|---|---|---|
| OQ-1 | Inter-procedural call graphs in Python/Ruby are weak at T0 | Escalate to T1/T2; mark Inferred; accept Gaps |
| OQ-2 | Computed channel identities defeat T0 event stitching | T1 trace ingest is the real fix; semantic match is best-effort |
| OQ-3 | Kuzu suitability for embedded desktop at scale | SQLite-CTE fallback de-risks; benchmark at M0 |
| OQ-4 | "Capability" clustering quality (T2) | Eval-gated; human curation in Workbench |
| OQ-5 | Monorepo vs polyrepo topology inference | Author-declared manifest primary; suggestion only |
| OQ-6 | GraphQL/event schemas as contracts across layers | Treat schema files as T0 contract anchors (high value, add early) |
| OQ-7 | How much code leaves device for cloud LLM | Span-level payloads + redaction + egress consent; default local |

---

## 17. Traceability

Requirements live in `user_stories.md` (US-0001+/AC-0001+, fixed schema + status enum). The matrix `US-TM.md` binds US ↔ AC ↔ Module(crate) ↔ Milestone ↔ Flow ↔ ADR ↔ Test. The build tracker is **GitHub Issues + the Cartograph project board** (per ADR-0007; the original `tracker.csv` is frozen at `docs/archive/tracker.csv`). ADRs for *this app's own* decisions are in `docs/adr/`.
