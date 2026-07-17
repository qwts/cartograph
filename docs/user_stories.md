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
- **AC-0076** Given accumulated terminal jobs, when I clear finished jobs and confirm the warning, then done/failed/cancelled rows are removed while queued/running/interrupted jobs and all graph facts remain intact.
- **AC-0077** Given the production Jobs surface, when it renders, then it offers only lifecycle verbs on existing work (Cancel/Retry/Resume and Clear finished) — no job-creation control.
- **AC-0078** Given any recovery command (ingest, add-repo, add-system, or an ingest retry/resume), when it runs, then extraction executes on a blocking worker thread — the webview/main thread never blocks, the UI stays interactive, and job progress renders throughout.
- **AC-0085** Given one or more ingested targets, when the Workspace renders, then it states what the current system contains — each repo with its commit identity, derived from the graph's own facts (never from history logs, which survive a clear) — and that further ingests merge into the same system; the destructive action is labeled in system terms (Clear system) and its confirmation names exactly what is removed (every recovered fact) and what survives (job history and settings).
- **AC-0094** Given a running recovery job (ingest, add-repo, or add-system), when I navigate away from the Recovering screen or run it in background, then it stays reachable from the status bar ("Ingesting…") and from a "View live" action on its row in the Jobs surface; while it runs, both surfaces show a best-effort, non-persisted live detail of the file and layer (application/infra/language) currently being read, throttled so it never floods the event bridge.
- **Security:** Tokens stored in OS keychain; never logged; least-privilege App scopes.
- **Performance:** Shallow/sparse clone; 1 GB repo clones within bounded progress feedback.
- **Trace:** M0–M3 · `ingest`, `core-graph`, `app`, `ui` · — · T-0001..0003,T-0049..0050,T-0076..0078,T-0085,T-0094

### US-0002 — Deterministic extraction of server-side facts (TS/JS/Python/Go/Java)
- **Actor:** Engine
- **As a** engineer **I want** import/call graphs, endpoints, and data access extracted statically **so that** server facts are Confirmed without inference.
- **Priority:** Must · **Status:** In-Progress
- **AC-0004** Given a TS repo, when ingested, then endpoints (method/path/handler) are extracted via the framework adapter and marked Confirmed.
- **AC-0005** Given typed TS, when building call edges, then intra-procedural edges are complete and inter-procedural edges resolve where types permit.
- **AC-0006** Given any extracted fact, when inspected, then it carries provenance (file/span/commit, tier, extractor_id).
- **AC-0053** Given Python that import-proves FastAPI or Flask, when the repo is ingested, then Confirmed T0 File/Symbol/IMPORTS/CALLS facts plus literal Endpoint/HANDLES registrations are recovered with exact evidence, directory-proven imported calls are joined deterministically, lookalike framework calls are ignored, and the ingest summary reports Python separately.
- **AC-0054** Given Go that import-proves `net/http`, chi, or gin, when the repo is ingested, then Confirmed T0 File/Symbol/IMPORTS/CALLS facts plus literal Endpoint registrations are recovered with exact evidence, local-package calls and handlers are joined deterministically, unresolved handler expressions retain the Endpoint with an explicit HANDLES Gap, dynamic routes and lookalike registrations are ignored, files requiring an undeclared build target are excluded, server-only layer hints are honored, and the ingest summary reports Go separately.
- **AC-0079** Given a Java repository, when it is ingested, then classes/interfaces/enums/records and methods become Confirmed T0 Symbols with exact evidence spans, same-class and import-proven cross-file calls are joined deterministically repo-wide, a declared-package import whose target cannot be proven fails closed to an explicit Gap, foreign-package imports assert nothing, and the ingest summary reports Java separately.
- **AC-0080** Given Java that import-proves Spring Web annotations (named or wildcard `org.springframework.*` imports), when a `@RestController`/`@Controller` class is parsed, then `@{Get,Post,Put,Delete,Patch}Mapping` methods become Confirmed Endpoints with class-level `@RequestMapping` path composition and HANDLES edges to their handler methods; lookalike annotations without the proving import produce no endpoints.
- **AC-0095** Given a `.js`/`.jsx`/`.mjs`/`.cjs` repo, when ingested, then the same TypeScript-crate grammar recovers Confirmed T0 File/Symbol/IMPORTS/CALLS facts and Express/Next/React-Router endpoints and screens as it does for `.ts`/`.tsx` (no separate JS adapter, and Preflight/Settings report JavaScript as installed rather than a requestable planned adapter); an extensionless relative import resolves to the real file and symbol regardless of its actual source extension once the whole directory is known, rather than a phantom `.ts`-guessed placeholder.
- **AC-0099** Given a TS/JS source with an `eval()` or `new Function()` whose code argument is compile-time-known — a string literal, a substitution-free template, or a same-file `const`/const-object member proven by binding (never a name coincidence; for `new Function` the last string argument is the body and earlier ones are parameter names), then the string content is parsed by the same extractor and its Symbols, CALLS, and event sites are emitted as Confirmed T0 facts carrying `via: "eval"` with evidence citing the argument's span at the eval site, the enclosing symbol gains a CALLS edge into the extracted code's entry symbol so flows cross the eval boundary, and Preflight's `inline-eval` finding for a fully proven site closes on the next scan (adapter-supplied claims, so the textual scan and the AST proof never disagree) while a const-shaped-but-unproven argument downgrades to an explicit potential Gap and any other dynamic argument — including a shadowed local `eval`/`Function` binding, which is not the global and yields no facts — stays an Unsupported finding, never a guessed fact.
- **Security:** No code leaves device at T0.
- **Performance:** Incremental tree-sitter parse; re-parse only changed files by `content_hash`.
- **Trace:** M1,M10 · `adapters-lang-ts`, `adapters-lang-python`, `adapters-lang-go`, `adapters-lang-java`, `adapters-fw`, `core-prov`, `ingest`, `app`, `ui` · — · T-0004..0006,T-0053..0054,T-0079..0080,T-0095,T-0099

