---
"cartograph": patch
---

Fixed: in a multi-repo workspace, a repo's recorded ingest-history content hash no longer changes when a different repo in the same workspace changes. The recorded hash now scopes to the ingested registration's own facts (a whole-system ingest still hashes every repo it loaded), matching the "same commit under the same source registration" determinism guarantee.
