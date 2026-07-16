---
'cartograph': minor
---

Plugin facts are pinned to the exact artifact that produced them: the host stamps every fact's provenance with `{plugin-id}@{hash}` and the full BLAKE3 artifact hash, overwriting anything the guest wrote — a plugin can never impersonate a built-in extractor, a rebuilt artifact is a different extractor identity, and repeat runs of the same artifact are provably identical.
