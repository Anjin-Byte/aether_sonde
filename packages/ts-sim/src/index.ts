// Public API of @aether-sonde/sim — the TypeScript adapter for the
// Aether Sonde discrete-event Ethernet simulator.
//
// Three opaque-handle classes (`TypedTopologyBuilder`, `TypedWorld`,
// `TypedEngine`), three observable functions, three error wrapper
// classes, and a complete set of discriminated-union types for the
// boundary payloads.
//
// All handle classes implement `Disposable` so consumers can use
// `using` blocks for explicit resource cleanup.

export { init } from "aether-sonde-wasm";

export { TypedTopologyBuilder, TypedWorld } from "./builder.js";

export { TypedEngine } from "./engine.js";

export {
  carrierSense,
  collisionDetect,
  firstCollisionDetectAt,
} from "./observe.js";

export {
  // Discriminated unions
  type Edit,
  type Event,
  type EditError,
  type BuildError,
  type EngineError,
  type Phase,
  type Signal,
  type SignalKind,
  type SignalLostReason,
  type SegmentKind,
  type Direction,
  type NodeKind,
  // Structural types
  type Endpoint,
  type EventKey,
  type LoggedEvent,
  type LogSnapshot,
  type MacConfig,
  type BackoffPolicy,
  type JamPolicy,
  type IfgPolicy,
  // Constants
  MAC_CONFIG_IEEE_802_3,
  // Error wrapper classes
  EditErrorE,
  BuildErrorE,
  EngineErrorE,
} from "./types.js";
