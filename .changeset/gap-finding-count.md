---
"cartograph": patch
---

Unresolved calls are no longer double-counted in the findings register: a gap edge that touches its gap node supports that node's finding (one finding per unresolved call, mirroring how drift counts), while an edge-only gap with no gap node on either end stays a finding of its own. The headline count, the Spec Workbench gap chip, and the gap lane all reconcile through one shared definition. Gap edges also carry the same `reason` as their gap node, so no register row renders a bare edge identity in the reason column.
