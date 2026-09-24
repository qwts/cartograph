---
"cartograph": patch
---

On Windows, where Cartograph cannot read physical memory, Auto ingest parallelism is now capped at 4 workers instead of one worker per core. Other platforms keep the one-worker-per-2-GiB memory cap.
