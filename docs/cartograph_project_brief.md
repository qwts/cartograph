# Project Brief: Cartograph — Cross-Layer Spec-Recovery Engine

## 1. Overview
Cartograph is a developer-focused desktop application designed to solve the "documentation rot" problem by automatically recovering a system's true specification from its source code, infrastructure declarations, and cloud topology. 

Unlike static analysis tools that focus on a single language or LLM-based tools that risk hallucination, Cartograph uses a **four-tier escalation ladder** to build a unified, provenance-tagged knowledge graph across five layers: Infrastructure, Cloud, Server, Events, and Client.

## 2. Core Principles
*   **Fidelity First:** Prefer an explicit **Gap** node over a hallucinated or unverified connection.
*   **Provenance:** Every node and edge in the graph carries metadata identifying its source (file, line, commit) and its producing tier (Deterministic, Dynamic, Semantic, or Agentic).
*   **Human-in-the-Loop:** The engine proposes; the engineer curates. Decisions to accept or reject inferred links are persistent.
*   **Local-First & Private:** Analysis happens on-device using Rust and tree-sitter. Cloud LLMs are opt-in only.

## 3. The Escalation Ladder
| Tier | Method | Confidence |
| :--- | :--- | :--- |
| **T0: Deterministic** | Static parse (tree-sitter), IaC HCL/AST graph. | Confirmed |
| **T1: Dynamic** | Observed evidence (Terraform state, OTel traces, test logs). | Confirmed (Observed) |
| **T2: Semantic** | Local embeddings, similarity matching, name/contract clustering. | InferredStrong |
| **T3: Agentic** | Bounded LLM agents proposing resolutions with cited evidence. | InferredWeak |

## 4. Key Features & Views
### 4.1 Workspace Dashboard
*   Management of repository sets (GitHub App/PAT auth).
*   System topology declaration (`cartograph.system.toml`).
*   Ingest progress monitoring with granular status for each analysis tier.

### 4.2 Atlas (The Unified Graph)
*   Interactive Cytoscape.js canvas visualizing the entire system.
*   Layer filters (Infra, Cloud, Server, Events, Client).
*   **Evidence Panel:** A read-only code viewer providing instant "jump-to-source" for any fact.

### 4.3 Flow Inspector
*   End-to-end business flow tracing starting from user triggers (Screens/Actions).
*   Horizontal sequence diagrams with tier badges for every hop.
*   Explicit **Gap Nodes** where the tracer cannot establish a link.

### 4.4 Spec Workbench
*   Curation surface for generated artifacts (User Stories, ADRs, Data Models).
*   **Gap & Drift Registers:** Tables identifying unresolved hops and conflicts between documentation and code.
*   "Accept / Reject / Annotate" workflow for all inferred proposals.

## 5. Technical Stack
*   **Shell:** Tauri 2 (Rust core + React/TS/Vite UI).
*   **Graph Store:** Kuzu (Embedded Graph) + SQLite (Relational Spine).
*   **Parsing:** tree-sitter (Multi-language incremental parsing).
*   **LLM:** Ollama (Local default) with pluggable provider interface.

## 6. Target Audience
Software Architects, Lead Engineers, and Security Auditors who need a trustworthy, reproducible, and auditable map of complex, multi-repo cloud systems.
