# ADR-0022 — Immutable source captures before producer verification

- **Status:** Accepted for the staged core implementation in #387
- **Date:** 2026-09-10
- **Deciders:** Cartograph owner direction; implementation agent within SPEC-01

## Context

Proposal staging records the host-supplied task text but cannot prove those bytes
produced the recovered facts. Live checkout reads can drift from both an earlier
parse and a named Git commit. Configurations and other supporting inputs also
affect recovery. A metadata-only digest cannot supply unavailable original bytes.

## Decision

Introduce a bounded local source-capture core with explicit selected membership,
immutable raw buffers, distinct working-tree and exact Git-object acquisition,
canonical manifests and atomic local byte retention. Later reads validate the
complete capture/file reference and slice the verified buffer with strict text
decoding. Keep capture references separate from legacy provenance and staged
proposal wire formats until each producer explicitly consumes captured inputs.

Implement acquisition/storage first, with a real TS parser integration fixture.
Do not attach a verified flag to existing facts or globally certify an extraction
because a source path happens to occur in a capture. Production adoption remains
#385, with complete supporting-input accounting and reference-aware retention.

## Consequences

Restart and checkout mutation cannot silently substitute bytes for retained
captures. Raw byte identity, availability, producing confidence and human review
remain separate. A bounded private raw-source store adds local retention and disk
overhead, so selection is explicit and capacity failures preserve existing data.
The first core has no eviction or production UI; adoption requires storage access,
ownership and deletion policy. Metadata is not retroactive proof of a parse, and
captured historical bytes are not proof of current curated-context eligibility.

## Alternatives

1. Hash files when an agent asks: rejected because the earlier parser may have
   consumed different bytes and a digest alone cannot reproduce unavailable input.
2. Trust the named Git commit: rejected because current adapters parse checkout
   bytes, while filters and line endings may differ from the blob.
3. Materialize a snapshot tree and certify all extraction: deferred because every
   producer and auxiliary read must first prove which immutable inputs it used.
