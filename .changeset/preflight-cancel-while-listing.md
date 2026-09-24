---
"cartograph": patch
---

Preflight's Cancel now works while the scan is still listing a large repository, not just once it starts checking files. The listing stops at the next directory and records no findings. While it lists, the status line shows "Finding files… N so far" with the folder being read, rather than "Detecting…" with no progress.
