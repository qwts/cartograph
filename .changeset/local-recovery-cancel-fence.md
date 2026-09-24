---
"cartograph": patch
---

A local recovery cancelled while it reconciles its preflight findings no longer writes them to the register: the reconciled findings are written only once the recovery has completed, as GitHub and system-manifest recoveries already do. A cancelled recovery leaves the pending findings in place.
