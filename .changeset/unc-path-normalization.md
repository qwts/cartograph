---
"cartograph": patch
---

Windows path canonicalization no longer leaks `\\?\` extended-length prefixes into stored graph facts: every ingest/evidence canonicalize call now goes through one normalization helper (dunce), so the same commit hashes identically across platforms and evidence lookups compare consistent path forms.
