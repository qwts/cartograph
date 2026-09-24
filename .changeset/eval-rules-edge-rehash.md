---
"cartograph": patch
---

The `DEPENDS_ON` edge to an `eval()` code's source-rule Gap now has its own content hash. Before, it reused the Gap node's hash, so two distinct facts shared one. The edge's hash changes once on the next ingest, then stays stable.
