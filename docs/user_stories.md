# user_stories.md — Cartograph

**Schema (fixed):**
`ID · Title · Actor · Narrative(As a / I want / So that) · Priority(MoSCoW) · Status · Acceptance Criteria(AC-XXXX, Given/When/Then) · Security · Performance · Trace(Milestone, Crate, Flow?, Test IDs)`

**Status enum:** `Draft · Ready · In-Progress · In-Review · Blocked · Done · Deferred`

> Convention: Security and Performance facts are mapped onto the owning US/AC. Trace binds each story to a milestone, the owning crate(s), and test IDs (see `US-TM.md`).

---

### US-0001 — Add a repo set and define system topology
- **Actor:** Engineer
- **As a** engineer **I want** to add 1..N GitHub repos and declare which repos form one system **so that** the engine treats them as a single analyzable whole.
- **Priority:** Must · **Status:** Done
- **AC-0001** Given valid GitHub App/PAT auth, when I add a repo URL, then it is cloned read-only and listed with commit SHA.
- **AC-0002** Given a multi-repo set, when I edit `cartograph.system.toml`, then declared layer hints and channel identities are applied at ingest.
- **AC-0003** Given an unauthorized repo, when I add it, then I get a clear auth-failure with remediation and no partial clone.
- **AC-0049** Given any completed ingest, when its summary is shown, then TypeScript and Terraform each report source-file, node, and edge counts including explicit zeros.
- **AC-0050** Given a non-empty graph, when I request a clear and confirm the warning, then all graph facts are removed while durable job history remains intact.
- **Security:** Tokens stored in OS keychain; never logged; least-privilege App scopes.
- **Performance:** Shallow/sparse clone; 1 GB repo clones within bounded progress feedback.
- **Trace:** M0–M3 · `ingest`, `core-graph`, `app`, `ui` · — · T-0001..0003,T-0049..0050

### US-0002 — Deterministic extraction of server-side facts (TS first)
- **Actor:** Engine
- **As a** engineer **I want** import/call graphs, endpoints, and data access extracted statically **so that** server facts are Confirmed without inference.
- **Priority:** Must · **Status:** Done
- **AC-0004** Given a TS repo, when ingested, then endpoints (method/path/handler) are extracted via the framework adapter and marked Confirmed.
- **AC-0005** Given typed TS, when building call edges, then intra-procedural edges are complete and inter-procedural edges resolve where types permit.
- **AC-0006** Given any extracted fact, when inspected, then it carries provenance (file/span/commit, tier, extractor_id).
- **Security:** No code leaves device at T0.
- **Performance:** Incremental tree-sitter parse; re-parse only changed files by `content_hash`.
- **Trace:** M1 · `adapters-lang-ts`, `adapters-fw`, `core-prov` · — · T-0004..0006

### US-0003 — IaC resource graph + cloud capability resolution (Terraform/AWS)
- **Actor:** Engine
- **As a** engineer **I want** a deterministic resource graph with cloud-capability edges **so that** infra/cloud topology is Confirmed.
- **Priority:** Must · **Status:** Done
- **AC-0007** Given Terraform HCL, when parsed, then a resource DAG is built from interpolation references.
- **AC-0008** Given AWS resources, when resolved against the Capability Registry, then TRIGGERS/ROUTES/GRANTS edges are emitted deterministically.
- **AC-0009** Given Terraform state/plan JSON, when available, then T1 enrichment supersedes ambiguous T0 refs (observed provenance).
- **AC-0043** Given an API Gateway v1 integration with direct resource references, when resolved against the Capability Registry, then a Confirmed ROUTES edge links the REST API to its integration target.
- **AC-0044** Given a Lambda permission with direct resource references, when resolved against the Capability Registry, then a Confirmed TRIGGERS edge links its source ARN to the Lambda function.
- **AC-0045** Given Lambda@Edge associations nested under default or ordered CloudFront cache behaviors, when resolved against the Capability Registry, then Confirmed TRIGGERS edges link the distribution to every referenced Lambda function.
- **AC-0046** Given an EventBridge Pipe with direct resource references, when resolved against the Capability Registry, then a Confirmed TRIGGERS edge links its source to its target.
- **AC-0047** Given an IAM policy that references an `aws_iam_policy_document` defined in the same extraction, when its statement resources are resolvable, then Confirmed GRANTS edges target those resources with actions and evidence from the document; an absent or unresolved document target remains explicit.
- **AC-0048** Given a Terraform module with a literal local source confined to the ingest root, when extracted, then its resources and internal edges are instantiated under the `module.<name>.` address prefix recursively and deterministically; remote, escaping, symlink-escaping, or cyclic sources remain explicit leaf modules.
- **Security:** IAM GRANTS feed the security view; secrets in state are redacted.
- **Performance:** Registry lookups O(1) per resource type.
- **Trace:** M2,M6 · `iac`, `dynamic`, `spec`, `app` · — · T-0007..0009,T-0043..0048

