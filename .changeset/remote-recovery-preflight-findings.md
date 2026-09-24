---
"cartograph": patch
---

GitHub and system-manifest recoveries now write their preflight findings to the register, as local recoveries already did. Each recovered repo's dynamic `eval()` / `new Function()` sites, uncovered languages and other unsupported constructs show up as Unsupported findings, reconciled with that recovery's own proof. A failed or cancelled recovery writes nothing.
