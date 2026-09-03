---
"cartograph": patch
---

The WASM plugin host now runs on Wasmtime 48.0.1 (`wasmtime` and
`wasmtime-wasi` bumped together from 47.0.3), which carries the upstream
fixes for RUSTSEC-2026-0268 (guest-controlled host heap allocation via
WASIp3 streams) and RUSTSEC-2026-0269 (filesystem sandbox escape with
trailing slashes in paths/symlinks). Third-party notices were regenerated
for the moved crates.