### US-0004 — Event graph with channel-identity stitching
- **Actor:** Engine
- **As a** engineer **I want** producers and consumers linked by channel identity **so that** flows cross service and repo boundaries.
- **Priority:** Must · **Status:** Draft
- **AC-0010** Given a literal channel id on both sides, when matched, then a Confirmed PUBLISHES/SUBSCRIBES edge is created.
- **AC-0011** Given a channel id from a present config/env file, when resolved, then the edge is Confirmed via the config resolver.
- **AC-0012** Given a runtime-computed channel id, when unresolved at T0, then the hop escalates and, if still unresolved, emits a Gap with reason.
- **Security:** —
- **Performance:** Channel index keyed by identity for O(1) match.
- **Trace:** M3,M5,M6 · `events`, `dynamic`, `flowtracer`, `app` · — · T-0010..0012

### US-0005 — Client-side interaction graph (React first)
- **Actor:** Engine
- **As a** engineer **I want** screens, components, and data-fetch call sites extracted **so that** flows are anchored at user actions.
- **Priority:** Must · **Status:** Draft
- **AC-0013** Given a React/Next repo, when ingested, then Screen/Component nodes and FETCHES edges to endpoints are created.
- **AC-0014** Given a data-fetch call site, when the endpoint is resolvable, then FETCHES is Confirmed; otherwise it escalates.
- **Security:** —
- **Performance:** —
- **Trace:** M4 · `adapters-lang-ts` (tsx), `adapters-fw` · — · T-0013..0014

### US-0006 — Deterministic end-to-end flow tracer
- **Actor:** Engine
- **As a** engineer **I want** cross-layer flows traced T0-first with explicit gaps **so that** flows are trustworthy.
- **Priority:** Must · **Status:** Draft
- **AC-0015** Given a trigger, when traced, then each hop resolves at the lowest possible tier and records its tier.
- **AC-0016** Given any unresolved hop, when no tier resolves it, then a Gap node truncates that branch (no silent completion).
- **AC-0017** Given a completed trace, when scored, then flow_status ∈ {Verified, Partial, Inferred} per the scoring rule.
- **Security:** —
- **Performance:** Path queries bounded; long flows stream incrementally to UI.
- **Trace:** M3–M5 · `flowtracer` · F-* · T-0015..0017

### US-0007 — Provenance and confidence on every fact
- **Actor:** Engine
- **As a** engineer **I want** every node/edge tagged with tier + confidence + evidence **so that** integrity is enforced.
- **Priority:** Must · **Status:** Ready
- **AC-0018** Given any fact, when stored, then it has {tier, confidence_tier, evidence[], extractor_id, content_hash}.
- **AC-0019** Given a T2/T3 producer, when it runs, then it cannot overwrite or upgrade a T0/T1 fact (R-INT-1).
- **AC-0020** Given an agent (T3), when it acts, then it can only propose edges/annotations with cited evidence; it cannot write T0/T1 (R-INT-3).
- **Security:** —
- **Performance:** —
- **Trace:** M0,M8 · `core-prov`, `agents` · — · T-0018..0020

### US-0008 — Semantic tier with eval gating
- **Actor:** Engine
- **As a** engineer **I want** semantic matching gated by paired evals **so that** inferred links meet a precision floor.
- **Priority:** Should · **Status:** Draft
- **AC-0021** Given unresolved hops, when T2 runs, then proposals are marked InferredStrong with evidence.
- **AC-0022** Given a labeled eval set, when T2 is measured, then proposals below the precision floor are excluded from `best-effort` exports.
- **Security:** Embeddings computed locally by default.
- **Performance:** ANN index query sub-100ms typical.
- **Trace:** M7 · `semantic`, `llm` · — · T-0021..0022

### US-0009 — Bounded agentic tier with egress firewall
- **Actor:** Engine
- **As a** engineer **I want** agents to propose resolutions only, with per-tier cloud opt-in **so that** privacy and integrity hold.
- **Priority:** Should · **Status:** Draft
- **AC-0023** Given a Local-only policy, when T3 needs a cloud provider, then it hard-fails closed (no silent egress).
- **AC-0024** Given cloud opt-in, when an agent runs, then the consent dialog shows the exact span-level payload leaving the device.
- **AC-0025** Given an agent proposal, when accepted/rejected, then the decision persists and re-applies on re-ingest.
- **Security:** Default-deny cloud egress; secret redaction on payloads.
- **Performance:** —
- **Trace:** M8 · `agents`, `llm` · — · T-0023..0025

