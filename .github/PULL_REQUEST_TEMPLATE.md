## Summary

<!-- What changed and why, in a sentence or two. -->

## Traceability

- **Issue:** Closes #
- **US / AC:** <!-- e.g. US-0002 / AC-0004..0006, or "process — no US" -->
- **ADR impact:** <!-- new ADR-XXXX / amends ADR-XXXX / none -->

## Release intent

- [ ] Added a Changeset for shipped behavior/fixes.
- [ ] No Changeset: docs, tests, or internal tooling only.

## Checklist

- [ ] Docs updated in this PR if behavior or decisions changed (`user_stories.md`, `US-TM.md`, ADR)
- [ ] `node scripts/check-traceability.mjs && npm run version:check && npm run test:version`
- [ ] `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
- [ ] `npm --prefix ui run lint && npm --prefix ui run typecheck && npm --prefix ui run test && npm --prefix ui run build`
- [ ] No unrelated changes bundled in
