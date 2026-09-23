---
"cartograph": patch
---

Preflight and recovery now skip everything a repository's own `.gitignore` files exclude. Build output, emitted bundles and vendored code (`build/`, `out/`, `vendor/` and similar) are no longer ingested as source. Every language, Terraform, plugins and Preflight now read the same file set.
