---
"cartograph": patch
---

A traced flow is now Partial, not Verified, when a symbol on it runs code T0 could not recover, such as an `eval()` whose argument could not be proven to a literal. The Gap appears as a hop in the flow with its reason. Rule-evidence gaps, which only mark where business-rule evidence stops, still leave a flow Verified.
