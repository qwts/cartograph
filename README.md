# Cartograph

**Cross-layer spec-recovery engine.** Cartograph ingests the repos that make up a
running system and recovers its true specification — business flows, business
rules, ADRs, data model — as a unified, provenance-tagged knowledge graph across
five layers: infrastructure, cloud, server, events, and client.

The core thesis is a **four-tier escalation ladder**: every fact is produced by
the lowest tier that can establish it, and carries provenance + a confidence
tier. The engine prefers an explicit **Gap** over an unsupported assertion.

| Tier | Method | Confidence |
|---|---|---|
| T0 Deterministic | Static parse (tree-sitter), IaC/HCL graph, framework adapters | Confirmed |
| T1 Dynamic | Observed evidence (Terraform state, OTel traces, test runs) | Confirmed (observed) |
| T2 Semantic | Local embeddings, similarity/contract matching | InferredStrong |
| T3 Agentic | Bounded LLM agents proposing links with cited evidence | InferredWeak |

Deliverable: an **official specification** (user stories + acceptance criteria,
ADRs, flow dossiers, traceability matrix, gap & drift registers) trustworthy
enough that a third party could re-specify the system from the document alone.

## Status

M0 (skeleton + stores): Tauri 2 shell boots, SQLite graph store and durable
job spine round-trip. Next: M1 deterministic TS extraction. See the
[milestone plan](docs/SPEC-00_master.md#14-milestone-plan-m0m10).

```sh
# prerequisites: Node (see .nvmrc) and Rust via rustup — https://rustup.rs
# (rust-toolchain.toml pins the exact toolchain; rustup picks it up automatically)
npm install && npm --prefix ui install
npm run tauri dev   # first run compiles the Rust workspace — takes a few minutes
```

## Documentation map

| Document | Purpose |
|---|---|
| [docs/SPEC-00_master.md](docs/SPEC-00_master.md) | Master specification — single source of truth |
| [docs/cartograph_project_brief.md](docs/cartograph_project_brief.md) | Short project brief |
| [docs/user_stories.md](docs/user_stories.md) | User stories + acceptance criteria (US/AC schema) |
| [docs/US-TM.md](docs/US-TM.md) | Traceability matrix: US ↔ AC ↔ crate ↔ milestone ↔ ADR ↔ test |
| [docs/adr/](docs/adr/) | Architecture decision records |
| [docs/design/](docs/design/) | Design tokens + Stitch UI mockups |

Work is tracked in [GitHub Issues + Project](https://github.com/qwtm/cartograph/issues);
`docs/archive/tracker.csv` is the pre-import snapshot and is not maintained.

## Stack (decided — see ADRs)

Tauri 2 · Rust core · React + TypeScript + Vite UI · tree-sitter ·
SQLite/WAL graph store with recursive-CTE traversal (ADR-0008) + state spine ·
Ollama local-first LLM with pluggable cloud providers (opt-in egress).

## Contributing

This repo runs a strict, agent-first SDLC: every change lands through a gated
PR traceable to a user story / acceptance criterion. See
[CONTRIBUTING.md](CONTRIBUTING.md) and `AGENTS.md` for the workflow.
