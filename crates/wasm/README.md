# aether-sonde-wasm

WebAssembly bindings for the Aether Sonde simulation core. This crate is
a thin delegation layer over [`aether-sonde`](../core); all simulation
logic lives in the core.

## Build

Requires the Rust `wasm32-unknown-unknown` target and
[`wasm-pack`](https://rustwasm.github.io/wasm-pack/installer/).

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-pack    # one-time

# From the workspace root:
wasm-pack build crates/wasm --target bundler --release
```

The build emits `crates/wasm/pkg/`, an ESM-ready npm package containing
the JS glue, TypeScript declarations, and the `.wasm` binary. The `pkg/`
directory is gitignored (build artifact).

## Test

```sh
wasm-pack test crates/wasm --node
```

Runs the boundary tests under Node — fast, no browser drivers required.