### US-0003 — IaC resource graph + cloud capability resolution (Terraform/Pulumi/AWS)
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
- **AC-0051** Given TypeScript that import-proves an AWS Pulumi constructor, when a resource with a literal logical name is parsed, then a Confirmed T0 Resource plus REFERENCES/DEPENDS_ON (including `parent`/`dependsOn`) and applicable shared Capability Registry edges are emitted; lookalike constructors without a Pulumi import produce no IaC facts.
- **AC-0052** Given a Pulumi stack export or preview JSON artifact declared as `pulumi_json`, when it is ingested, then matching T0 Pulumi resources gain separate Dynamic/Confirmed observed inputs, outputs, URN, and evidence while Pulumi secret wrappers are redacted and unmatched observations never fabricate T0 resources.
- **Security:** IAM GRANTS feed the security view; secrets in state are redacted.
- **Performance:** Registry lookups O(1) per resource type.
- **Trace:** M2,M6 · `adapters-lang-ts`, `iac`, `dynamic`, `spec`, `app` · — · T-0007..0009,T-0043..0048,T-0051..0052

### US-0004 — Event graph with channel-identity stitching
- **Actor:** Engine
- **As a** engineer **I want** producers and consumers linked by channel identity **so that** flows cross service and repo boundaries.
- **Priority:** Must · **Status:** Done
- **AC-0010** Given a literal channel id on both sides, when matched, then a Confirmed PUBLISHES/SUBSCRIBES edge is created.
- **AC-0011** Given a channel id from a present config/env file, when resolved, then the edge is Confirmed via the config resolver.
- **AC-0012** Given a runtime-computed channel id, when unresolved at T0, then the hop escalates and, if still unresolved, emits a Gap with reason.
- **Security:** —
- **Performance:** Channel index keyed by identity for O(1) match.
- **Trace:** M3,M5,M6 · `events`, `dynamic`, `flowtracer`, `app` · — · T-0010..0012

