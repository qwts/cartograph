# Vendure local-definition assessment

**Complete-rule score remains 0/8; all 41 decision cases remain unexecuted.** The new output adds initializer-as-written evidence without changing the original conditions or returns. Acceptance is not achieved.

The bounded MT-H2-03 assessment passes at implementation `3b146ce0425648b447c477e3a4e8f67bc6673d8a`. Two fresh extraction runs and both source audits agree byte for byte. Their output also matches the independently reviewed preliminary graph. No target code was executed or modified.

## Input and review boundary

Target: `vendurehq/vendure` at `2cc309deb5394af176314552ac7320a7b26a0104`. All 17 input files have exact membership and match both the frozen file hashes and retained checkout bytes. Oracle SHA-256: `dcd9833e8f9be2269a6711f8bd04119c83931f6de45e04d17d1b99da9d51295d`. Input-manifest SHA-256: `96d2595907c4b1ea05991c7f0f2460df2f7b75267c486d5ee3e96c3d6ae49b83`. The oracle and earlier baseline remain unchanged; dependency closure remains incomplete.

The reviewer used TypeScript 6.0.3 syntax trees and lexical binding separately from the tree-sitter producer. This is an agent audit; the reviewer contributed generic projection/tests and is not an independent human approver. The parser did not execute target modules or load imported dependencies.

## Local evidence gained

| Measure | Result |
|---|---:|
| Guarded exits | 139, unchanged |
| Observations with definitions | 101 |
| Definition records / distinct bindings | 117 / 99 |
| Admitted use occurrences | 135 |
| Arena nodes / initializer dependencies | 250 / 70 |
| New citation occurrences checked | 723 |
| Complete-syntax displays matching original bytes | 339 |
| Redacted / unsupported expression occurrences | 2 / 26 |
| Source or structure mismatches found | 0 |

All 135 admitted uses resolve to the recorded declaration and meet lexical block/statement order. All 250 arena nodes match original operators, child spans and member flags. All 34 Binding dependencies match original-site declarations, including containing declaration citations for destructuring. Explicit gap links exist. These counts repeat reused definitions across observations; they are not complete-rule recall or runtime-value accuracy.

All 139 original rule IDs, conditions, effects, exit spans, owners and source orders match the old baseline. All retain T0/Confirmed authority for local source observations, with execution predicate and consumer effect `not_established`. The exact initializer qualification appears 139 times.

## VEN-READY-06: inventory-not-tracked initializer

Owner: `sym:vendurehq/vendure@packages/core/src/service/services/product-variant.service.ts#ProductVariantService.getSaleableStockLevel`. Declaration bytes `13652..13820`, initializer `13686..13820`, admitted use `13834..13853`, and exit `13869..13900` match `product-variant.service.ts`.

```ts
variant.trackInventory === GlobalFlag.FALSE ||
            (variant.trackInventory === GlobalFlag.INHERIT && trackInventory === false)
```

The 20-node arena preserves `||`, parentheses, `&&`, strict `===`, member reads and literal `false`. The guard remains `(inventoryNotTracked)` and the return remains `Number.MAX_SAFE_INTEGER`. Its nine dependencies cite the original parameter, import and destructured settings binding. Member values and global settings remain unresolved; enum-property values, numeric sentinel meaning, complete reachability and configured consumer effects are not established. **This improves local evidence and still does not complete VEN-READY-06.**

## Negative checks

- The stock collection declaration at `default-order-process.ts:13412..13472` retains `[]` only as an Unsupported initializer with an explicit gap. Source shows later `.push` inside a loop. Mutation/loop gaps remain, and the guard stays `(variantsWithInsufficientSaleableStock.length)`; no zero-length or constant outcome is claimed.
- Awaited/destructured settings have binding citations and limitations, without runtime-value or configuration interpretation. Enum properties remain unresolved member reads.
- No rule was added for the unguarded stock arithmetic return at `product-variant.service.ts:14227..14294`. This absence is not a claim that the business feature is absent.
- All 139 observations retain unknown execution predicates and consumer effects. Configured composition, first-rejection precedence and external consumer actions remain outside this result.

## Reproducibility and limits

| Final artifact | Bytes | SHA-256 |
|---|---:|---|
| bundle.json | 6481417 | `5300d6e9ae5fd1d5dad77f410adfd60f2d0b554ec3bdab8a203882a95b6f2384` |
| graph.json | 3927045 | `76b1fa5731bb70b03fb67613a00a5760f68206dec225964c99830c7c79405e56` |
| metadata.json | 3285 | `aa0886cb0c4ee3578163b732ce9c758d2bdcf64c53890155a8a5b055de9f9a78` |
| rule-evidence.md | 1544228 | `3713ce4a993e2ac2dcf164783be39e8bf23c2271f6beed12f67c8c802e9baac9` |

Runs `vendure-local-definitions-final-1` and `vendure-local-definitions-final-2` were byte-identical across all four artifacts. [Baseline](vendure-cart-readiness.local-definitions.baseline.json), [score](vendure-cart-readiness.local-definitions.score.json) and [audit result](vendure-cart-readiness.local-definitions.audit.json) retain the revisions, sizes, hashes and audit counts. The [portable audit helper](../../scripts/audit-local-definitions.cjs) records TypeScript 6.0.3 and runs without target import resolution or execution. Negative controls rejected a changed operator, display and removed Gap edge; source-canary text did not appear in diagnostics. Both final audit JSON files were byte-identical.

The review covers every new initializer citation and typed expression shape, plus the specified positive and negative source sites. It does not execute cases, prove full domain semantics, exhaustively assess unrelated graph facts, establish general secret-detection completeness, or stress the configured maximum bounds. Captured-source receipt integrity is a separate integration-test contract; retrospective source matching in this ordinary extraction benchmark is not an attestation.
