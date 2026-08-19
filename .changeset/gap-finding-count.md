---
"cartograph": patch
---

Unresolved calls are no longer double-counted in the findings register: the gap node carries the finding and the gap CALLS edge is a supporting assertion, mirroring how drift counts nodes only — so "open findings" now matches the number of actual unknowns. Gap edges also carry the same `reason` as their gap node, so no register row renders a bare edge identity in the reason column, and the gap lane no longer lists a phantom "unresolved edge" cause class.
