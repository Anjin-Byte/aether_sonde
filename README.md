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
