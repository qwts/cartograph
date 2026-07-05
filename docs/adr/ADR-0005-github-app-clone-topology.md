# ADR-0005 — GitHub App auth + git2 clone + system topology manifest

- **Status:** Accepted
- **Date:** 2026-06-21
- **Deciders:** Chris Kane

## Context
Need read-only access to 1..N repos and a way to declare which repos form one system.

## Decision
Auth via **GitHub App** (installation token) preferred, **PAT** fallback, optional `gh`
CLI shell-out. Clone via **git2/libgit2** (shallow/sparse). Metadata/history via
**octocrab** (feeds T1 evidence + ADR timeline). System composition is declared in an
author-editable `cartograph.system.toml` (repo set, layer hints, config/env locations,
known channel identities); T2 may suggest additions but the user confirms.

## Consequences
- Least-privilege, read-only scopes; tokens in OS keychain.
- Manifest is the source of truth for topology; inference is suggestion-only.

## Alternatives (≤3)
- **PAT-only** — simpler, broader scopes, weaker security posture.
- **gh CLI as primary** — convenient where pre-authed, brittle as a dependency.