### US-0005 — Client-side interaction graph (React first)
- **Actor:** Engine
- **As a** engineer **I want** screens, components, and data-fetch call sites extracted **so that** flows are anchored at user actions.
- **Priority:** Must · **Status:** Done
- **AC-0013** Given a React/Next repo, when ingested, then Screen/Component nodes and FETCHES edges to endpoints are created.
- **AC-0014** Given a data-fetch call site, when the endpoint is resolvable, then FETCHES is Confirmed; otherwise it escalates.
- **Security:** —
- **Performance:** —
- **Trace:** M4 · `adapters-lang-ts` (tsx), `adapters-fw` · — · T-0013..0014

### US-0006 — Deterministic end-to-end flow tracer
- **Actor:** Engine
- **As a** engineer **I want** cross-layer flows traced T0-first with explicit gaps **so that** flows are trustworthy.
- **Priority:** Must · **Status:** Done
- **AC-0015** Given a trigger, when traced, then each hop resolves at the lowest possible tier and records its tier.
- **AC-0016** Given any unresolved hop, when no tier resolves it, then a Gap node truncates that branch (no silent completion).
- **AC-0017** Given a completed trace, when scored, then flow_status ∈ {Verified, Partial, Inferred} per the scoring rule.
- **AC-0083** Given a recovered WebExtension graph, when flows are traced, then extension user-action anchors trigger their own flows — every ExtensionContext, and every manifest Command routed to its dispatching context (`_execute_*` to the toolbar action, all others to background, with an explicit Gap when that context is absent) — whose hops walk ENTRY into the entry file and DEFINED_IN in reverse from that file into the symbols it defines, deterministically under input reordering, while internally published channels participate as mid-flow hops rather than triggers.
- **Security:** —
- **Performance:** Path queries bounded; long flows stream incrementally to UI.
- **Trace:** M3–M5 · `flowtracer` · F-* · T-0015..0017, T-0083

### US-0007 — Provenance and confidence on every fact
- **Actor:** Engine
- **As a** engineer **I want** every node/edge tagged with tier + confidence + evidence **so that** integrity is enforced.
- **Priority:** Must · **Status:** Done
- **AC-0018** Given any fact, when stored, then it has {tier, confidence_tier, evidence[], extractor_id, content_hash}.
- **AC-0019** Given a T2/T3 producer, when it runs, then it cannot overwrite or upgrade a T0/T1 fact (R-INT-1).
- **AC-0020** Given an agent (T3), when it acts, then it can only propose edges/annotations with cited evidence; it cannot write T0/T1 (R-INT-3).
- **AC-0061** Given the Provenance & Eval surface, when it renders, then the tier-distribution, extractor-coverage, eval-gate, and re-ingest-history sections read the shared summary/coverage/history sources (counts identical to Workspace and Gaps & Drift), every chart carries a complete textual description (`role="img"` with a descriptive label — no color-only encoding), a not-in-scope extractor reads n/a rather than 0%, and the determinism footer only claims verification when the history actually contains same-commit records with equal content hashes.
- **Security:** —
- **Performance:** —
- **Trace:** M0,M8 · `core-prov`, `agents` · — · T-0018..0020,T-0061

### US-0008 — Semantic tier with eval gating
- **Actor:** Engine
- **As a** engineer **I want** semantic matching gated by paired evals **so that** inferred links meet a precision floor.
- **Priority:** Should · **Status:** Done
- **AC-0021** Given real ingested unresolved channel or call hops, when T2 runs, then IaC-backed channel resources and repository symbols are eligible targets and proposals are marked InferredStrong with evidence.
- **AC-0022** Given a labeled eval set, when T2 is measured, then proposals below the precision floor are excluded from `best-effort` exports.
- **Security:** Embeddings computed locally by default.
- **Performance:** ANN index query sub-100ms typical.
- **Trace:** M7 · `semantic`, `llm` · — · T-0021..0022

