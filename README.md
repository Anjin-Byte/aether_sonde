# Aether Sonde

[![Docs](https://img.shields.io/badge/docs-latest-blue)](https://anjin-byte.github.io/aether_sonde/aether_sonde/index.html)

A simulator for Ethernet networks. It models how signals travel between
stations, how collisions form on shared wires, and how bridges relay
frames across separate links.

## Status

Early. The core handles half-duplex, full-duplex, and bridged topologies.
The Rust crate is the engine; a WebAssembly bridge for use from TypeScript
is the planned next step.

## Build and test

```
cargo build
cargo test
```

## More

Design notes and the math behind the simulator live in `design/`.

License: MIT OR Apache-2.0.
