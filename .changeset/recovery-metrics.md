---
'cartograph': minor
---

Recovery metrics: every completed ingest now persists a history record
— tier tallies counted with the register's own provenance definition,
unsupported/no-evidence counts, per-extractor coverage, and an
order-independent whole-graph content hash — so re-ingesting the same
commit shows identical hashes in queryable history and the determinism
invariant becomes observable data. New ingest_history and
extractor_coverage commands feed the Provenance & Eval surface.
