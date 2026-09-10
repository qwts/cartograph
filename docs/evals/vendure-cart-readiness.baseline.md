# Vendure cart-readiness baseline

**Result: 0/5 core guards + 0/3 supporting rules = 0/8 complete confirmed rule
matches. H2 is not passed.** Cartograph recovered six relevant local exit anchors.
That is partial source evidence, not semantic rule recall.

The source-only run used 17 explicitly selected production files from Vendure
commit `2cc309deb5394af176314552ac7320a7b26a0104`. No upstream tests, expected
labels, target installation, target execution, model calls, or cloud providers
were used by extraction. This is an adapter/spec projection baseline, not the
full desktop ingestion pipeline or a market-readiness test.

## Revisions and evidence

- Implementation: `a61cb669f0a36e173adaf76741ccd266a2880c99`.
- Initial independently scored implementation: `c01c2e77b07d3a39b010cc5284972a3feed4b897`.
  The final revision reproduced identical graph, bundle, metadata and inventory
  bytes after the quoted-key and malformed-parser fixes.
- Review follow-up: `c40a9b869a36fb15be5e14cc7e148d8a84a704f8` adds explicit
  computed-name disclosure. Two further runs reproduced all four scored artifacts
  byte-for-byte; all six runs agree. This does not execute the 41 decision cases
  or change the zero-of-eight complete-rule result.
- [Frozen expected set](vendure-cart-readiness.expected.json): five core rules,
  three supporting rules, 41 decision cases and ten negative claims. SHA-256:
  `dcd9833e8f9be2269a6711f8bd04119c83931f6de45e04d17d1b99da9d51295d`.
- [Input manifest](vendure-cart-readiness.input.json), [run record](vendure-cart-readiness.baseline.json),
  and [independent score](vendure-cart-readiness.score.json) retain source/output
  hashes, exact observation IDs/spans, audit scope and remaining gaps.
- Graph SHA-256: `134db5e4f52d337cb1208856b0fe58f985f44296681bc55feb86bfb427b46a08`.
  Snapshot: `context-v1:6543894b0f31ef8683c42b7f4da5f476623aa5e4ad41fab9c5edcf3b538b31d8`.

## What was recovered

The selected files produced 139 guarded-exit observations, 1,675 graph nodes and
1,983 edges. Each observation explicitly leaves complete execution predicates
and consumer effects unestablished. The inventory is 1,062,848 bytes; it is a
source-evidence listing, not a curated cart-readiness domain view.

| Expected rule | Relevant local evidence | Still needed for a complete match |
|---|---|---|
| VEN-READY-01: variants available | Option/state guards and the count-mismatch exit | Distinct-ID and deletion-query semantics, default configuration, precedence and rejection consumer |
| VEN-READY-02: contents required | Payment-entry, option and empty-lines syntax | Configuration linkage, reachability and rejection consumer |
| VEN-READY-03: customer required | Payment-entry, option and customer truthiness | Configuration linkage, reachability and rejection consumer |
| VEN-READY-04: shipping required | Payment-entry, option and missing/empty shipping syntax | Configuration linkage, reachability and rejection consumer |
| VEN-READY-05: sufficient stock | Option and accumulated-array length at a local exit | Per-line comparison, accumulator behavior, affected variants, stock semantics and consumer; the return expression is explicitly unsupported |
| VEN-READY-06: tracking disabled | `inventoryNotTracked`, its declaration reference and `Number.MAX_SAFE_INTEGER` | Expanded FALSE/INHERIT/global-setting predicate and dependency semantics |
| VEN-READY-07: saleable arithmetic | Service symbols and stock/settings call edges | Unguarded arithmetic, threshold selection and dependency interpretation; no rule observation at this return |
| VEN-READY-08: active-channel totals | Strategy symbol and membership-helper call | Channel membership, summation and configured-strategy closure; no rule observation at this return |

The independent agent review checked the six relevant exit/condition spans and
owner edges against pinned source and found no false local syntax claim in that
subset. The other 133 observations were not fully audited. Full-rule precision is
not estimable with zero complete claims; it must not be reported as 100%.

All 41 decision cases remain **unexecuted**. The source oracle was reviewed before
output existed; this was agent review, not human approval or an external audit.

## Input and product limits

Five production dependencies cited by the frozen oracle are outside this initial
input: `packages/common/src/unique.ts`, the RequestContext implementation, the
default stock-location base strategy, GlobalSettings entity and ProductVariant
entity. Their exact paths are in the score. Keep these input limits separate from
the extractor's unimplemented semantics; the eight-rule denominator is unchanged.

Identical runs produced byte-for-byte identical outputs. The first direct debug
binary run took 6.25 seconds locally; timings are observations, not a performance
gate or a prediction for full repositories. The run record retains subsequent
revision checks. Peak memory and desktop responsiveness were not measured.

The next recovery increment needs generic, cited def-use and configured-consumer
proof, followed by query and collection/arithmetic interpretation. Source naming,
domain curation, durable specialists, MCP/ACP execution and an independently
completed user pilot remain separate SPEC-01 gates.

## Reproduce

Use the implementation revision above and a local checkout containing the pinned
Vendure commit. The staging helper reads exact Git blobs and excludes test files;
the expected manifest is not an extraction input. Choose fresh output paths.

```sh
python3 scripts/stage-source-eval.py "$TASK_VENDURE_CHECKOUT" \
  docs/evals/vendure-cart-readiness.input.json /tmp/cartograph-rule-input
cargo run -p spec --example recover_source_rules -- \
  /tmp/cartograph-rule-input vendurehq/vendure \
  2cc309deb5394af176314552ac7320a7b26a0104 /tmp/cartograph-rule-run-1
cargo run -p spec --example recover_source_rules -- \
  /tmp/cartograph-rule-input vendurehq/vendure \
  2cc309deb5394af176314552ac7320a7b26a0104 /tmp/cartograph-rule-run-2
cmp /tmp/cartograph-rule-run-1/graph.json /tmp/cartograph-rule-run-2/graph.json
cmp /tmp/cartograph-rule-run-1/bundle.json /tmp/cartograph-rule-run-2/bundle.json
```

The metadata's input-file hashes and context snapshot use BLAKE3 through
`core-prov`; staging proofs and artifact records explicitly use SHA-256.
