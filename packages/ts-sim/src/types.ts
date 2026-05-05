// Discriminated unions and structural types mirroring the Rust core's
// public surface. The serde tag layout is:
//
//   - `type` discriminator: Edit, Event
//   - `kind` discriminator: EditError, BuildError, EngineError, BackoffPolicyError, JamPolicyError, SignalError
//
// `bigint` is used for any u64 the Rust core exposes (BitTime in
// picoseconds, Bits, BitRate). `number` is used for u32 IDs.

// ---------------------------------------------------------------------------
// Phase / EventKey / Signal / Resource ids
// ---------------------------------------------------------------------------

export type Phase = "Release" | "Assertion" | "Reaction" | "LocalDecision";

export interface EventKey {
  time: bigint;
  phase: Phase;
  serial_id: bigint;
}

export type SignalKind = "Frame" | "Jam";

export interface Signal {
  source: number;
  t0: bigint;
  duration: bigint;
  kind: SignalKind;
}

export type SignalLostReason =
  | "SegmentRemoved"
  | "PortDisconnected"
  | "NodeRemoved";

export type SegmentKind = "Hd" | "Fd";

export type Direction = "AtoB" | "BtoA";

export interface Endpoint {
  node: number;
  port: number;
}

// ---------------------------------------------------------------------------
// Node kinds (Rust externally-tagged enum: tuple variants → { Variant: data })
// ---------------------------------------------------------------------------

/** Static configuration for a learning switch. Mirrors `aether_sonde::topology::SwitchData`. */
export interface SwitchData {
  decode_threshold: bigint;
  processing_delay: bigint;
  mac_table_capacity: number;
  aging_threshold: bigint;
}

// EndStationData is a unit struct → null
export type NodeKind =
  | { EndStation: Record<string, never> }
  | { Repeater: { delta_h: bigint } }
  | { Bridge: { decode_threshold: bigint; processing_delay: bigint } }
  | { Switch: SwitchData };

// ---------------------------------------------------------------------------
// MAC / Backoff / Jam / IFG policies
// ---------------------------------------------------------------------------

export interface BackoffPolicy {
  attempt_limit: number;
  backoff_limit: number;
}

export interface JamPolicy {
  bits: bigint;
}

export interface IfgPolicy {
  bits: bigint;
}

export interface MacConfig {
  backoff: BackoffPolicy;
  jam: JamPolicy;
  ifg: IfgPolicy;
}

/** Canonical IEEE 802.3 MAC configuration. Mirrors `MacConfig::IEEE_802_3`. */
export const MAC_CONFIG_IEEE_802_3: MacConfig = {
  backoff: { attempt_limit: 16, backoff_limit: 10 },
  jam: { bits: 32n },
  ifg: { bits: 96n },
};

// ---------------------------------------------------------------------------
// Edit (serde tag = "type")
// ---------------------------------------------------------------------------

export type Edit =
  | { type: "AddEndStation"; port_count: number }
  | { type: "AddRepeater"; port_count: number; delta_h: bigint }
  | {
      type: "AddBridge";
      port_count: number;
      decode_threshold: bigint;
      processing_delay: bigint;
    }
  | {
      type: "AddSwitch";
      port_count: number;
      data: SwitchData;
    }
  | {
      type: "AddHdSegment";
      rate: bigint;
      delay: bigint;
      a: Endpoint;
      b: Endpoint;
    }
  | {
      type: "AddFdSegment";
      rate: bigint;
      delay: bigint;
      a: Endpoint;
      b: Endpoint;
    }
  | { type: "RemoveSegment"; segment: number }
  | { type: "RemoveNode"; node: number }
  | { type: "DisconnectPort"; node: number; port: number }
  | { type: "SetSegmentDelay"; segment: number; new_delay: bigint }
  | { type: "SetSegmentRate"; segment: number; new_rate: bigint }
  | { type: "SetMacConfig"; node: number; config: MacConfig };

