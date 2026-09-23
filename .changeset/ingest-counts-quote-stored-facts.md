---
"cartograph": patch
---

Ingest summaries now quote the distinct facts stored in the graph rather than raw extraction occurrences, so the summary agrees with the Workspace graph counts. Occurrences merged into one fact (one relation cited from several call or import sites, or declarations sharing one id) are stated beside the totals instead of disappearing silently.