### US-0009 — Bounded agentic tier with egress firewall
- **Actor:** Engine
- **As a** engineer **I want** agents to propose resolutions only, with per-tier cloud opt-in **so that** privacy and integrity hold.
- **Priority:** Should · **Status:** Done
- **AC-0023** Given a Local-only policy, when T3 needs a cloud provider, then it hard-fails closed (no silent egress).
- **AC-0024** Given cloud opt-in, when an agent runs, then the consent dialog shows the exact span-level payload leaving the device.
- **AC-0025** Given an agent proposal, when accepted/rejected, then the decision persists and re-applies on re-ingest.
- **AC-0055** Given a tier whose provider is switched to cloud in Settings, when standing consent has not been granted, then the full disclosure (provider, pinned model id, endpoint, per-token pricing, lane caveats) is shown before consent is recordable and no cloud call is possible; if no disclosure is available, no consent affordance is offered at all.
- **AC-0056** Given standing cloud consent, when it is revoked, the tier is disabled, or the tier's provider leaves cloud, then the derived egress policy and the status-bar egress summary immediately return to local-only.
- **AC-0087** Given the Settings surface, when it renders with a live core, then an Adapters section lists every installed adapter (language, extractor id, covered extensions, and what it extracts) from the same registry Preflight consults — inventory and coverage can never disagree — with the word "adapter" explained in place (per language/format, distinct from detected frameworks and toolchain versions), and known-but-uninstalled adapter types (JavaScript, C, C++, Kotlin, Swift, Objective-C) listed as recommendations wired to the request-adapter lane; a detected uncovered language with a planned adapter names that adapter type in its Preflight finding.
- **AC-0089** Given a gap cause class, when it is escalated as a batch, then every instance runs locally through the propose-only broker in one durable cancellable job (a failed instance records a failure, never an abort; a cancel stops at the next instance boundary), each success is an individually staged proposal accepted or rejected through the standard decision records — nothing joins the graph unaccepted — and cloud escalation remains per-instance because a consent grant binds to one exact payload hash; classes that truncate traced flows rank above larger classes that block nothing, deterministically, in both the cause lane and the by-tier tab.
- **AC-0063** Given an open gap, when strategies are requested, then the report derives from real provenance (attempted tiers, stop reason, required evidence citations, closed candidate set) with local and cloud options carrying exact redacted-payload egress estimates, the cloud option fails closed without standing T3 consent; and when an escalation runs, local executes immediately while cloud requires both standing consent and a per-payload grant whose hash matches the exact preview, the run is a durable job, egress bytes are recorded, and the result is a staged proposal that never joins the graph (accept/reject flows through the existing decision records).
- **Security:** Default-deny cloud egress; secret redaction on payloads.
- **Performance:** —
- **Trace:** M8 · `agents`, `llm`, `ingest`, `app`, `ui` · — · T-0023..0025,T-0055..0056,T-0063,T-0087,T-0089

