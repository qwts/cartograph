---
"cartograph": patch
---

A preflight started right after a recovery shows as done is no longer cancelled or overwritten by that recovery's late write of its reconciled findings. Local, GitHub and system-manifest recoveries now take their place in the preflight order before they report completion.
