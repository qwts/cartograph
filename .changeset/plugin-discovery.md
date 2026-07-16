---
'cartograph': minor
---

Adapter plugins are now discoverable and switchable per project: Cartograph scans the project's .cartograph/adapters/ and a user-level adapters directory (the project copy wins on id conflict, shadowing stated), keys every artifact by its content hash, and lists them in Settings with a per-project enable/disable that fails closed — a plugin is off until you turn it on, and it only ever extracts facts behind the conformance gate.
