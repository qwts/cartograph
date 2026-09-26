---
"cartograph": patch
---

Java overloaded methods and constructors now get distinct symbol identity (a parameter-signature suffix) instead of collapsing onto one fact, and calls resolve to the matching overload by argument count — scoped to the owning type's fully qualified (package-prefixed) name so two packages' same-named types never share an overload set — failing closed to an explicit Gap when arity alone can't decide. Existing graphs re-key these facts on their next re-ingest (`GRAPH_SCHEMA_VERSION` 5 → 6, sequenced after #527's 4 → 5 bump).
