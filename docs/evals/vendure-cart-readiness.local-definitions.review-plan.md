# Vendure local-definition review plan

Frozen before new #398 extraction output. This plan supplements SPEC-08 and does
not alter the existing oracle or baseline.

Use commit `2cc309deb5394af176314552ac7320a7b26a0104` and the existing 17-file
input manifest. Confirm all input hashes and membership before runs. Keep expected
labels, this review plan, upstream tests and recovered output outside extraction
input. Target code is neither modified nor executed in this source-only assessment.

Review the initializer of `inventoryNotTracked` and each admitted definition/use
against original source spans, lexical scope, exact operators and unresolved
property/configuration dependencies. Inspect generic behavior separately through
renamed fixtures; no target names belong to extraction logic.

Negative checks include `variantsWithInsufficientSaleableStock = []` followed by
mutation, awaited/destructured settings, enum-property values, unguarded arithmetic
and configured consumers. Initializer syntax must not certify later values,
rejection meaning, complete stock arithmetic or an absent business feature.

Produce separate local-definitions baseline JSON, score JSON and score Markdown.
Record implementation revision, unchanged oracle/input/target hashes, artifact
sizes/hashes, two-run byte comparisons, source-span audit results, gaps, unexpected
claims and reviewer limits. Preserve all earlier artifacts. Report local evidence
improvements separately from the fixed 0/8 existing complete-rule baseline and
41 unexecuted cases; any changed score requires the frozen oracle's full proof.

The ordinary extraction example is an observation benchmark. Captured producer
receipt/source integrity is verified separately by integration tests, not by
matching source files retrospectively.