// ---------------------------------------------------------------------------
// Event (serde tag = "type")
// ---------------------------------------------------------------------------

export type Event =
  | { type: "TxAttempt"; node: number; frame: number }
  | { type: "TxStart"; node: number; signal: Signal }
  | { type: "TxEnd"; node: number; signal: Signal }
  | { type: "FrontArrive"; node: number; port: number; signal: Signal }
  | { type: "BackArrive"; node: number; port: number; signal: Signal }
  | { type: "CollisionDetect"; node: number; signal: Signal }
  | { type: "JamStart"; node: number }
  | { type: "JamEnd"; node: number }
  | {
      type: "FrameEligible";
      bridge: number;
      port: number;
      frame: number;
    }
  | { type: "Enqueue"; serializer: number; frame: number }
  | { type: "Dequeue"; serializer: number; frame: number }
  | { type: "BackoffExpire"; node: number; attempt: number }
  | { type: "SegmentAdded"; segment: number; kind: SegmentKind }
  | { type: "SegmentRemoved"; segment: number }
  | { type: "NodeAdded"; node: number }
  | { type: "NodeRemoved"; node: number }
  | {
      type: "SegmentDelayChanged";
      segment: number;
      old: bigint;
      new: bigint;
    }
  | {
      type: "SegmentRateChanged";
      segment: number;
      old: bigint;
      new: bigint;
    }
  | { type: "MacConfigChanged"; node: number }
  | {
      type: "PortDisconnected";
      node: number;
      port: number;
      segment: number;
    }
  | { type: "SignalLost"; signal: Signal; reason: SignalLostReason }
  | { type: "DeviceCommandApplied"; node: number }
  | { type: "AgingTick"; node: number };

// ---------------------------------------------------------------------------
// LoggedEvent / Log
// ---------------------------------------------------------------------------

export interface LoggedEvent {
  key: EventKey;
  event: Event;
}

/**
 * Snapshot of an engine's event log. Returned by `TypedEngine.log()`.
 * Internal shape mirrors the Rust `Log` struct's serde representation.
 */
export interface LogSnapshot {
  entries: readonly LoggedEvent[];
}

// ---------------------------------------------------------------------------
// Errors (serde tag = "kind")
// ---------------------------------------------------------------------------

export type EditError =
  | { kind: "NotYetImplemented"; edit_kind: string }
  | { kind: "UnknownNode"; node: number }
  | { kind: "UnknownSegment"; segment: number }
  | { kind: "UnknownPort"; node: number; port: number }
  | { kind: "WouldViolateA7"; component_root: number }
  | { kind: "InvalidEdit"; reason: string };

export type BuildError =
  | { kind: "UnknownNode"; node: number }
  | { kind: "UnknownPort"; node: number; port: number }
  | {
      kind: "PortAlreadyConnected";
      node: number;
      port: number;
      existing_segment: number;
    }
  | { kind: "EndpointsOnSameNode"; node: number }
  | { kind: "ZeroDelay" }
  | { kind: "UniquePathViolated"; component_root: number }
  | { kind: "InvalidConfig"; reason: string };

export interface EngineError {
  kind: "ZeroBitFrame";
}

// ---------------------------------------------------------------------------
// Error wrapper classes — preserve `instanceof` and stack traces
// ---------------------------------------------------------------------------

/** Thrown by `TypedEngine.applyEdit` on validation failure. */
export class EditErrorE extends Error {
  override readonly name = "EditErrorE";
  constructor(readonly inner: EditError) {
    super(`EditError: ${inner.kind}`);
  }
}

/** Thrown by `TypedTopologyBuilder.addHdSegment` / `addFdSegment` / `build`. */
export class BuildErrorE extends Error {
  override readonly name = "BuildErrorE";
  constructor(readonly inner: BuildError) {
    super(`BuildError: ${inner.kind}`);
  }
}

