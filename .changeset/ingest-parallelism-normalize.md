---
"cartograph": patch
---

A fixed "Ingest parallelism" worker count saved on a larger machine (or before a CPU quota shrank) is now read back as this machine's maximum, so startup no longer oversubscribes and Settings shows a choice it actually offers. Settings also states that fixed counts stop at 64 workers.
