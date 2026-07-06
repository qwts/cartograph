# ADR-0009 — v1 GitHub auth ladder: env token → gh CLI; GitHub App deferred

- **Status:** Accepted
- **Date:** 2026-07-06
- **Deciders:** Chris Kane (proposed by Claude at US-0001 slice 1)

## Context
SPEC-00 §10 prefers GitHub App installation tokens with PAT fallback and an
optional `gh` CLI shell-out. US-0001 (AC-0001/AC-0003) needs working auth
now, for a single-user local desktop app (NG5: no multi-user backend in v1)
whose only current user already has `gh` authenticated. A GitHub App brings
app registration, private-key management, JWT minting, and installation
token exchange — infrastructure that pays off for distribution, not for
dogfooding. The SPEC security note also calls for OS-keychain token storage,
which presumes a settings surface the shell does not have yet.

## Decision
v1 resolves credentials through a deterministic ladder, first hit wins:

1. `GH_TOKEN`, then `GITHUB_TOKEN` (environment)
2. `gh auth token` shell-out (environments already authenticated)
3. anonymous (public repos still clone)

Tokens are used only as in-memory clone credentials
(`x-access-token:<token>`), never logged and never persisted by Cartograph.
Auth-shaped clone failures map to a typed error carrying remediation text
(AC-0003); failed clones leave no partial directory (temp dir + atomic
rename).

## Consequences
- AC-0001/AC-0003 are implementable and testable offline today; the ladder
  costs ~40 lines and no new secrets management.
- OS-keychain storage (via e.g. `keyring`) lands when the shell gains a
  settings view; the ladder gains a rung, nothing is displaced.
- GitHub App auth remains the SPEC §10 end-state for distribution;
  revisit when Cartograph is used by anyone whose machine lacks `gh`.

## Alternatives
1. **GitHub App now** — most secure and SPEC-preferred end-state; rejected
   for v1: registration + key custody + token exchange serve zero current
   users, and NG5 caps v1 at single-user.
2. **Keychain-stored PAT with a settings UI** — right long-term home for a
   token; rejected as a blocker: no settings surface exists yet and the env
   + `gh` rungs cover the dogfood machine.
3. **`gh` CLI for the clone itself (`gh repo clone`)** — least code;
   rejected: loses typed error classification (AC-0003 remediation), adds a
   hard runtime dependency on `gh`, and forfeits git2's progress hooks
   needed for SPEC §10's bounded progress feedback later.
