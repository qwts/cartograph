# Test fixtures

Each subdirectory is a standalone guest crate (its own detached `[workspace]`,
outside the main Cargo workspace so `cargo test --workspace` never tries to
build it for the host target) compiled to `wasm32-wasip2` and checked into
`compiled/*.wasm` as small prebuilt binaries — `adapters-plugin-host`'s test
suite runs against those directly with no wasm toolchain required.

| Fixture | Proves |
|---|---|
| `ok-adapter` | Host <-> guest round trip: valid source in, `Extraction` out |
| `busy-loop` | Fuel exhaustion and epoch-deadline enforcement (never returns) |
| `memory-hog` | Memory-cap enforcement (grows memory without bound) |
| `net-probe` | No ambient network capability (attempts a TCP connect) |

To regenerate after changing `wit/adapter.wit` or a fixture's `src/lib.rs`:

```sh
rustup target add wasm32-wasip2  # once
for f in ok-adapter busy-loop memory-hog net-probe; do
  (cd "$f" && cargo build --target wasm32-wasip2 --release)
  cp "$f/target/wasm32-wasip2/release/$(echo "$f" | tr - _).wasm" "compiled/$f.wasm"
done
```
