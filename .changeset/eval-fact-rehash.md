---
"cartograph": patch
---

Facts recovered from `eval()` / `new Function()` code now get a unique content hash per eval site. Before, the same code evaluated at two sites produced distinct CALLS edges that shared one hash. Each recovered fact's hash now comes from its final site-scoped identity. Existing eval-recovered facts get new hashes once on the next ingest, then stay stable.
