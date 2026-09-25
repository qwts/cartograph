---
"cartograph": patch
---

Preflight and recovery now report the correct Terraform file count when a module is explicitly referenced from an otherwise-gitignored directory — it's counted, matching what extraction actually reads. A `go.mod` the repository's own `.gitignore` excludes is now treated as absent rather than read anyway; internal Go imports that depend on it fall back to an explicit Gap instead of resolving through an ignored file.
