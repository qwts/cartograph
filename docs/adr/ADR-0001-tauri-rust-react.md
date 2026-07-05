# ADR-0001 — Tauri 2 + Rust core + React web UI

- **Status:** Accepted
- **Date:** 2026-06-21
- **Deciders:** Chris Kane

## Context
Cross-platform (macOS primary, Windows) desktop app whose value is a deterministic
parse/graph core plus a rich graph/flow visualization surface.

## Decision
Use **Tauri 2** with a **Rust** core and a **React + TypeScript (Vite)** web UI.
MVC mapping: Rust = Model + Controller (commands/jobs); React = View. UI state in Zustand.

## Consequences
- Small footprint vs Electron; Rust core matches the deterministic-first ethos and
  gives a real systems language for parsing and graph traversal.
- Web UI unlocks Cytoscape.js / React Flow / Mermaid for the canvas and flow views.
- Cost: Rust↔webview bridge discipline (no blocking the webview thread); durable jobs.

## Alternatives (≤3)
- **Electron** — heavier runtime, JS core fights the deterministic ethos.
- **Wails (Go)** — viable, but Rust's tree-sitter/Tauri ecosystem is stronger here.
