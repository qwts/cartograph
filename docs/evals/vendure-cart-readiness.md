# Vendure benchmark — cart readiness to ArrangingPayment

**Status: Independently source-reviewed baseline oracle frozen before extraction.**
Scope: the H2 domain-truth gate in [SPEC-01](../SPEC-01_context-hub.md), traced to
[AC-0106](../user_stories.md#us-0021--inspect-and-curate-a-persistent-business-domain-context).
The frozen [expected manifest and decision-case matrix](vendure-cart-readiness.expected.json)
contains five core entry guards, three supporting stock rules, 41 decision cases,
and ten negative claims. Its SHA-256 is
`dcd9833e8f9be2269a6711f8bd04119c83931f6de45e04d17d1b99da9d51295d`.
Benchmark acceptance cases have not been executed. Source review establishes
expectations; it does not establish Cartograph recovery coverage or a pass.

The [first measured source-observation baseline](vendure-cart-readiness.baseline.md)
records 0/8 complete matches, six relevant local exit anchors and deterministic
outputs. Decision cases remain unexecuted; H2 remains incomplete.

Codex context_tests performed the second source review on 2026-09-10 against the
clean pinned checkout, before accessing or generating any recovered benchmark
output. This is **agent review, not human approval**. The reviewer contributed
generic Cartograph implementation work; the independence claim concerns source
review before benchmark output, not blindness to the implementation. The manifest
records that limitation, resolved refinements, and no unresolved disagreements.

## Target and configuration

- Repository: `https://github.com/vendurehq/vendure` (canonical upstream).
- Release: [v3.7.3][release], published 2026-09-02.
- Immutable commit: [`2cc309deb5394af176314552ac7320a7b26a0104`][commit].
- Annotated tag object: `43d0aa165ac0d28b5a8d57fe4139ce2435497648`, verified through
  GitHub to point to that commit. Runs must pin the commit, not resolve the tag anew.
- Initial state: an existing order in `AddingItems`; requested state:
  `ArrangingPayment`. Use built-in configuration unless a case declares an override.
- Exclude admin Draft orders, custom process behavior, storefront rendering,
  payment authorization/settlement, and fulfillment from the first coverage denominator.

The expected domain hub answers why a cart can enter payment arrangement, which
rules can prevent it, what configuration changes those rules, and which runtime
inputs remain unknown. Its title and membership need their own producing tiers;
plausible business wording cannot establish a confirmed predicate.

The enforcing path is [ShopOrderResolver.transitionOrderToState][resolver] →
[OrderService.transitionToState][service] → [OrderStateMachine.transition][machine]
→ configured transition guards → delegated stock services where needed →
transition or rejection. Preserve unsupported dependency hops as explicit gaps.

## Frozen expected rules

Each row is one rule. A matching function name or call edge alone does not recover
the rule: the predicate, scope, configuration condition, and consequence must agree.

| ID | Expected rule | Enforcing evidence |
|---|---|---|
| VEN-READY-01 | Unless `checkAllVariantsExist` is false, leaving `AddingItems` for a non-`Cancelled` state with lines requires every distinct referenced variant and its parent product to be undeleted. A count mismatch rejects the transition. | [default-order-process.ts L298–315][variants] |
| VEN-READY-02 | Entry to `ArrangingPayment` rejects zero order lines unless `arrangingPaymentRequiresContents` is false. | [L317–320][contents] |
| VEN-READY-03 | Entry requires an associated customer unless `arrangingPaymentRequiresCustomer` is false. | [L321–323][customer] |
| VEN-READY-04 | Entry requires a defined, nonempty `shippingLines` array unless `arrangingPaymentRequiresShipping` is false. | [L324–329][shipping] |
| VEN-READY-05 | Unless `arrangingPaymentRequiresStock` is false, each line's quantity is compared with its variant's saleable stock. Any quantity greater than that stock rejects entry and identifies affected variants; equality passes this comparison. | [L330–350][stock-guard] |
| VEN-READY-06 | Saleable stock is `Number.MAX_SAFE_INTEGER` when variant inventory tracking is `FALSE`, or is `INHERIT` while global tracking is false. | [product-variant.service.ts L323–331][untracked] |
| VEN-READY-07 | Otherwise saleable stock is on-hand stock minus allocated stock minus the effective out-of-stock threshold. The threshold is global when `useGlobalOutOfStockThreshold` is true, otherwise variant-specific. | [L332–340][saleable] |
| VEN-READY-08 | With the default multi-channel stock strategy, available on-hand and allocated stock are summed only from locations applicable to the active channel. | [multi-channel-stock-location-strategy.ts L82–100][channel-stock] |

Report the first five entry guards separately from the three supporting stock
rules. All eight have deterministic source evidence; this oracle does not imply
the current adapters can recover them.

## Configuration and negative claims

The default configuration selects `[defaultOrderProcess]`; that process is
`configureDefaultOrderProcess({})`. The guards test `!== false`, so omitted options
retain their checks. Processes can change transitions or add rejection hooks;
do not generalize this expected set to an unknown deployment. [Construction][defaults],
[configuration][order-config], [composition][machine].

Vendure v3.7.3 uses `MultiChannelStockLocationStrategy` by default, rather than the
older `DefaultStockLocationStrategy`. An overridden strategy changes the stock
interpretation and requires a separately reviewed expected set. [Configuration][catalog-config].

The reviewed dependency closure includes the finite-state machine's consumption
of string rejections before state assignment, the service's typed transition
error, variant-specific stock loading, and cached active-channel membership.
The manifest records exact UTF-8 byte ranges, file and span hashes, and source
dependencies; line numbers are navigation aids. Locale catalogs and translated
message text are not semantic matching criteria.

- Guards return the first rejection in source order: unavailable variants, empty
  order, missing customer, missing shipping, then insufficient stock. A domain
  view may explain every rule; do not claim Vendure reports all failures together.
- Customer association does not prove authentication, verified email, or complete
  customer details. Shipping-line presence does not prove address validity or
  current shipping eligibility. These stronger claims are outside these guards.
- The stock guard compares per-line quantities; do not replace this with an
  aggregate same-variant quantity check when describing the recovered code.
- Entering `ArrangingPayment` does not itself place the order or allocate stock
  under default strategies. Both trigger on leaving it for `PaymentAuthorized`
  or `PaymentSettled`. [Placement][placement], [allocation][allocation].
- Source can confirm implemented predicates, not a running cart's customer,
  deletion state, stock values, or configuration. Unobserved values remain unknown.
- This slice does not prove complete checkout recovery or arbitrary-repository support.
- Inventory bypass returns the finite `Number.MAX_SAFE_INTEGER`; tracked saleable
  stock is not clamped to zero. Schema defaults do not prove persisted values.
- The variant-existence rule is the specific distinct-ID query/count predicate.
  Its `LEFT JOIN` and deletion-null check are not a separate assertion that a
  parent row exists or that a variant is enabled or published.
- A guard permit does not guarantee transaction success: dependency reads,
  persistence, event publishing, and finalization can still fail.

## Independent expected-set procedure

Steps 1–3 are complete for this baseline. All cases remain `not_executed`; later
scoring must record results separately. The reviewed upstream tests corroborate
behavior but use test configuration: notably, the stock suite selects a custom
`TestOrderPlacedStrategy`. They are neither default-configuration execution
evidence nor extraction input. Cases with synthetic local-guard inputs, such as
repeated same-variant lines or numeric limits, explicitly avoid claiming Shop API
reachability.

1. Before seeing Cartograph output, a reviewer inspects the pinned enforcement
   code and dependency closure. Freeze a manifest containing the eight IDs,
   predicates, scope, configuration gates, effects, source spans, and dependencies.
2. A second reviewer checks that manifest independently. Read upstream tests as
   corroboration: [missing customer][customer-test], [shipping and success][shipping-test],
   and [stock consumed by another customer][stock-test]. Reading tests is not
   execution evidence. Record disagreements and resolve them before scoring.
3. Freeze a decision-case matrix: valid baseline; each core-guard violation;
   undefined versus empty shipping lines; deleted variant versus deleted parent;
   stock equality versus excess; inventory FALSE and both INHERIT branches;
   global versus local threshold; off-channel stock; each explicit-false override;
   and multiple violations to establish rejection precedence. Include the negative
   claims above and the `Cancelled` exception to the variant-existence guard.
4. Keep expected labels and upstream tests outside Cartograph's extraction input.
   Extract production source with the declared configuration. Never generate the
   oracle from the recovered rules, generated prose, or extractor implementation.
5. Match output against the frozen manifest. Duplicates cannot increase recall.
   Review additional assertions against source rather than assuming every extra
   assertion is false. Record missing, incorrect, inferred, and unsupported results.
6. Retain target and Cartograph commits, configuration, expected-manifest checksum,
   output hashes, case decisions, reviewer identities, and unresolved disagreements.

## Proposed acceptance gates

These are targets, not measured results. The independently reviewed manifest and
case matrix must be frozen before an acceptance run.

| Measure | Proposed gate |
|---|---|
| Confirmed rule coverage | 5/5 entry guards and 3/3 supporting rules, also reported as 8/8. Gaps, generic call edges, and inferred proposals do not count as confirmed matches. |
| Confirmed rule precision | Every confirmed assertion has the correct predicate, scope, configuration, and consequence; zero unsupported confirmed assertions. Report reviewed extras separately. |
| Evidence completeness | All accepted rules cite the pinned commit, exact source spans, extractor/tier, and linked dependencies. Missing dependency evidence remains visible. |
| Inference accounting | Report confirmed matches, inferred proposals, and unresolved gaps separately. Accepting a proposal never upgrades its producing tier. |
| Decision fidelity | Every independently frozen case agrees with the recovered model, including boundaries, overrides, and rejection precedence. Record passed/total cases. |
| Domain usability | One cart-readiness hub connects the transition, eight rules, concepts, evidence, and unknown runtime inputs; a reviewer can explain each rejection and override. |
| Determinism | Two identical-input runs produce identical rule identities and content hashes. |

Do not substitute statement precision for rule coverage or infer coverage from
artifact existence. A successful result proves only this pinned, configured slice.

[release]: https://github.com/vendurehq/vendure/releases/tag/v3.7.3
[commit]: https://github.com/vendurehq/vendure/commit/2cc309deb5394af176314552ac7320a7b26a0104
[resolver]: https://github.com/vendurehq/vendure/blob/2cc309deb5394af176314552ac7320a7b26a0104/packages/core/src/api/resolvers/shop/shop-order.resolver.ts#L315-L329
[service]: https://github.com/vendurehq/vendure/blob/2cc309deb5394af176314552ac7320a7b26a0104/packages/core/src/service/services/order.service.ts#L1312-L1341
[machine]: https://github.com/vendurehq/vendure/blob/2cc309deb5394af176314552ac7320a7b26a0104/packages/core/src/service/helpers/order-state-machine/order-state-machine.ts#L38-L75
[variants]: https://github.com/vendurehq/vendure/blob/2cc309deb5394af176314552ac7320a7b26a0104/packages/core/src/config/order/default-order-process.ts#L298-L315
[contents]: https://github.com/vendurehq/vendure/blob/2cc309deb5394af176314552ac7320a7b26a0104/packages/core/src/config/order/default-order-process.ts#L317-L320
[customer]: https://github.com/vendurehq/vendure/blob/2cc309deb5394af176314552ac7320a7b26a0104/packages/core/src/config/order/default-order-process.ts#L321-L323
[shipping]: https://github.com/vendurehq/vendure/blob/2cc309deb5394af176314552ac7320a7b26a0104/packages/core/src/config/order/default-order-process.ts#L324-L329
[stock-guard]: https://github.com/vendurehq/vendure/blob/2cc309deb5394af176314552ac7320a7b26a0104/packages/core/src/config/order/default-order-process.ts#L330-L350
[untracked]: https://github.com/vendurehq/vendure/blob/2cc309deb5394af176314552ac7320a7b26a0104/packages/core/src/service/services/product-variant.service.ts#L323-L331
[saleable]: https://github.com/vendurehq/vendure/blob/2cc309deb5394af176314552ac7320a7b26a0104/packages/core/src/service/services/product-variant.service.ts#L332-L340
[channel-stock]: https://github.com/vendurehq/vendure/blob/2cc309deb5394af176314552ac7320a7b26a0104/packages/core/src/config/catalog/multi-channel-stock-location-strategy.ts#L82-L100
[defaults]: https://github.com/vendurehq/vendure/blob/2cc309deb5394af176314552ac7320a7b26a0104/packages/core/src/config/order/default-order-process.ts#L465-L475
[order-config]: https://github.com/vendurehq/vendure/blob/2cc309deb5394af176314552ac7320a7b26a0104/packages/core/src/config/default-config.ts#L176-L192
[catalog-config]: https://github.com/vendurehq/vendure/blob/2cc309deb5394af176314552ac7320a7b26a0104/packages/core/src/config/default-config.ts#L133-L142
[placement]: https://github.com/vendurehq/vendure/blob/2cc309deb5394af176314552ac7320a7b26a0104/packages/core/src/config/order/default-order-placed-strategy.ts#L14-L24
[allocation]: https://github.com/vendurehq/vendure/blob/2cc309deb5394af176314552ac7320a7b26a0104/packages/core/src/config/order/default-stock-allocation-strategy.ts#L14-L24
[customer-test]: https://github.com/vendurehq/vendure/blob/2cc309deb5394af176314552ac7320a7b26a0104/packages/core/e2e/shop-order.e2e-spec.ts#L932-L946
[shipping-test]: https://github.com/vendurehq/vendure/blob/2cc309deb5394af176314552ac7320a7b26a0104/packages/core/e2e/shop-order.e2e-spec.ts#L1125-L1160
[stock-test]: https://github.com/vendurehq/vendure/blob/2cc309deb5394af176314552ac7320a7b26a0104/packages/core/e2e/stock-control.e2e-spec.ts#L1283-L1353