### US-0010 — Atlas graph canvas with confidence overlay
- **Actor:** Engineer
- **As a** engineer **I want** to explore the unified graph with layer filters and tier coloring **so that** I can see what is confirmed vs inferred vs gapped.
- **Priority:** Must · **Status:** Draft
- **AC-0026** Given the graph, when I filter by layer, then only that layer's nodes/edges render.
- **AC-0027** Given the confidence overlay, when active, then nodes/edges are colored by tier and Gaps are flagged.
- **AC-0028** Given a node, when selected, then the evidence panel shows file/span/commit with read-only jump-to-source.
- **Security:** Source view is read-only (NG1).
- **Performance:** Canvas remains interactive at 10k+ nodes (Cytoscape.js).
- **Trace:** M9 · `app`, UI · — · T-0026..0028

### US-0011 — Flow Inspector with explicit gaps
- **Actor:** Engineer
- **As a** engineer **I want** to view a traced flow as a sequence with per-hop tier badges and explicit gap cards **so that** I trust the flow.
- **Priority:** Must · **Status:** Draft
- **AC-0029** Given a trigger, when I open Flow Inspector, then the flow renders as a sequence (React Flow + Mermaid) with tier badges.
- **AC-0030** Given a Gap hop, when shown, then it appears as an "unresolved" card with reason and attempted escalation.
- **AC-0031** Given the export toggle, when set to `verified-only`, then InferredWeak hops are excluded.
- **Security:** —
- **Performance:** —
- **Trace:** M9 · `app`, `flowtracer`, UI · F-* · T-0029..0031

### US-0012 — Spec Workbench, curation, and export
- **Actor:** Engineer
- **As a** engineer **I want** to review compiled artifacts, accept/reject inferred items, and export **so that** I get an official, trustworthy spec.
- **Priority:** Must · **Status:** Draft
- **AC-0032** Given compiled artifacts, when viewed, then every assertion shows inline provenance.
- **AC-0033** Given an inferred item, when I accept/reject/annotate, then the decision persists and survives re-ingest via content_hash.
- **AC-0034** Given export, when run, then it honors R-INT-5 (`verified-only` vs `best-effort`) and includes Gap + Drift registers.
- **AC-0035** Given the full set, when exported, then it produces user_stories, US-TM, flow dossiers, data model, ADRs, registers.
- **Security:** —
- **Performance:** —
- **Trace:** M9–M10 · `spec`, UI · — · T-0032..0035

### US-0013 — ADR recovery and drift detection
- **Actor:** Engine
- **As a** engineer **I want** found and recovered ADRs linked to governed targets, with conflicts surfaced **so that** decisions and drift are visible.
- **Priority:** Should · **Status:** Draft
- **AC-0036** Given existing ADR/RFC files, when parsed, then they link to governed targets (Confirmed).
- **AC-0037** Given the graph, when recovered ADRs are drafted, then they are marked recovered/inferred with evidence.
- **AC-0038** Given an ADR-vs-code conflict, when detected, then it appears in the Drift register mapped to the offending flow/edge.
- **Security:** —
- **Performance:** —
- **Trace:** M9 · `spec`, `agents` · — · T-0036..0038

### US-0014 — Determinism and re-ingest idempotency
- **Actor:** Engine
- **As a** engineer **I want** re-ingesting the same commit to yield an identical graph **so that** outputs are reproducible.
- **Priority:** Must · **Status:** Draft
- **AC-0039** Given the same commit, when re-ingested, then T0 graph content hashes are identical.
- **AC-0040** Given a changed file, when re-ingested, then only affected facts are recomputed.
- **Security:** —
- **Performance:** Delta re-ingest scales with change size, not repo size.
- **Trace:** M10 · `core-graph`, `core-prov` · — · T-0039..0040

### US-0015 — Security view of the spec
- **Actor:** Engineer
- **As a** engineer **I want** auth edges and IAM grants projected into a security view **so that** unauthenticated endpoints and over-broad policies surface as findings.
- **Priority:** Should · **Status:** Draft
- **AC-0041** Given endpoint auth facts, when projected, then unauthenticated endpoints are listed as findings mapped to US/AC.
- **AC-0042** Given IAM GRANTS, when analyzed, then over-broad policies are flagged with the resource scope.
- **Security:** This story *is* the security projection.
- **Performance:** —
- **Trace:** M9 · `iac`, `spec` · — · T-0041..0042