### US-0010 — Atlas graph canvas with confidence overlay
- **Actor:** Engineer
- **As a** engineer **I want** to explore the unified graph with layer filters and tier coloring **so that** I can see what is confirmed vs inferred vs gapped.
- **Priority:** Must · **Status:** Done
- **AC-0026** Given the graph, when I filter by layer, then only that layer's nodes/edges render.
- **AC-0027** Given the confidence overlay, when active, then nodes/edges are colored by tier and Gaps are flagged.
- **AC-0028** Given a node, when selected, then the evidence panel shows file/span/commit with read-only jump-to-source.
- **AC-0062** Given the evidence drawer, when open, then it is resizable (320↔560px), its overlay never swallows clicks outside the panel, Esc closes it, the source view carries true file line numbers (windowed files report their real starting line) and the full span range including end line:col, non-Confirmed facts show a why-strip plus a collapsible why-this-tier explanation, the provenance table renders the complete 64-hex content hash with a copy affordance whose payload equals the displayed value, supporting evidence spans are navigable, a Gap offers the Resolution Strategy CTA, and the footer states the R-INT-1 integrity rule.
- **AC-0064** Given the Atlas canvas, when it renders, then node shape encodes kind independently of color (octagon with a dashed red border = Gap, diamond = gateway/channel, rounded rectangle = everything else), every edge carries a clickable mono tier+relation chip (`T0 HANDLES`, `GAP …`) that opens the evidence drawer for the edge exactly like a node, the layer filter drives the header scope chip (`Atlas · <layer>`), and a single click selects a node or edge even while the drawer is open.
- **AC-0081** Given a recovered graph, when the Atlas renders, then nodes are placed in labeled architecture bands (Infrastructure → Cloud → Server → Events → Client, Gaps anchored to a neighbor's band) at deterministic positions — identical snapshots yield identical layouts regardless of input order — and past a size threshold the initial view renders collapsed per-band clusters (keyed by repo/module) with aggregated relation counts that expand on demand; reading the graph never requires manual arrangement.
- **AC-0086** Given a selected node, when I press Enter (or activate the focus control), then the view reflows deterministically to that node's ego graph — the root, its direct connections, and the edges among them, everything else removed — with each focus stacking one navigable level; Esc (or the breadcrumb) backs out exactly one level at a time, ending at the full graph, and the focus state is keyboard-operable and announced via a live region.
- **Security:** Source view is read-only (NG1).
- **Performance:** Canvas remains interactive at 10k+ nodes (Cytoscape.js).
- **Trace:** M9 · `core-graph`, `app`, `ui` · — · T-0026..0028,T-0062,T-0064,T-0081,T-0086

### US-0011 — Flow Inspector with explicit gaps
- **Actor:** Engineer
- **As a** engineer **I want** to view a traced flow as a sequence with per-hop tier badges and explicit gap cards **so that** I trust the flow.
- **Priority:** Must · **Status:** Done
- **AC-0029** Given a trigger, when I open Flow Inspector, then the flow renders as a hop-card sequence (plus the Mermaid dossier) with tier badges, ordered by the hops' recorded src/dst ids — never by array position.
- **AC-0030** Given a Gap hop, when shown, then it appears as an "unresolved" card with reason and attempted escalation.
- **AC-0031** Given the export toggle, when set to `verified-only`, then InferredWeak hops are excluded.
- **AC-0065** Given the Flow Inspector surface, when it renders, then the header names the flow (stable id, status badge with the gap count spelled out, trigger summary, flow score), the projection note makes the verified-only/best-effort difference explicit (hidden count; excluded InferredWeak hops stay visible as annotated non-interactive cards; the Gap node is always retained per R-INT-4), and the hop sequence wraps responsively with Fit/zoom driving real layout widths — never a CSS transform — so the row never scrolls horizontally.
- **AC-0066** Given a hop card, when clicked, then a hop ending at a Gap node opens the Resolution Strategy modal for that gap, and every other hop — including fail-closed hops with unrecognized confidence, which have no strategy to run — opens the read-only evidence drawer from the hop's own provenance.
- **AC-0084** Given a target where zero flows trace, when the Flow Inspector renders, then the empty state names every anchor kind recovery sought (screens, extension contexts, extension commands, HTTP endpoints, externally published channels) with the count found for each — never a generic ingest hint.
- **Security:** —
- **Performance:** —
- **Trace:** M9 · `app`, `flowtracer`, UI · F-* · T-0029..0031, T-0065..0066, T-0084

### US-0012 — Spec Workbench, curation, and export
- **Actor:** Engineer
- **As a** engineer **I want** to review compiled artifacts, accept/reject inferred items, and export **so that** I get an official, trustworthy spec.
- **Priority:** Must · **Status:** Done
- **AC-0032** Given compiled artifacts, when viewed, then every assertion shows inline provenance.
- **AC-0033** Given an inferred item, when I accept/reject/annotate, then the decision persists and survives re-ingest via content_hash.
- **AC-0034** Given export, when run, then it honors R-INT-5 (`verified-only` vs `best-effort`) and includes Gap + Drift registers.
- **AC-0035** Given the full set, when exported, then it produces user stories, US-TM, flow dossiers, resource topology as Markdown with fenced Mermaid plus provenance, a data model retaining READS/WRITES/MAPS_TO relations that terminate at DataEntity, ADRs, Gap + Drift registers, and mapped security findings.
- **AC-0057** Given a completed recovery, when the Workspace landing renders, then the outcome tally (open findings split into gaps, unsupported patterns, and no-evidence) and every provenance-health count derive from the single register summary and atlas projection all surfaces share — findings are listed explicitly, never guessed or double-counted.
- **AC-0058** Given the artifacts grid, when an artifact was generated, then it carries two independent badges — generation ("Artifact generated") and recovery authority (authoritative/partial/inferred) — while the gap register carries exactly one completion-style badge ("N open findings") and a missing artifact is labeled "Not generated" rather than omitted.
- **AC-0067** Given the Spec Workbench, when a Confirmed (deterministic) assertion renders, then its Accept/Reject/Annotate controls are visible but truly disabled — `disabled` plus `aria-disabled="true"`, never hidden and never exposed as enabled in the a11y tree — with an inline "Confirmed T0 — locked, read-only" explanation and a document-level note stating the R-INT-1 rule; only proposed T2/T3 assertions are curatable, Gap assertions offer no curation at all, and each doc-list entry carries a per-document count chip (US/AC/flows/ADR units, alert tone for non-empty registers).
- **Security:** —
- **Performance:** —
- **Trace:** M9–M10 · `spec`, UI · — · T-0032..0035,T-0057..0058,T-0067

### US-0013 — ADR recovery and drift detection
- **Actor:** Engine
- **As a** engineer **I want** found and recovered ADRs linked to governed targets, with conflicts surfaced **so that** decisions and drift are visible.
- **Priority:** Should · **Status:** Done
- **AC-0036** Given existing Markdown ADR/RFC files, when parsed or re-ingested, then explicit `Governs:` or exact backtick target ids link to existing full-system graph targets as Confirmed facts, including targets from another repo, while removed declarations and deleted ADR files remove their prior links and found nodes.
- **AC-0037** Given evidence-backed channel architecture, when recovered ADRs are drafted, then they are marked recovered/inferred, cite the producing graph evidence, remain curatable, and cannot survive rejection of their supporting facts.
- **AC-0038** Given a found ADR with an explicit `Forbids:` edge constraint, when governed code conflicts, then a confidence-preserving finding appears in the Drift register mapped to the offending edge and any containing flow, unless that supporting edge was rejected.
- **AC-0059** Given the Gaps & Drift register surface, when it renders, then System Gaps, unsupported patterns, and no-evidence findings appear in three distinct lanes that never conflate (an unsupported item is a tool limitation, never a Gap), the header tally quotes the same register summary Workspace quotes, gaps group by their next escalation tier, and drift findings list under their own tab — all wired from the spec compiler's registers, never re-derived in the UI.
- **AC-0088** Given the load-bearing vocabulary wherever it renders (System-gap, unsupported-pattern, and no-evidence lanes; the Workspace outcome and artifact-authority badges; the Atlas confidence legend; the Flow Inspector projection toggle), when a user reaches the term, then a keyboard-accessible in-place help affordance explains it within one interaction (one Tab and one Enter; Esc dismisses) with a Learn-more deep link, and every note renders from the single-sourced help-notes module — never a per-surface copy that could drift.
- **AC-0082** Given a register with more gaps than a screenful, when the System-gaps lane renders, then gaps group into cause classes (stop reason × extractor) ordered deterministically by instance count, each class expands on demand to paged instance rows that stay responsive at thousands of gaps, and every instance keeps its Resolution Strategy path — the lane reads as a ranked handful of causes, never a wall of rows.
- **Security:** —
- **Performance:** —
- **Trace:** M9 · `spec`, `app` · — · T-0036..0038,T-0059,T-0082

### US-0014 — Determinism and re-ingest idempotency
- **Actor:** Engine
- **As a** engineer **I want** re-ingesting the same commit to yield an identical graph **so that** outputs are reproducible.
- **Priority:** Must · **Status:** Done
- **AC-0039** Given the same commit and inputs, when re-ingested, then the ordered T0 node/edge identity plus content-hash snapshot is identical and unchanged graph facts are not rewritten.
- **AC-0040** Given a changed or deleted TS/TSX/Terraform file, when re-ingested in the same process, then only new or byte-changed per-file extraction contexts are reparsed, unchanged contexts are reused by source hash, deterministic repository-wide joins are refreshed, and stale facts are removed.
- **AC-0060** Given any completed ingest, when it finishes, then a history record persists its tier tallies (counted with the register's own provenance definition), unsupported/no-evidence counts, per-extractor coverage (files in scope, distinct files with facts — a covering adapter with zero facts is a 0% row, never a missing row), and an order-independent whole-graph content hash, so re-ingesting the same commit shows identical hashes in queryable history — determinism observable as data, not only asserted in tests.
- **Security:** —
- **Performance:** Delta re-ingest scales with change size, not repo size.
- **Trace:** M10 · `adapters-lang-ts`, `iac`, `core-graph`, `core-prov`, `app` · — · T-0039..0040,T-0060

### US-0015 — Security view of the spec
- **Actor:** Engineer
- **As a** engineer **I want** auth edges and IAM grants projected into a security view **so that** unauthenticated endpoints and over-broad policies surface as findings.
- **Priority:** Should · **Status:** Done
- **AC-0041** Given an endpoint with an explicit negative auth fact, when projected, then it is listed as a cited security finding mapped to US-0015/AC-0041; missing auth evidence alone never asserts unauthenticated status.
- **AC-0042** Given an IAM `GRANTS` edge with a wildcard action or literal wildcard resource scope, when analyzed, then a confidence-preserving finding lists the exact actions and resource scope and maps to US-0015/AC-0042.
- **Security:** This story *is* the security projection.
- **Performance:** —
- **Trace:** M9 · `iac`, `spec`, `ui` · — · T-0041..0042

### US-0016 — Recover WebExtension systems end to end
- **Actor:** Engineer
- **As a** engineer **I want** Cartograph to recover a browser extension's entry points, message topology, persisted data, permissions, capabilities, and flows **so that** a real TypeScript WebExtension such as Image Trail compiles into a useful official specification instead of a file-only graph.
- **Priority:** Must · **Status:** In-Progress
- _(Issue #99 drafted these as AC-0055..0059 before that range was assigned; renumbered here.)_
- **AC-0071** Given a Manifest V2/V3 browser extension, when it is ingested, then manifest-declared service workers/background scripts, content scripts, extension pages, toolbar actions, commands, permissions, host permissions, and externally connectable boundaries become deterministic provenance-tagged topology facts (Extension/ExtensionContext/Command nodes with DECLARES/ENTRY edges) and exact-scope GRANTS security facts; declared entry files bind to their `.ts` sources when the built `.js` is absent, a missing entry is an explicit Gap, and the ingest summary reports the WebExtension layer separately.
- **AC-0072** Given import-proven or global Chrome runtime messaging plus explicit message definitions and handler registrations, when the repository is ingested, then deterministic channels and PUBLISHES/SUBSCRIBES edges connect extension contexts; unresolved or dynamic message identities remain explicit Gaps.
- **AC-0073** Given explicit IndexedDB schema/store declarations and repository operations, when ingested, then DataEntity nodes and READS/WRITES relations produce a cited data model.
- **AC-0074** Given recovered WebExtension entry points, message flows, data entities, found ADRs, and permission facts, when the official spec is compiled, then the artifact set contains useful cited assertions and the verified-only output is deterministic across repeat ingest of the same commit.
- **AC-0075** Given an ingest completes with partial or unsupported coverage, when the engineer reviews the result, then the recovery workspace presents artifact readiness, coverage diagnostics, explicit unsupported-pattern guidance, Atlas/Flow/Spec navigation, and evidence context instead of presenting a file graph as a complete analysis.
- **Security:** All new facts are T0 Deterministic/Confirmed or explicit Gap; target code stays read-only; manifest host and API permissions retain their exact scopes.
- **Performance:** Re-ingest uses the existing content-hash cache and determinism guarantees.
- **Trace:** post-M10 dogfood · `adapters-lang-ts`, `adapters-fw`, `events`, `spec`, `app`, UI · WebExtension flows · T-0071..0075

### US-0017 — Runtime-loadable adapter plugins
- **Actor:** Engineer
- **As a** engineer **I want** to install a gated adapter plugin while Cartograph is running **so that** an unsupported language or event system becomes a recovered layer instead of a permanent finding.
- **Priority:** Should · **Status:** Done
- **AC-0068** Given a WASM adapter artifact in a discovery directory (project-local `.cartograph/adapters/` beating user-level on id conflict), when Cartograph loads it, then it activates only after a durable conformance-gate job passes SPI contract tests, the generator-supplied golden corpus (expected facts with exact evidence spans), and a double-run determinism check; a failed or ungated adapter stays in the proposed state under the standard propose/accept curation semantics.
- **AC-0069** Given facts emitted by a plugin adapter, when inspected, then their provenance carries `extractor_id@version` plus the plugin artifact's content hash, and re-ingesting the same commit with the same adapter set yields an identical whole-graph content hash (the US-0014 determinism invariant extends to plugin facts).
- **AC-0070** Given a plugin at extraction time, when it attempts network access, a filesystem write, or exceeds its fuel/memory bounds, then the run fails closed with an explicit finding and no partial facts join the graph; source access is read-only and host-mediated, and no LLM is ever invoked inside a T0 plugin.
- **AC-0093** Given a Preflight unsupported finding for an uncovered language, when the engineer follows its request-adapter action and a generated artifact lands in a discovery directory, passes the conformance gate, and is enabled for the project, then the next scan reports the language covered by the plugin (the originating unsupported finding closes), extraction routes the files the plugin's golden corpus claims through the plugin with host-pinned facts, and compiled-in adapters always win a contested extension.
- **Security:** AI-authored code runs sandboxed (no network, no writes, bounded resources) per ADR-0017; fail closed.
- **Performance:** Plugin extraction may trail native adapters; first-class languages stay compiled in.
- **Trace:** post-M10 · `adapters-*`, `ingest`, `app`, UI · — · T-0068..0070, T-0093

### US-0018 — In-app Help, single-sourced with the wiki
- **Actor:** Engineer
- **As a** engineer **I want** help inside the app — a native Help menu, an in-app Help view, and contextual per-surface topics — **so that** guidance is reachable without leaving the work, and never drifts from the published wiki.
- **Priority:** Should · **Status:** Done
- **AC-0090** Given the native app menu, when it renders, then a Help submenu offers Cartograph Help (opens the in-app Help view), User guide — wiki (opens the wiki in the system browser, never the webview), Report an issue (the tracker's new-issue page, system browser), and About Cartograph with the app version.
- **AC-0091** Given the in-app Help view, when opened — from the menu, the command palette, or the `?`/F1 shortcut — then it renders a keyboard-navigable topic TOC (concepts plus every surface) and the selected topic's content entirely from markdown bundled at build time, offline, with no network dependency.
- **AC-0092** Given any surface, when its contextual help entry (the header's Help action) is used, then the Help view opens on that surface's topic; topics are authored once under `docs/help/` and mirrored to the wiki, and the CI drift check fails when repo content and wiki diverge.
- **Security:** External links open via the system browser; help content ships in the bundle (no runtime fetch).
- **Performance:** Topics render from pre-bundled strings; no I/O on open.
- **Trace:** post-M10 · `app`, UI, `scripts` · — · T-0090..0092
