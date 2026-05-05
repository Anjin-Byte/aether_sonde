# @aether-sonde/sim

TypeScript adapter for the Aether Sonde discrete-event Ethernet
simulator. Wraps the [`aether-sonde-wasm`](../../crates/wasm) bindings
with exhaustively-typed discriminated unions, ergonomic error classes,
and idiomatic `Symbol.dispose` resource management.

## Install (local development)

The adapter consumes the WASM crate's emitted `pkg/` directory via a
`file:` link. Build the WASM package first, then install:

```sh
# From the workspace root.
wasm-pack build --target bundler --release crates/wasm
npm install --prefix packages/ts-sim
```

## Use

```ts
import {
  TypedTopologyBuilder,
  TypedEngine,
  MAC_CONFIG_IEEE_802_3,
  type Event,
} from "@aether-sonde/sim";

// Build a 2-station HD pair.
using builder = TypedTopologyBuilder.create();
const s1 = builder.addEndStation(1);
const s2 = builder.addEndStation(1);
builder.addHdSegment(10_000_000n, 5_000_000n, s1, 0, s2, 0);
const world = builder.build();

// Run a single transmission.
using engine = TypedEngine.create(world, 0n);
const frame = engine.registerFrame(s1, 512n, false, 10_000_000n);
engine.scheduleTxAttempt(0n, s1, frame);
engine.runUntilIdle();

// Read the typed log.
for (const entry of engine.log()) {
  switch (entry.event.type) {
    case "TxStart":
      console.log("tx-start at", entry.key.time);
      break;
    case "FrontArrive":
      console.log("front-arrive at node", entry.event.node);
      break;
    // ... TypeScript verifies every variant is covered.
  }
}
```

## Test

```sh
npm test --prefix packages/ts-sim
```

Runs `vitest` against the typed adapter API. Tests cover the HD-1
oracle, every `Edit` round-trip, error narrowing for each `EditError`
kind, and same-seed determinism.