/** Thrown by `TypedEngine.registerFrame`. */
export class EngineErrorE extends Error {
  override readonly name = "EngineErrorE";
  constructor(readonly inner: EngineError) {
    super(`EngineError: ${inner.kind}`);
  }
}

// ---------------------------------------------------------------------------
// Frame structure (Round 4)
// ---------------------------------------------------------------------------

/** IEEE 802 MAC address — six octets. Mirrors `aether_sonde::frame::MacAddress`. */
export type MacAddress = readonly [
  number,
  number,
  number,
  number,
  number,
  number,
];

/** IEEE 802.3 EtherType / length field. */
export type EtherType = number;

/** 802.1Q VLAN tag. */
export interface VlanTag {
  priority: number;
  drop_eligible: boolean;
  vid: number;
}

/** Frame payload (sealed; round 4 has only `Opaque`). */
export interface FramePayload {
  type: "Opaque";
  bits: bigint;
}

/** Ethernet II / 802.3 / 802.1Q frame at MAC-layer granularity. */
export interface Frame {
  destination: MacAddress;
  source: MacAddress;
  ethertype: EtherType;
  vlan: VlanTag | null;
  payload: FramePayload;
}

// ---------------------------------------------------------------------------
// Device snapshots (Round 4 — `Engine::deviceSnapshot`)
// ---------------------------------------------------------------------------

/** How a forwarding-table entry entered the table. */
export type MacEntryOrigin = "Learned" | "ManualInsert";

/** One entry in a learning device's forwarding table. */
export interface MacTableEntry {
  mac: MacAddress;
  port: number;
  learned_at: bigint;
  origin: MacEntryOrigin;
}

export interface EndStationSnapshot {
  port_count: number;
}

export interface RepeaterSnapshot {
  port_count: number;
  delta_h: bigint;
}

export interface BridgeSnapshot {
  decode_threshold_bits: bigint;
  processing_delay: bigint;
  egress_queue_depth: readonly (readonly [number, number])[];
  egress_busy: readonly (readonly [number, boolean])[];
}

export interface SwitchSnapshot {
  decode_threshold_bits: bigint;
  processing_delay: bigint;
  aging_threshold: bigint;
  mac_table: readonly MacTableEntry[];
  egress_queue_depth: readonly (readonly [number, number])[];
  egress_busy: readonly (readonly [number, boolean])[];
}

/**
 * Typed snapshot of a node's link-layer device state. Returned by
 * `TypedEngine.deviceSnapshot`. Discriminated by `type`.
 */
export type DeviceSnapshot =
  | ({ type: "EndStation" } & EndStationSnapshot)
  | ({ type: "Repeater" } & RepeaterSnapshot)
  | ({ type: "Bridge" } & BridgeSnapshot)
  | ({ type: "Switch" } & SwitchSnapshot);

// ---------------------------------------------------------------------------
// Device commands (Round 4 — `Engine::applyDeviceCommand`)
// ---------------------------------------------------------------------------

/**
 * Mid-simulation mutation of a device's internal state. Distinct
 * from `Edit`, which mutates topology. Discriminated by `type`.
 */
export type DeviceCommand =
  | {
      type: "InsertMacEntry";
      node: number;
      mac: MacAddress;
      port: number;
    }
  | { type: "RemoveMacEntry"; node: number; mac: MacAddress }
  | { type: "FlushMacTable"; node: number }
  | {
      type: "SetSwitchAgingThreshold";
      node: number;
      threshold: bigint;
    };

/** Errors returned by `TypedEngine.applyDeviceCommand`. */
export type DeviceCommandError =
  | { kind: "UnknownNode"; node: number }
  | { kind: "NotApplicable"; reason: string }
  | { kind: "InvalidArgument"; reason: string };

/** Thrown by `TypedEngine.applyDeviceCommand` on validation failure. */
export class DeviceCommandErrorE extends Error {
  override readonly name = "DeviceCommandErrorE";
  constructor(readonly inner: DeviceCommandError) {
    super(`DeviceCommandError: ${inner.kind}`);
  }
}
