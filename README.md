# Aether Sonde

[![Docs](https://img.shields.io/badge/docs-latest-blue)](https://anjin-byte.github.io/aether_sonde/aether_sonde/index.html)

A simulator for Ethernet networks. It models how signals travel between
stations, how collisions form on shared wires, and how bridges relay
frames across separate links.

Put differently, I am trying to build out a general simulation of the MAC (Media Access Control) data-link sublayer. This is not a simulation of the physical medium or the signals that propagate through them. I do simulate the latency associated with sending information because it helps explain certain characteristics of data-link design decisions. i.e. CSMA/CD. I am also of the opinion that a good enough simulation of classic ethernet could support later network and transport additions (though I have no plans for anything beyond the conceptual basics)

## Status

Early. The core handles half-duplex, full-duplex, and bridged topologies.
A WebAssembly binding crate (`aether-sonde-wasm`) and a TypeScript
adapter package (`@aether-sonde/sim`) expose the simulator to
JavaScript and TypeScript consumers.

## Build and test

```
cargo build
cargo test
```

For the WASM bindings, see [`crates/wasm/README.md`](crates/wasm/README.md).
For the typed TypeScript API, see [`packages/ts-sim/README.md`](packages/ts-sim/README.md).

## License

Copyright 2026 Aether Sonde contributors.

Licensed under either of

- Apache License, Version 2.0
  ([LICENSE-APACHE](LICENSE-APACHE) or <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license
  ([LICENSE-MIT](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
